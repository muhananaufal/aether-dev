//! Working out what to run inside a database container, and with what.
//!
//! Credentials are never stored or asked for: they are read from the
//! container's own environment, which the daemon already reports. The tool
//! this replaces kept the same passwords in a tracked .env file and passed
//! them on the command line.
//!
//! Deciding the command is a pure function so the argument shapes, the
//! credential lookup and the refusals are all testable without a database.

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{container} does not publish {variable}, so its credentials are unknown")]
    MissingCredential {
        container: &'static str,
        variable: String,
    },
    #[error("{0:?} is not a usable database name")]
    InvalidDatabase(String),
    #[error("the dump is gzipped but unreadable: {0}")]
    UnreadableDump(String),
    #[error("no dump tool is known for the image {0:?}")]
    UnknownEngine(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    MySql,
    Postgres,
    Mongo,
}

impl Engine {
    /// Reads the engine off the image name, ignoring any registry and tag.
    /// An image that is not a database returns `None` rather than a guess.
    pub fn from_image(image: &str) -> Option<Engine> {
        let name = image
            .rsplit('/')
            .next()
            .unwrap_or(image)
            .split(':')
            .next()
            .unwrap_or_default();
        match name {
            "mysql" | "mariadb" | "percona" => Some(Engine::MySql),
            "postgres" | "postgis" => Some(Engine::Postgres),
            "mongo" => Some(Engine::Mongo),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Engine::MySql => "mysql",
            Engine::Postgres => "postgres",
            Engine::Mongo => "mongo",
        }
    }
}

/// Who to connect to a database as, when the container's own environment is
/// not the answer.
///
/// An official image publishes its credentials in well-known variables and
/// nothing here needs setting. A container built by hand, one that keeps its
/// password somewhere else, or one whose dumps should run as a user other than
/// the superuser, is not unusual enough to be unsupported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    /// The user to connect as, instead of the engine's usual one.
    pub user: Option<String>,
    /// The password itself, for a container that does not carry it.
    pub password: Option<String>,
    /// The variable in the container's environment that holds the password,
    /// when it is not the one the official image uses.
    pub password_env: Option<String>,
}

impl Engine {
    /// The user this engine connects as when nobody says otherwise. MySQL's
    /// superuser is fixed by the image; the other two record theirs in the
    /// environment, where a container created with a different one still
    /// reports the truth.
    fn default_user(self, container_env: &[String]) -> Result<String, DbError> {
        match self {
            Engine::MySql => Ok("root".to_string()),
            Engine::Postgres => self.required(container_env, "POSTGRES_USER"),
            Engine::Mongo => self.required(container_env, "MONGO_INITDB_ROOT_USERNAME"),
        }
    }

    /// The variable the official image keeps the password in.
    fn password_var(self) -> &'static str {
        match self {
            Engine::MySql => "MYSQL_ROOT_PASSWORD",
            Engine::Postgres => "POSTGRES_PASSWORD",
            Engine::Mongo => "MONGO_INITDB_ROOT_PASSWORD",
        }
    }

    fn required(self, container_env: &[String], variable: &str) -> Result<String, DbError> {
        value_of(container_env, variable).ok_or_else(|| DbError::MissingCredential {
            container: self.label(),
            variable: variable.to_string(),
        })
    }
}

impl Account {
    /// Works out the user and password to use, preferring what was configured
    /// and falling back to what the container reports about itself.
    fn resolve(
        &self,
        engine: Engine,
        container_env: &[String],
    ) -> Result<(String, String), DbError> {
        let user = match &self.user {
            Some(user) => user.clone(),
            None => engine.default_user(container_env)?,
        };
        let password = match (&self.password, &self.password_env) {
            (Some(password), _) => password.clone(),
            // Naming a variable means read that one, not that one or the usual
            // one: a silent fallback here would hide a typo behind a password
            // that happens to work.
            (None, Some(variable)) => engine.required(container_env, variable)?,
            (None, None) => engine.required(container_env, engine.password_var())?,
        };
        Ok((user, password))
    }
}

/// A command to run inside a container, and the environment to run it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPlan {
    pub command: Vec<String>,
    pub env: Vec<String>,
}

