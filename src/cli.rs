//! The non-interactive surface. Every capability lives here first; the
//! terminal UI is a convenience layer that calls the same functions.
//!
//! This ordering is deliberate. Anything reachable only by navigating a TUI
//! cannot be put in a scheduler, cannot be piped, and cannot be tested without
//! driving a terminal.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser, PartialEq, Eq)]
#[command(
    name = "adev",
    version,
    about = "Terminal dashboard for a local development environment"
)]
pub struct Cli {
    /// Configuration file to read instead of the default location.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// List projects with their stack and git status.
    Scan {
        /// Emit JSON instead of a table, for scripts.
        #[arg(long)]
        json: bool,
    },
    /// Show the state of the containers defined by the compose file.
    Services {
        #[arg(long)]
        json: bool,
        /// Also report how much memory each one is using. Left out by default
        /// because the daemon takes about a second and a half per container to
        /// answer, which would make the common listing pay for the rare one.
        #[arg(long)]
        memory: bool,
    },
    /// Show which service ports are answering.
    Ports {
        #[arg(long)]
        json: bool,
    },
    /// Dump and restore databases.
    #[command(subcommand)]
    Db(DbCommand),
    /// Manage the local reverse-proxy hostnames.
    #[command(subcommand)]
    Domains(DomainCommand),
    /// Start services that already exist but are stopped.
    ///
    /// This starts existing containers. A service the compose file defines but
    /// that has never been created has no container to start, and is reported
    /// as such rather than silently skipped.
    Start {
        #[arg(required_unless_present = "all")]
        services: Vec<String>,
        /// Every service that has a container, rather than naming them.
        #[arg(long)]
        all: bool,
    },
    /// Stop running services, leaving their containers in place.
    Stop {
        #[arg(required_unless_present = "all")]
        services: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Restart services.
    Restart {
        #[arg(required_unless_present = "all")]
        services: Vec<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show which .env a project is running with, or switch it.
    Dotenv {
        project: String,
        /// The file to copy over .env. The one being replaced is kept as
        /// .env.bak, so a switch can be undone.
        #[arg(long = "use", value_name = "FILE")]
        use_file: Option<String>,
    },
    /// Open a service or a project in a browser.
    Open {
        /// A service name, or a project name — services are looked at first.
        target: String,
    },
    /// End whatever is holding a port.
    Kill {
        port: u16,
        /// Say what would be ended, without ending it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Follow what a service is writing.
    Logs {
        /// Service or container to read.
        service: String,
        /// Keep the stream open and print new lines as they arrive.
        #[arg(long, short)]
        follow: bool,
        /// Start this many lines back instead of at the beginning.
        #[arg(long, short)]
        tail: Option<u32>,
    },
    /// Start a project, with its own toolchain in front on PATH and the dev
    /// command its kind of project uses.
    Run {
        project: String,
        /// Say what would run, and on which port, without running it.
        #[arg(long)]
        print: bool,
    },
    /// Show which toolchain versions a project resolves to, and why.
    Env { project: String },
    /// Run a command with the project's toolchain in front on PATH.
    Exec {
        project: String,
        /// Everything after `--`. Taken whole so the command's own flags are
        /// never mistaken for this tool's.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Start a shell with the project's toolchain in front on PATH.
    Shell {
        project: String,
        /// Which shell to start. Without this, SHELL is used on unix and
        /// ComSpec on Windows.
        #[arg(long)]
        shell: Option<String>,
    },
    /// Open the interactive dashboard.
    Tui,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum DbCommand {
    /// Write a database to a file.
    Export {
        /// Service the database lives in, as named in the compose file.
        service: String,
        #[arg(long)]
        database: String,
        /// Destination file. Required: a dump written somewhere the caller did
        /// not choose is a dump the caller will not find.
        #[arg(long, value_name = "FILE")]
        out: PathBuf,
        /// Compress the dump with gzip.
        #[arg(long)]
        gzip: bool,
        /// Overwrite the destination if it already exists. Without this an
        /// existing file is left alone, because it is somebody's backup.
        #[arg(long)]
        force: bool,
    },
    /// Dump every database on every database service.
    Backup {
        /// Where to write. A timestamped directory is created inside it, so
        /// one backup never overwrites another.
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
        #[arg(long)]
        gzip: bool,
    },
    /// Load a dump file into a database.
    Import {
        service: String,
        #[arg(long)]
        database: String,
        #[arg(long, value_name = "FILE")]
        file: PathBuf,
    },
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum DomainCommand {
    /// Show the configured hostnames and what they point at.
    List,
    /// Route a hostname to a container inside the docker network.
    Add {
        /// Hostname to serve, such as db.localhost.
        host: String,
        /// Container and the port it listens on inside the network, written
        /// as container:port. Not the port published to this machine.
        upstream: String,
        #[arg(long)]
        no_reload: bool,
    },
    /// Stop routing a hostname.
    Remove {
        host: String,
        #[arg(long)]
        no_reload: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use std::path::PathBuf;

    #[test]
    fn the_command_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn listing_commands_are_human_readable_unless_json_is_asked_for() {
        assert_eq!(
            Cli::parse_from(["adev", "scan"]).command,
            Command::Scan { json: false }
        );
        assert_eq!(
            Cli::parse_from(["adev", "scan", "--json"]).command,
            Command::Scan { json: true }
        );
        assert_eq!(
            Cli::parse_from(["adev", "services", "--json"]).command,
            Command::Services {
                json: true,
                memory: false
            }
        );
        assert_eq!(
            Cli::parse_from(["adev", "ports", "--json"]).command,
            Command::Ports { json: true }
        );
    }

    #[test]
    fn a_database_export_names_the_service_the_database_and_the_destination() {
        let cli = Cli::parse_from([
            "adev",
            "db",
            "export",
            "mysql",
            "--database",
            "shop",
            "--out",
            "shop.sql",
        ]);
        assert_eq!(
            cli.command,
            Command::Db(DbCommand::Export {
                service: "mysql".to_string(),
                database: "shop".to_string(),
                out: PathBuf::from("shop.sql"),
                gzip: false,
                force: false,
            })
        );
    }

    #[test]
    fn an_export_without_a_destination_is_refused_rather_than_guessed() {
        assert!(
            Cli::try_parse_from(["adev", "db", "export", "mysql", "--database", "shop"]).is_err()
        );
    }

    #[test]
    fn a_database_import_names_the_file_it_reads() {
        let cli = Cli::parse_from([
            "adev",
            "db",
            "import",
            "postgres",
            "--database",
            "shop",
            "--file",
            "dump.sql",
        ]);
        assert_eq!(
            cli.command,
            Command::Db(DbCommand::Import {
                service: "postgres".to_string(),
                database: "shop".to_string(),
                file: PathBuf::from("dump.sql"),
            })
        );
    }

    #[test]
    fn domains_can_be_listed_added_and_removed() {
        assert_eq!(
            Cli::parse_from(["adev", "domains", "list"]).command,
            Command::Domains(DomainCommand::List)
        );
        assert_eq!(
            Cli::parse_from(["adev", "domains", "add", "db.localhost", "localhost:19000"]).command,
            Command::Domains(DomainCommand::Add {
                host: "db.localhost".to_string(),
                upstream: "localhost:19000".to_string(),
                no_reload: false,
            })
        );
        assert_eq!(
            Cli::parse_from(["adev", "domains", "remove", "db.localhost"]).command,
            Command::Domains(DomainCommand::Remove {
                host: "db.localhost".to_string(),
                no_reload: false,
            })
        );
    }

    #[test]
    fn the_terminal_ui_is_one_subcommand_among_the_others_not_the_default() {
        assert_eq!(Cli::parse_from(["adev", "tui"]).command, Command::Tui);
        assert!(
            Cli::try_parse_from(["adev"]).is_err(),
            "running with no subcommand must say so rather than silently pick one"
        );
    }

    #[test]
    fn the_config_path_is_accepted_before_or_after_the_subcommand() {
        assert_eq!(
            Cli::parse_from(["adev", "--config", "a.toml", "scan"]).config,
            Some(PathBuf::from("a.toml"))
        );
        assert_eq!(
            Cli::parse_from(["adev", "scan", "--config", "a.toml"]).config,
            Some(PathBuf::from("a.toml"))
        );
    }
    #[test]
    fn a_domain_change_can_be_written_without_restarting_the_proxy() {
        assert_eq!(
            Cli::parse_from(["adev", "domains", "add", "db.localhost", "dbgate-ui:3000"]).command,
            Command::Domains(DomainCommand::Add {
                host: "db.localhost".to_string(),
                upstream: "dbgate-ui:3000".to_string(),
                no_reload: false,
            })
        );
        assert_eq!(
            Cli::parse_from(["adev", "domains", "remove", "db.localhost", "--no-reload"]).command,
            Command::Domains(DomainCommand::Remove {
                host: "db.localhost".to_string(),
                no_reload: true,
            })
        );
    }
    #[test]
    fn an_export_refuses_to_overwrite_unless_told_to() {
        let plain = Cli::parse_from([
            "adev",
            "db",
            "export",
            "mysql",
            "--database",
            "shop",
            "--out",
            "shop.sql",
        ]);
        assert_eq!(
            plain.command,
            Command::Db(DbCommand::Export {
                service: "mysql".to_string(),
                database: "shop".to_string(),
                out: PathBuf::from("shop.sql"),
                gzip: false,
                force: false,
            })
        );
        let forced = Cli::parse_from([
            "adev",
            "db",
            "export",
            "mysql",
            "--database",
            "shop",
            "--out",
            "shop.sql",
            "--force",
        ]);
        assert!(matches!(
            forced.command,
            Command::Db(DbCommand::Export { force: true, .. })
        ));
    }

    #[test]
    fn logs_name_a_service_and_can_follow_or_limit_how_far_back_they_start() {
        assert_eq!(
            Cli::parse_from(["adev", "logs", "mysql"]).command,
            Command::Logs {
                service: "mysql".to_string(),
                follow: false,
                tail: None,
            }
        );
        assert_eq!(
            Cli::parse_from(["adev", "logs", "mysql", "--follow", "--tail", "50"]).command,
            Command::Logs {
                service: "mysql".to_string(),
                follow: true,
                tail: Some(50),
            }
        );
    }

    #[test]
    fn services_can_be_started_stopped_and_restarted_by_name() {
        assert_eq!(
            Cli::parse_from(["adev", "start", "mysql", "postgres"]).command,
            Command::Start {
                services: vec!["mysql".to_string(), "postgres".to_string()],
                all: false,
            }
        );
        assert_eq!(
            Cli::parse_from(["adev", "stop", "mysql"]).command,
            Command::Stop {
                services: vec!["mysql".to_string()],
                all: false,
            }
        );
        assert_eq!(
            Cli::parse_from(["adev", "restart", "caddy"]).command,
            Command::Restart {
                services: vec!["caddy".to_string()],
                all: false,
            }
        );
    }

    #[test]
    fn an_action_with_no_service_named_is_refused_rather_than_applied_to_everything() {
        assert!(
            Cli::try_parse_from(["adev", "stop"]).is_err(),
            "stopping every service because none was named is not a plausible intent"
        );
        assert!(Cli::try_parse_from(["adev", "start"]).is_err());
        assert!(Cli::try_parse_from(["adev", "restart"]).is_err());
    }

    #[test]
    fn memory_is_asked_for_explicitly_because_it_is_slow() {
        assert_eq!(
            Cli::parse_from(["adev", "services"]).command,
            Command::Services {
                json: false,
                memory: false
            },
            "a listing must stay fast by default"
        );
        assert_eq!(
            Cli::parse_from(["adev", "services", "--memory"]).command,
            Command::Services {
                json: false,
                memory: true
            }
        );
    }

    #[test]
    fn a_backup_names_a_directory_rather_than_a_file() {
        assert_eq!(
            Cli::parse_from(["adev", "db", "backup", "--out", "backups"]).command,
            Command::Db(DbCommand::Backup {
                out: PathBuf::from("backups"),
                gzip: false,
            })
        );
        assert!(
            Cli::try_parse_from(["adev", "db", "backup"]).is_err(),
            "writing a backup somewhere the caller did not choose is not a default"
        );
    }

    #[test]
    fn a_project_can_be_asked_what_toolchain_it_resolves_to() {
        assert_eq!(
            Cli::parse_from(["adev", "env", "legacy-billing"]).command,
            Command::Env {
                project: "legacy-billing".to_string()
            }
        );
    }

    #[test]
    fn a_command_runs_after_a_separator_so_its_own_flags_are_left_alone() {
        let cli = Cli::parse_from(["adev", "exec", "old-shop", "--", "php", "-v"]);
        assert_eq!(
            cli.command,
            Command::Exec {
                project: "old-shop".to_string(),
                command: vec!["php".to_string(), "-v".to_string()],
            }
        );

        let with_flags = Cli::parse_from(["adev", "exec", "old-shop", "--", "php", "--version"]);
        assert!(
            matches!(with_flags.command, Command::Exec { .. }),
            "--version belongs to php, not to adev"
        );
    }

    #[test]
    fn exec_without_a_command_is_refused_rather_than_running_nothing() {
        assert!(Cli::try_parse_from(["adev", "exec", "old-shop"]).is_err());
    }

    #[test]
    fn a_shell_can_be_named_when_the_environment_does_not_say() {
        assert_eq!(
            Cli::parse_from(["adev", "shell", "old-shop"]).command,
            Command::Shell {
                project: "old-shop".to_string(),
                shell: None,
            }
        );
        assert_eq!(
            Cli::parse_from(["adev", "shell", "old-shop", "--shell", "pwsh"]).command,
            Command::Shell {
                project: "old-shop".to_string(),
                shell: Some("pwsh".to_string()),
            }
        );
    }

    #[test]
    fn a_project_can_be_started_or_only_described() {
        assert_eq!(
            Cli::parse_from(["adev", "run", "sapta-web"]).command,
            Command::Run {
                project: "sapta-web".to_string(),
                print: false,
            }
        );
        assert_eq!(
            Cli::parse_from(["adev", "run", "sapta-web", "--print"]).command,
            Command::Run {
                project: "sapta-web".to_string(),
                print: true,
            }
        );
        assert!(
            Cli::try_parse_from(["adev", "run"]).is_err(),
            "there is no sensible project to start when none was named"
        );
    }

    #[test]
    fn freeing_a_port_names_the_port_and_can_be_asked_first() {
        assert_eq!(
            Cli::parse_from(["adev", "kill", "8000"]).command,
            Command::Kill {
                port: 8000,
                dry_run: false,
            }
        );
        assert!(matches!(
            Cli::parse_from(["adev", "kill", "8000", "--dry-run"]).command,
            Command::Kill { dry_run: true, .. }
        ));
        assert!(
            Cli::try_parse_from(["adev", "kill"]).is_err(),
            "killing whatever happens to be listening is not a default"
        );
        assert!(
            Cli::try_parse_from(["adev", "kill", "not-a-port"]).is_err(),
            "a port is a number, and refusing early beats failing later"
        );
    }

    #[test]
    fn every_service_can_be_acted_on_at_once_but_only_when_asked() {
        assert_eq!(
            Cli::parse_from(["adev", "stop", "--all"]).command,
            Command::Stop {
                services: vec![],
                all: true,
            }
        );
        assert_eq!(
            Cli::parse_from(["adev", "start", "mysql"]).command,
            Command::Start {
                services: vec!["mysql".to_string()],
                all: false,
            }
        );
        assert!(
            Cli::try_parse_from(["adev", "stop"]).is_err(),
            "stopping everything because nothing was named is not a plausible intent"
        );
    }

    #[test]
    fn opening_something_in_a_browser_names_what_to_open() {
        assert_eq!(
            Cli::parse_from(["adev", "open", "dbgate"]).command,
            Command::Open {
                target: "dbgate".to_string()
            }
        );
        assert!(Cli::try_parse_from(["adev", "open"]).is_err());
    }

    #[test]
    fn the_env_file_in_use_can_be_listed_or_swapped() {
        assert_eq!(
            Cli::parse_from(["adev", "dotenv", "old-billing"]).command,
            Command::Dotenv {
                project: "old-billing".to_string(),
                use_file: None,
            }
        );
        assert_eq!(
            Cli::parse_from(["adev", "dotenv", "old-billing", "--use", ".env.staging"]).command,
            Command::Dotenv {
                project: "old-billing".to_string(),
                use_file: Some(".env.staging".to_string()),
            }
        );
    }
}
