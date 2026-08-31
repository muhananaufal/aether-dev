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
            Command::Services { json: true }
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
}