/// Builds the dump command for one database.
pub fn dump_plan(
    engine: Engine,
    database: &str,
    container_env: &[String],
    account: &Account,
) -> Result<ExecPlan, DbError> {
    let database = validated_database(database)?;
    let (user, password) = account.resolve(engine, container_env)?;

    match engine {
        Engine::MySql => Ok(ExecPlan {
            command: vec![
                "mysqldump".to_string(),
                format!("--user={user}"),
                "--single-transaction".to_string(),
                "--routines".to_string(),
                "--triggers".to_string(),
                // Without this the dump carries this server's GTID state
                // and refuses to load into any other one.
                "--set-gtid-purged=OFF".to_string(),
                database,
            ],
            // mysqldump reads MYSQL_PWD from the environment, so the
            // password never reaches the command line where every other
            // process in the container could read it.
            env: vec![format!("MYSQL_PWD={password}")],
        }),
        Engine::Postgres => Ok(ExecPlan {
            command: vec![
                "pg_dump".to_string(),
                format!("--username={user}"),
                // Ownership and grants belong to the machine that made the
                // dump, not to whoever restores it.
                "--no-owner".to_string(),
                "--no-privileges".to_string(),
                database,
            ],
            env: vec![format!("PGPASSWORD={password}")],
        }),
        Engine::Mongo => Ok(ExecPlan {
            command: vec![
                "mongodump".to_string(),
                "--archive".to_string(),
                format!("--db={database}"),
                format!("--username={user}"),
                // mongodump offers no way to take a password from the
                // environment, so unlike the other two engines it is
                // visible in this container's process list while running.
                format!("--password={password}"),
                "--authenticationDatabase=admin".to_string(),
            ],
            env: Vec::new(),
        }),
    }
}

/// Returns the dump as it should reach the database, decompressing it when it
/// is gzipped.
///
/// Decided from the content rather than from a file name, because a dump does
/// not stop being gzipped when somebody renames it - and compressed bytes fed
/// to a database fail in a way that does not point back here.
pub fn decode_dump(bytes: Vec<u8>) -> Result<Vec<u8>, DbError> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes);
    }
    let mut plain = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&bytes[..]), &mut plain)
        .map(|_| plain)
        .map_err(|error| DbError::UnreadableDump(error.to_string()))
}

/// Splits on the first equals sign only: a password may well contain more.
fn value_of(env: &[String], key: &str) -> Option<String> {
    env.iter().find_map(|entry| {
        let (name, value) = entry.split_once('=')?;
        (name == key).then(|| value.to_string())
    })
}

/// The name becomes an argument to a command running as root inside the
/// container, so anything that could be read as another argument is refused
/// rather than escaped.
fn validated_database(database: &str) -> Result<String, DbError> {
    let usable = !database.is_empty()
        && !database.starts_with('-')
        && database
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !usable {
        return Err(DbError::InvalidDatabase(database.to_string()));
    }
    Ok(database.to_string())
}

/// Builds the restore command for one database.
///
/// The database name and the file path travel as their own arguments to `sh`,
/// never spliced into the script text, so neither can be read as part of the
/// command however it is spelled.
pub fn restore_plan(
    engine: Engine,
    database: &str,
    file: &str,
    container_env: &[String],
    account: &Account,
) -> Result<ExecPlan, DbError> {
    let database = validated_database(database)?;
    let (user, password) = account.resolve(engine, container_env)?;

    let (script, mut args, env) = match engine {
        Engine::MySql => (
            "mysql --user=\"$1\" \"$2\" < \"$3\"",
            vec![user, database],
            vec![format!("MYSQL_PWD={password}")],
        ),
        Engine::Postgres => (
            "psql --username=\"$1\" --dbname=\"$2\" --file=\"$3\" --set ON_ERROR_STOP=1",
            vec![user, database],
            vec![format!("PGPASSWORD={password}")],
        ),
        Engine::Mongo => (
            "mongorestore --username=\"$1\" --password=\"$2\" \
             --authenticationDatabase=admin --db=\"$3\" --archive=\"$4\"",
            vec![user, password, database],
            Vec::new(),
        ),
    };

    // "sh" fills $0; the real arguments start at $1.
    let mut command = vec![
        "sh".to_string(),
        "-c".to_string(),
        script.to_string(),
        "sh".to_string(),
    ];
    command.append(&mut args);
    command.push(file.to_string());
    Ok(ExecPlan { command, env })
}

