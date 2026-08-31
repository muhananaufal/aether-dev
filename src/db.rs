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
        variable: &'static str,
    },
    #[error("{0:?} is not a usable database name")]
    InvalidDatabase(String),
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
) -> Result<ExecPlan, DbError> {
    let database = validated_database(database)?;

    match engine {
        Engine::MySql => {
            let password = value_of(container_env, "MYSQL_ROOT_PASSWORD").ok_or(
                DbError::MissingCredential {
                    container: "mysql",
                    variable: "MYSQL_ROOT_PASSWORD",
                },
            )?;
            Ok(ExecPlan {
                command: vec![
                    "mysqldump".to_string(),
                    "--user=root".to_string(),
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
            })
        }
        Engine::Postgres => {
            let user =
                value_of(container_env, "POSTGRES_USER").ok_or(DbError::MissingCredential {
                    container: "postgres",
                    variable: "POSTGRES_USER",
                })?;
            let password =
                value_of(container_env, "POSTGRES_PASSWORD").ok_or(DbError::MissingCredential {
                    container: "postgres",
                    variable: "POSTGRES_PASSWORD",
                })?;
            Ok(ExecPlan {
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
            })
        }
        Engine::Mongo => {
            let user = value_of(container_env, "MONGO_INITDB_ROOT_USERNAME").ok_or(
                DbError::MissingCredential {
                    container: "mongo",
                    variable: "MONGO_INITDB_ROOT_USERNAME",
                },
            )?;
            let password = value_of(container_env, "MONGO_INITDB_ROOT_PASSWORD").ok_or(
                DbError::MissingCredential {
                    container: "mongo",
                    variable: "MONGO_INITDB_ROOT_PASSWORD",
                },
            )?;
            Ok(ExecPlan {
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
            })
        }
    }
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
) -> Result<ExecPlan, DbError> {
    let database = validated_database(database)?;

    let (script, mut args, env) = match engine {
        Engine::MySql => {
            let password = value_of(container_env, "MYSQL_ROOT_PASSWORD").ok_or(
                DbError::MissingCredential {
                    container: "mysql",
                    variable: "MYSQL_ROOT_PASSWORD",
                },
            )?;
            (
                "mysql --user=root \"$1\" < \"$2\"",
                vec![database],
                vec![format!("MYSQL_PWD={password}")],
            )
        }
        Engine::Postgres => {
            let user =
                value_of(container_env, "POSTGRES_USER").ok_or(DbError::MissingCredential {
                    container: "postgres",
                    variable: "POSTGRES_USER",
                })?;
            let password =
                value_of(container_env, "POSTGRES_PASSWORD").ok_or(DbError::MissingCredential {
                    container: "postgres",
                    variable: "POSTGRES_PASSWORD",
                })?;
            (
                "psql --username=\"$1\" --dbname=\"$2\" --file=\"$3\" --set ON_ERROR_STOP=1",
                vec![user, database],
                vec![format!("PGPASSWORD={password}")],
            )
        }
        Engine::Mongo => {
            let user = value_of(container_env, "MONGO_INITDB_ROOT_USERNAME").ok_or(
                DbError::MissingCredential {
                    container: "mongo",
                    variable: "MONGO_INITDB_ROOT_USERNAME",
                },
            )?;
            let password = value_of(container_env, "MONGO_INITDB_ROOT_PASSWORD").ok_or(
                DbError::MissingCredential {
                    container: "mongo",
                    variable: "MONGO_INITDB_ROOT_PASSWORD",
                },
            )?;
            (
                "mongorestore --username=\"$1\" --password=\"$2\" \
                 --authenticationDatabase=admin --db=\"$3\" --archive=\"$4\"",
                vec![user, password, database],
                Vec::new(),
            )
        }
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

    #[test]
    fn a_mysql_dump_names_the_database_and_keeps_the_password_out_of_the_command() {
        let plan = dump_plan(Engine::MySql, "shop", &mysql_env()).unwrap();
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
        let plan = dump_plan(Engine::Postgres, "tickets", &postgres_env()).unwrap();
        assert_eq!(plan.command[0], "pg_dump");
        assert!(plan.command.iter().any(|arg| arg == "--username=ticket"));
        assert!(plan.command.iter().any(|arg| arg == "tickets"));
        assert!(!plan.command.iter().any(|arg| arg.contains("p4ss")));
        assert!(plan.env.contains(&"PGPASSWORD=p4ss".to_string()));
    }

    #[test]
    fn a_mongo_dump_authenticates_against_the_admin_database() {
        let plan = dump_plan(Engine::Mongo, "events", &mongo_env()).unwrap();
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
        let err = dump_plan(Engine::MySql, "shop", &["PATH=/usr/bin".to_string()]).unwrap_err();
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
            let err = dump_plan(Engine::MySql, hostile, &mysql_env()).unwrap_err();
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
                dump_plan(Engine::MySql, ok, &mysql_env()).is_ok(),
                "{ok} should be allowed"
            );
        }
    }

    #[test]
    fn an_environment_value_containing_an_equals_sign_survives_intact() {
        let env = vec!["MYSQL_ROOT_PASSWORD=a=b=c".to_string()];
        let plan = dump_plan(Engine::MySql, "shop", &env).unwrap();
        assert!(
            plan.env.contains(&"MYSQL_PWD=a=b=c".to_string()),
            "splitting on every equals sign would truncate the password"
        );
    }
    #[test]
    fn a_mysql_restore_passes_the_database_and_file_as_separate_arguments() {
        let plan = restore_plan(Engine::MySql, "shop", "/tmp/dump.sql", &mysql_env()).unwrap();
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
        let plan =
            restore_plan(Engine::Postgres, "tickets", "/tmp/d.sql", &postgres_env()).unwrap();
        assert_eq!(plan.command[0], "sh");
        assert!(plan.command.contains(&"ticket".to_string()));
        assert!(plan.command.contains(&"tickets".to_string()));
        assert!(plan.env.contains(&"PGPASSWORD=p4ss".to_string()));
    }

    #[test]
    fn a_mongo_restore_reads_the_archive_it_was_given() {
        let plan = restore_plan(Engine::Mongo, "events", "/tmp/d.archive", &mongo_env()).unwrap();
        assert!(plan.command.iter().any(|arg| arg.contains("mongorestore")));
        assert!(plan.command.contains(&"/tmp/d.archive".to_string()));
    }

    #[test]
    fn a_restore_refuses_the_same_database_names_a_dump_does() {
        let err =
            restore_plan(Engine::MySql, "shop; drop", "/tmp/d.sql", &mysql_env()).unwrap_err();
        assert!(matches!(err, DbError::InvalidDatabase(_)), "got {err:?}");
    }

    #[test]
    fn a_restore_without_credentials_is_refused_rather_than_attempted() {
        let err = restore_plan(Engine::Postgres, "t", "/tmp/d.sql", &[]).unwrap_err();
        assert!(
            matches!(err, DbError::MissingCredential { .. }),
            "got {err:?}"
        );
    }
}