/// Builds a dump of every database on a server, which is what a backup wants:
/// dumping one at a time would miss whatever was created since the list was
/// last looked at.
pub fn dump_all_plan(
    engine: Engine,
    container_env: &[String],
    account: &Account,
) -> Result<ExecPlan, DbError> {
    let (user, password) = account.resolve(engine, container_env)?;

    match engine {
        Engine::MySql => Ok(ExecPlan {
            command: vec![
                "mysqldump".to_string(),
                format!("--user={user}"),
                "--all-databases".to_string(),
                "--single-transaction".to_string(),
                "--routines".to_string(),
                "--triggers".to_string(),
                "--events".to_string(),
                "--set-gtid-purged=OFF".to_string(),
            ],
            env: vec![format!("MYSQL_PWD={password}")],
        }),
        Engine::Postgres => Ok(ExecPlan {
            // pg_dump takes one database. A backup of the server needs the
            // other tool, which also carries roles and tablespaces.
            command: vec!["pg_dumpall".to_string(), format!("--username={user}")],
            env: vec![format!("PGPASSWORD={password}")],
        }),
        Engine::Mongo => Ok(ExecPlan {
            command: vec![
                "mongodump".to_string(),
                "--archive".to_string(),
                format!("--username={user}"),
                format!("--password={password}"),
                "--authenticationDatabase=admin".to_string(),
            ],
            env: Vec::new(),
        }),
    }
}

/// Names a backup file after the service it came from, with the extension the
/// contents actually have.
pub fn backup_filename(service: &str, engine: Engine, gzip: bool) -> String {
    let extension = match engine {
        Engine::MySql | Engine::Postgres => "sql",
        Engine::Mongo => "archive",
    };
    if gzip {
        format!("{service}.{extension}.gz")
    } else {
        format!("{service}.{extension}")
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn mysql_env() -> Vec<String> {
        vec![
            "PATH=/usr/bin".to_string(),
            "MYSQL_ROOT_PASSWORD=s3cr3t".to_string(),
            "MYSQL_DATABASE=shop".to_string(),
        ]
    }

    fn postgres_env() -> Vec<String> {
        vec![
            "POSTGRES_USER=ticket".to_string(),
            "POSTGRES_PASSWORD=p4ss".to_string(),
        ]
    }

    fn mongo_env() -> Vec<String> {
        vec![
            "MONGO_INITDB_ROOT_USERNAME=admin".to_string(),
            "MONGO_INITDB_ROOT_PASSWORD=m0ngo".to_string(),
        ]
    }

    #[test]
    fn an_engine_is_recognised_from_the_image_it_runs() {
        assert_eq!(Engine::from_image("mysql:9.7"), Some(Engine::MySql));
        assert_eq!(Engine::from_image("mariadb:11"), Some(Engine::MySql));
        assert_eq!(
            Engine::from_image("postgres:16-alpine"),
            Some(Engine::Postgres)
        );
        assert_eq!(Engine::from_image("mongo:8.0"), Some(Engine::Mongo));
    }

    #[test]
    fn an_image_that_holds_no_database_is_not_guessed_at() {
        assert_eq!(Engine::from_image("redis:alpine"), None);
        assert_eq!(Engine::from_image("caddy:alpine"), None);
        assert_eq!(Engine::from_image("alpine/socat"), None);
        assert_eq!(
            Engine::from_image("docker.elastic.co/elasticsearch/elasticsearch:9.4.4"),
            None
        );
    }

    #[test]
    fn a_registry_prefix_does_not_hide_the_engine() {
        assert_eq!(
            Engine::from_image("docker.io/library/postgres:18"),
            Some(Engine::Postgres)
        );
    }

    /// The script refers to its arguments by position, so the order they are
    /// appended in is the whole contract. `contains` cannot see a swap; this
    /// pins each slot to the placeholder the script reads it through.
    #[test]
    fn every_restore_script_gets_its_arguments_in_the_order_it_reads_them() {
        let cases = [
            (Engine::MySql, mysql_env(), vec!["root", "shop", "/tmp/d"]),
            (
                Engine::Postgres,
                postgres_env(),
                vec!["ticket", "shop", "/tmp/d"],
            ),
            (
                Engine::Mongo,
                mongo_env(),
                vec!["admin", "m0ngo", "shop", "/tmp/d"],
            ),
        ];

        for (engine, env, expected) in cases {
            let plan = restore_plan(engine, "shop", "/tmp/d", &env, &Account::default()).unwrap();
            // "sh" -c <script> "sh" fills $0; the positional arguments follow.
            assert_eq!(&plan.command[0..2], &["sh", "-c"], "{engine:?}");
            assert_eq!(plan.command[3], "sh", "{engine:?} needs a filler for $0");
            assert_eq!(
                &plan.command[4..],
                expected.as_slice(),
                "{engine:?} arguments are out of order"
            );
            let script = &plan.command[2];
            for slot in 1..=expected.len() {
                assert!(
                    script.contains(&format!("${slot}")),
                    "{engine:?} passes an argument its script never reads: ${slot}"
                );
            }
        }
    }

    #[test]
    fn a_gzipped_dump_is_decompressed_and_a_plain_one_is_left_alone() {
        use std::io::Write;
        let sql = b"CREATE TABLE orders (id INT);\n".to_vec();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&sql).unwrap();
        let gzipped = encoder.finish().unwrap();
        assert_ne!(gzipped, sql, "the fixture must actually be compressed");

        assert_eq!(decode_dump(gzipped).unwrap(), sql);
        assert_eq!(
            decode_dump(sql.clone()).unwrap(),
            sql,
            "a plain dump must pass through untouched"
        );
    }

    #[test]
    fn a_file_that_only_looks_gzipped_is_refused_rather_than_sent_on() {
        // The magic bytes with nothing valid behind them: feeding this to a
        // database would fail somewhere far from the cause.
        let err = decode_dump(vec![0x1f, 0x8b, 0x00, 0x00]).unwrap_err();
        assert!(matches!(err, DbError::UnreadableDump(_)), "got {err:?}");
    }

    #[test]
    fn a_configured_user_replaces_the_one_the_engine_would_assume() {
        let account = Account {
            user: Some("backup".to_string()),
            ..Account::default()
        };
        let plan = dump_plan(Engine::MySql, "shop", &mysql_env(), &account).unwrap();
        assert!(
            plan.command.iter().any(|arg| arg == "--user=backup"),
            "root is a default, not a rule; got {:?}",
            plan.command
        );
    }

    #[test]
    fn a_configured_password_is_used_when_the_container_publishes_none() {
        let account = Account {
            password: Some("from-config".to_string()),
            ..Account::default()
        };
        let plan = dump_plan(
            Engine::MySql,
            "shop",
            &["PATH=/usr/bin".to_string()],
            &account,
        )
        .expect("a password in the config is a password");
        assert!(plan.env.contains(&"MYSQL_PWD=from-config".to_string()));
    }

    #[test]
    fn a_configured_password_variable_is_read_instead_of_the_usual_one() {
        let account = Account {
            password_env: Some("DB_ROOT_PW".to_string()),
            ..Account::default()
        };
        let env = vec![
            "MYSQL_ROOT_PASSWORD=wrong".to_string(),
            "DB_ROOT_PW=right".to_string(),
        ];
        let plan = dump_plan(Engine::MySql, "shop", &env, &account).unwrap();
        assert!(
            plan.env.contains(&"MYSQL_PWD=right".to_string()),
            "the named variable wins over the one the image happens to use; got {:?}",
            plan.env
        );
    }

    #[test]
    fn a_missing_credential_names_the_variable_that_was_actually_looked_for() {
        let account = Account {
            password_env: Some("DB_ROOT_PW".to_string()),
            ..Account::default()
        };
        let err = dump_plan(Engine::MySql, "shop", &[], &account).unwrap_err();
        assert!(
            err.to_string().contains("DB_ROOT_PW"),
            "an error that names a variable nobody configured sends the reader to the \
             wrong place; got {err}"
        );
    }

    #[test]
    fn a_configured_account_reaches_restore_and_backup_too() {
        let account = Account {
            user: Some("backup".to_string()),
            password: Some("pw".to_string()),
            ..Account::default()
        };
        let restore = restore_plan(Engine::Postgres, "shop", "/tmp/d.sql", &[], &account).unwrap();
        assert!(restore.command.iter().any(|arg| arg == "backup"));
        assert!(restore.env.contains(&"PGPASSWORD=pw".to_string()));

        let all = dump_all_plan(Engine::Postgres, &[], &account).unwrap();
        assert!(all.command.iter().any(|arg| arg == "--username=backup"));
        assert!(all.env.contains(&"PGPASSWORD=pw".to_string()));
    }

    #[test]
    fn an_empty_account_leaves_every_engine_reading_the_container_as_before() {
        let none = Account::default();
        assert!(dump_plan(Engine::MySql, "shop", &mysql_env(), &none).is_ok());
        assert!(dump_plan(Engine::Postgres, "t", &postgres_env(), &none).is_ok());
        assert!(dump_plan(Engine::Mongo, "e", &mongo_env(), &none).is_ok());
    }

    #[test]
    fn a_mysql_dump_names_the_database_and_keeps_the_password_out_of_the_command() {
        let plan = dump_plan(Engine::MySql, "shop", &mysql_env(), &Account::default()).unwrap();
        assert_eq!(plan.command[0], "mysqldump");
        assert!(plan.command.iter().any(|arg| arg == "shop"));
        assert!(
            !plan.command.iter().any(|arg| arg.contains("s3cr3t")),
            "a password in the command line is visible to every process in the container"
        );
        assert!(plan.env.contains(&"MYSQL_PWD=s3cr3t".to_string()));
    }

    #[test]
    fn a_postgres_dump_uses_the_user_the_container_was_created_with() {
        let plan = dump_plan(
            Engine::Postgres,
            "tickets",
            &postgres_env(),
            &Account::default(),
        )
        .unwrap();
        assert_eq!(plan.command[0], "pg_dump");
        assert!(plan.command.iter().any(|arg| arg == "--username=ticket"));
        assert!(plan.command.iter().any(|arg| arg == "tickets"));
        assert!(!plan.command.iter().any(|arg| arg.contains("p4ss")));
        assert!(plan.env.contains(&"PGPASSWORD=p4ss".to_string()));
    }

    #[test]
    fn a_mongo_dump_authenticates_against_the_admin_database() {
        let plan = dump_plan(Engine::Mongo, "events", &mongo_env(), &Account::default()).unwrap();
        assert_eq!(plan.command[0], "mongodump");
        assert!(plan.command.iter().any(|arg| arg == "--db=events"));
        assert!(plan
            .command
            .iter()
            .any(|arg| arg == "--authenticationDatabase=admin"));
        assert!(plan.command.iter().any(|arg| arg == "--username=admin"));
    }

    #[test]
    fn a_container_without_its_credentials_is_reported_rather_than_dumped_blind() {
        let err = dump_plan(
            Engine::MySql,
            "shop",
            &["PATH=/usr/bin".to_string()],
            &Account::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, DbError::MissingCredential { .. }),
            "got {err:?}"
        );
        assert!(
            err.to_string().contains("MYSQL_ROOT_PASSWORD"),
            "the message must name the variable so it can be fixed"
        );
    }

    #[test]
    fn a_database_name_that_could_smuggle_arguments_is_refused() {
        for hostile in ["", "shop; drop", "--host=elsewhere", "shop db", "a/b"] {
            let err =
                dump_plan(Engine::MySql, hostile, &mysql_env(), &Account::default()).unwrap_err();
            assert!(
                matches!(err, DbError::InvalidDatabase(_)),
                "{hostile:?} should be refused, got {err:?}"
            );
        }
    }

    #[test]
    fn an_ordinary_database_name_is_accepted() {
        for ok in ["shop", "my_db", "app-2", "Tickets"] {
            assert!(
                dump_plan(Engine::MySql, ok, &mysql_env(), &Account::default()).is_ok(),
                "{ok} should be allowed"
            );
        }
    }

    #[test]
    fn an_environment_value_containing_an_equals_sign_survives_intact() {
        let env = vec!["MYSQL_ROOT_PASSWORD=a=b=c".to_string()];
        let plan = dump_plan(Engine::MySql, "shop", &env, &Account::default()).unwrap();
        assert!(
            plan.env.contains(&"MYSQL_PWD=a=b=c".to_string()),
            "splitting on every equals sign would truncate the password"
        );
    }
    #[test]
    fn a_mysql_restore_passes_the_database_and_file_as_separate_arguments() {
        let plan = restore_plan(
            Engine::MySql,
            "shop",
            "/tmp/dump.sql",
            &mysql_env(),
            &Account::default(),
        )
        .unwrap();
        assert_eq!(plan.command[0], "sh");
        assert_eq!(plan.command[1], "-c");
        assert!(
            plan.command.contains(&"shop".to_string())
                && plan.command.contains(&"/tmp/dump.sql".to_string()),
            "the name and the path travel as their own arguments, so neither can be 
             read as part of the script"
        );
        assert!(!plan.command.iter().any(|arg| arg.contains("s3cr3t")));
        assert!(plan.env.contains(&"MYSQL_PWD=s3cr3t".to_string()));
    }

    #[test]
    fn a_postgres_restore_uses_the_containers_own_user() {
        let plan = restore_plan(
            Engine::Postgres,
            "tickets",
            "/tmp/d.sql",
            &postgres_env(),
            &Account::default(),
        )
        .unwrap();
        assert_eq!(plan.command[0], "sh");
        assert!(plan.command.contains(&"ticket".to_string()));
        assert!(plan.command.contains(&"tickets".to_string()));
        assert!(plan.env.contains(&"PGPASSWORD=p4ss".to_string()));
    }

    #[test]
    fn a_mongo_restore_reads_the_archive_it_was_given() {
        let plan = restore_plan(
            Engine::Mongo,
            "events",
            "/tmp/d.archive",
            &mongo_env(),
            &Account::default(),
        )
        .unwrap();
        assert!(plan.command.iter().any(|arg| arg.contains("mongorestore")));
        assert!(plan.command.contains(&"/tmp/d.archive".to_string()));
    }

    #[test]
    fn a_restore_refuses_the_same_database_names_a_dump_does() {
        let err = restore_plan(
            Engine::MySql,
            "shop; drop",
            "/tmp/d.sql",
            &mysql_env(),
            &Account::default(),
        )
        .unwrap_err();
        assert!(matches!(err, DbError::InvalidDatabase(_)), "got {err:?}");
    }

    #[test]
    fn a_restore_without_credentials_is_refused_rather_than_attempted() {
        let err = restore_plan(
            Engine::Postgres,
            "t",
            "/tmp/d.sql",
            &[],
            &Account::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, DbError::MissingCredential { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_whole_server_dump_takes_every_database_not_just_one() {
        let plan = dump_all_plan(Engine::MySql, &mysql_env(), &Account::default()).unwrap();
        assert!(plan.command.iter().any(|arg| arg == "--all-databases"));
        assert!(plan.env.contains(&"MYSQL_PWD=s3cr3t".to_string()));
    }

    #[test]
    fn postgres_uses_the_tool_that_covers_every_database() {
        let plan = dump_all_plan(Engine::Postgres, &postgres_env(), &Account::default()).unwrap();
        assert_eq!(
            plan.command[0], "pg_dumpall",
            "pg_dump takes one database; a backup of the server needs the other tool"
        );
        assert!(plan.command.iter().any(|arg| arg == "--username=ticket"));
        assert!(plan.env.contains(&"PGPASSWORD=p4ss".to_string()));
    }

    #[test]
    fn a_whole_server_mongo_dump_names_no_database() {
        let plan = dump_all_plan(Engine::Mongo, &mongo_env(), &Account::default()).unwrap();
        assert_eq!(plan.command[0], "mongodump");
        assert!(
            !plan.command.iter().any(|arg| arg.starts_with("--db=")),
            "naming a database would dump only that one"
        );
        assert!(plan.command.iter().any(|arg| arg == "--archive"));
    }

    #[test]
    fn a_whole_server_dump_still_needs_credentials() {
        let err = dump_all_plan(Engine::MySql, &[], &Account::default()).unwrap_err();
        assert!(
            matches!(err, DbError::MissingCredential { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn a_backup_file_is_named_for_the_service_and_the_engine_it_came_from() {
        assert_eq!(backup_filename("mysql", Engine::MySql, false), "mysql.sql");
        assert_eq!(
            backup_filename("mysql", Engine::MySql, true),
            "mysql.sql.gz"
        );
        assert_eq!(backup_filename("pg", Engine::Postgres, false), "pg.sql");
        assert_eq!(
            backup_filename("m", Engine::Mongo, false),
            "m.archive",
            "a mongo dump is not sql and should not pretend to be"
        );
    }
}
