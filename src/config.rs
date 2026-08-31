//! Everything the user is allowed to change. No path, port, or root directory
//! is a constant in the code: the previous tool hard-coded one machine's
//! layout and could therefore never run on anyone else's.

use crate::catalog::ServiceConfig;
use crate::memory::MemoryConfig;
use crate::open::OpenConfig;
use crate::recipe::RunOverride;
use crate::toolchain::ToolConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config is not valid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config: {field} {reason}")]
    Invalid {
        field: &'static str,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub project: ProjectConfig,
    pub scan: ScanConfig,
    pub docker: DockerConfig,
    pub caddy: CaddyConfig,
    /// Where each tool's versions live. Empty by default: nobody can guess
    /// where somebody installed their interpreters, and guessing wrong is how
    /// the previous tool ended up blind to a version it did not know of.
    pub toolchain: HashMap<String, ToolConfig>,
    /// Versions chosen by hand per project, for the legacy ones whose manifest
    /// says nothing about what they need.
    pub pin: HashMap<String, HashMap<String, String>>,
    /// How to start a kind of project, replacing a built-in recipe for every
    /// project that uses it.
    pub recipe: HashMap<String, RunOverride>,
    /// How to start one named project, which beats the recipe because it is
    /// more specific — and because a project can be called `laravel`.
    pub run: HashMap<String, RunOverride>,
    /// The services this machine is meant to have. Declaring one makes it
    /// visible before it exists, which is the only way a dashboard can be used
    /// to create it — and it is where the container name, port, hostname and
    /// database credentials stop being this tool's guesses.
    pub service: HashMap<String, ServiceConfig>,
    /// How this machine hands a URL or a directory to the rest of the desktop.
    pub open: OpenConfig,
    /// What the container host costs this machine, shown in the footer.
    pub memory: MemoryConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CaddyConfig {
    /// Container restarted so the proxy picks up a regenerated config.
    pub container: String,
    /// The generated reverse-proxy config. Overwritten without asking, so it
    /// is an output of this tool rather than something to edit.
    pub caddyfile: PathBuf,
    /// The source of truth for local hostnames.
    pub domains: PathBuf,
}

impl Default for CaddyConfig {
    fn default() -> Self {
        Self {
            container: "caddy-proxy".to_string(),
            caddyfile: PathBuf::from("Caddyfile"),
            domains: PathBuf::from("domains.toml"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectConfig {
    /// Directories to scan for projects.
    pub roots: Vec<PathBuf>,
    /// How deep to descend below each root before giving up.
    pub max_depth: usize,
    /// Directory names never descended into.
    pub ignore: Vec<String>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            // The current directory, deliberately: a default that named one
            // developer's folder would be the same mistake this project exists
            // to undo. Real roots belong in the user's config file.
            roots: vec![PathBuf::from(".")],
            max_depth: 3,
            ignore: vec![
                "node_modules".to_string(),
                "vendor".to_string(),
                "target".to_string(),
                ".git".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScanConfig {
    /// How many repositories are inspected at once. Measured on the author's
    /// machine, 24 repositories took 5,983 ms one at a time and 850 ms in
    /// parallel; this number is the whole reason the rewrite exists.
    pub workers: usize,
    /// A repository that takes longer than this is reported as unknown rather
    /// than allowed to stall the scan.
    pub git_timeout_ms: u64,
    /// How long a completed scan stays fresh before it is worth repeating.
    pub cache_ttl_secs: u64,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            workers: 12,
            git_timeout_ms: 2000,
            cache_ttl_secs: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DockerConfig {
    /// How to reach the Docker daemon. `auto` means "work it out at connect
    /// time"; that resolution is not implemented yet and is the last open
    /// question from the design session, so no endpoint string is guessed here.
    pub endpoint: String,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            endpoint: "auto".to_string(),
        }
    }
}

impl Config {
    /// Reads configuration from `path`. With no path the defaults apply, which
    /// is a working configuration rather than an error: the tool should start
    /// before it is configured.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_toml_str(&text)
    }

    /// The declared services, in a fixed order. A map has none, and a list of
    /// services that reshuffles itself between refreshes cannot be selected
    /// from.
    pub fn services_declared(&self) -> Vec<(String, ServiceConfig)> {
        let mut declared: Vec<(String, ServiceConfig)> = self
            .service
            .iter()
            .map(|(name, settings)| (name.clone(), settings.clone()))
            .collect();
        declared.sort_by(|a, b| a.0.cmp(&b.0));
        declared
    }

    pub fn from_toml_str(text: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Rejects configurations that parse but cannot possibly work, so the
    /// failure lands at startup with a name attached rather than as an empty
    /// screen later.
    fn validate(&self) -> Result<(), ConfigError> {
        if self.project.roots.is_empty() {
            return Err(ConfigError::Invalid {
                field: "project.roots",
                reason: "must list at least one directory to scan",
            });
        }
        if self.scan.workers == 0 {
            return Err(ConfigError::Invalid {
                field: "scan.workers",
                reason: "must be at least 1",
            });
        }
        for (name, entry) in self.recipe.iter().chain(self.run.iter()) {
            // An entry that says nothing changes nothing, and reads as though
            // it did. Better to refuse it than to have somebody wonder why
            // their override had no effect.
            if entry.command.is_none() && entry.port.is_none() {
                let _ = name;
                return Err(ConfigError::Invalid {
                    field: "run",
                    reason: "must set a command, a port, or both",
                });
            }
        }
        for (name, tool) in &self.toolchain {
            // A tool with nowhere to look finds nothing, and finding nothing
            // looks exactly like having nothing installed. Say which it is.
            if tool.search.is_empty() && tool.versions.is_empty() {
                return Err(ConfigError::Invalid {
                    field: "toolchain.search",
                    reason: "must list a directory to look in, or name versions directly",
                });
            }
            if tool.binary.trim().is_empty() {
                return Err(ConfigError::Invalid {
                    field: "toolchain.binary",
                    reason: "must name the file that marks a directory as an install",
                });
            }
            let _ = name;
        }
        if self.caddy.container.trim().is_empty() {
            return Err(ConfigError::Invalid {
                field: "caddy.container",
                reason: "must name the container to restart after a change",
            });
        }
        for (name, service) in &self.service {
            if service
                .container
                .as_ref()
                .is_some_and(|c| c.trim().is_empty())
            {
                let _ = name;
                return Err(ConfigError::Invalid {
                    field: "service.container",
                    reason: "must name a container, or be left out to use the service name",
                });
            }
            // Two passwords is not twice as configured, it is a question about
            // which one is live that nobody should have to answer by experiment.
            if service.password.is_some() && service.password_env.is_some() {
                return Err(ConfigError::Invalid {
                    field: "service.password",
                    reason: "cannot be set alongside password_env; choose one",
                });
            }
        }
        if self.scan.git_timeout_ms == 0 {
            return Err(ConfigError::Invalid {
                field: "scan.git_timeout_ms",
                reason: "must be greater than zero",
            });
        }
        Ok(())
    }
}

/// The name looked for when no configuration file was named on the command
/// line.
pub const CONFIG_NAME: &str = "aether.toml";

/// Finds the configuration to use, nearest first.
///
/// The current directory and then each parent, so running `adev` from inside a
/// project still finds the configuration that sits above the whole workspace -
/// and so a single project can override it by keeping its own. Failing all of
/// those, the one for this machine.
///
/// Having to pass `--config` on every command is the kind of friction that
/// stops a tool being used at all.
pub fn discover(start: &Path, machine_wide: Option<&Path>) -> Option<PathBuf> {
    for directory in start.ancestors() {
        let candidate = directory.join(CONFIG_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    machine_wide
        .filter(|path| path.is_file())
        .map(Path::to_path_buf)
}

/// Writes a configuration describing this machine, for somebody who has not
/// got one yet.
///
/// Only what is actually there goes in. A generated file that names a
/// directory nobody has is worse than an empty one: it reads as a fact about
/// the machine and sends the reader looking for a bug that is not there.
///
/// It is a starting point rather than a settings screen. The file has comments
/// explaining the choices, and rewriting it from a form would throw those away
/// every time somebody changed a number.
pub fn starter(
    roots: &[PathBuf],
    toolchains: &[(String, PathBuf, String)],
    docker_host: Option<&str>,
) -> String {
    let mut out = String::from(
        "# Written by `adev config --init` from what was found on this machine.\n\
         # Everything here has a default; delete anything you do not want to pin.\n",
    );

    let present: Vec<&PathBuf> = roots.iter().filter(|root| root.is_dir()).collect();
    if !present.is_empty() {
        out.push_str("\n[project]\nroots = [");
        let quoted: Vec<String> = present
            .iter()
            .map(|root| {
                format!(
                    "\"{}\"",
                    root.display()
                        .to_string()
                        .replace(std::path::MAIN_SEPARATOR, "/")
                )
            })
            .collect();
        out.push_str(&quoted.join(", "));
        out.push_str("]\n");
    }

    if let Some(endpoint) = docker_host {
        out.push_str(&format!(
            "\n[docker]\n# Taken from DOCKER_HOST as it was set when this ran.\nendpoint = \"{endpoint}\"\n"
        ));
    }

    for (tool, directory, binary) in toolchains {
        if !holds_a_version(directory, binary) {
            continue;
        }
        out.push_str(&format!(
            "\n[toolchain.{tool}]\nsearch = [\"{}\"]\nbinary = \"{binary}\"\n",
            directory
                .display()
                .to_string()
                .replace(std::path::MAIN_SEPARATOR, "/")
        ));
    }

    out
}

/// Whether a directory actually holds at least one installation, rather than
/// merely existing.
fn holds_a_version(directory: &Path, binary: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        path.is_dir() && (path.join(binary).is_file() || path.join("bin").join(binary).is_file())
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn an_empty_config_is_valid_and_yields_the_defaults() {
        let cfg = Config::from_toml_str("").expect("an empty config must be valid");
        assert_eq!(cfg, Config::default());
        assert!(
            cfg.scan.workers > 0,
            "a default that cannot scan is not a default"
        );
    }

    #[test]
    fn a_partial_config_keeps_defaults_for_what_it_does_not_mention() {
        let cfg = Config::from_toml_str("[scan]\nworkers = 4\n").unwrap();
        let defaults = Config::default();
        assert_eq!(cfg.scan.workers, 4);
        assert_eq!(cfg.scan.cache_ttl_secs, defaults.scan.cache_ttl_secs);
        assert_eq!(cfg.project.roots, defaults.project.roots);
        assert_eq!(cfg.docker.endpoint, defaults.docker.endpoint);
    }

    #[test]
    fn project_roots_are_read_as_paths_not_strings() {
        let cfg = Config::from_toml_str("[project]\nroots = [\"C:/Projects\", \"E:/Projects\"]\n")
            .unwrap();
        assert_eq!(
            cfg.project.roots,
            vec![PathBuf::from("C:/Projects"), PathBuf::from("E:/Projects")]
        );
    }

    #[test]
    fn zero_workers_is_rejected_because_the_scan_would_never_run() {
        let err = Config::from_toml_str("[scan]\nworkers = 0\n").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn an_empty_root_list_is_rejected_because_there_is_nothing_to_scan() {
        let err = Config::from_toml_str("[project]\nroots = []\n").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn a_zero_git_timeout_is_rejected_because_every_repository_would_be_abandoned() {
        let err = Config::from_toml_str("[scan]\ngit_timeout_ms = 0\n").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn malformed_toml_is_reported_as_a_parse_error_not_a_panic() {
        let err = Config::from_toml_str("[scan").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)), "got {err:?}");
    }

    #[test]
    fn an_unknown_key_is_rejected_so_typos_do_not_pass_silently() {
        let err = Config::from_toml_str("[scan]\nworkerz = 4\n").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)), "got {err:?}");
    }
    #[test]
    fn a_missing_config_file_is_an_error_that_names_the_path() {
        let err = Config::load(Some(Path::new("no-such-config-9f2a1c4.toml"))).unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "got {err:?}");
        assert!(err.to_string().contains("no-such-config-9f2a1c4.toml"));
    }

    #[test]
    fn asking_for_no_config_file_at_all_yields_the_defaults() {
        assert_eq!(Config::load(None).unwrap(), Config::default());
    }

    #[test]
    fn a_config_file_is_read_from_disk_and_validated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aether.toml");
        std::fs::write(
            &path,
            "[scan]
workers = 3
",
        )
        .unwrap();
        assert_eq!(Config::load(Some(&path)).unwrap().scan.workers, 3);

        std::fs::write(
            &path,
            "[scan]
workers = 0
",
        )
        .unwrap();
        assert!(matches!(
            Config::load(Some(&path)).unwrap_err(),
            ConfigError::Invalid { .. }
        ));
    }
    #[test]
    fn the_proxy_section_has_working_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.caddy.container, "caddy-proxy");
        assert_eq!(cfg.caddy.caddyfile, PathBuf::from("Caddyfile"));
        assert_eq!(cfg.caddy.domains, PathBuf::from("domains.toml"));
    }

    #[test]
    fn the_proxy_paths_can_be_pointed_somewhere_else() {
        let cfg = Config::from_toml_str(
            "[caddy]
container = \"edge\"
caddyfile = \"conf/Caddyfile\"
",
        )
        .unwrap();
        assert_eq!(cfg.caddy.container, "edge");
        assert_eq!(cfg.caddy.caddyfile, PathBuf::from("conf/Caddyfile"));
        assert_eq!(
            cfg.caddy.domains,
            Config::default().caddy.domains,
            "what the file does not mention keeps its default"
        );
    }

    #[test]
    fn an_empty_proxy_container_is_rejected_because_nothing_could_be_reloaded() {
        let err = Config::from_toml_str(
            "[caddy]
container = \"\"
",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn no_toolchain_is_configured_by_default_because_nobody_can_guess_where_they_live() {
        assert!(Config::default().toolchain.is_empty());
        assert!(Config::default().pin.is_empty());
    }

    #[test]
    fn a_toolchain_names_where_to_look_and_what_to_look_for() {
        let cfg = Config::from_toml_str(
            "[toolchain.php]\nsearch = [\"C:/laragon/bin/php\"]\nbinary = \"php.exe\"\n",
        )
        .unwrap();
        let php = cfg.toolchain.get("php").expect("php configured");
        assert_eq!(php.search, vec![PathBuf::from("C:/laragon/bin/php")]);
        assert_eq!(php.binary, "php.exe");
        assert_eq!(php.bin_subdir, None);
    }

    #[test]
    fn a_toolchain_whose_binary_lives_in_a_subdirectory_can_say_so() {
        let cfg = Config::from_toml_str(
            "[toolchain.go]\nsearch = [\"C:/Go\"]\nbinary = \"go.exe\"\nbin_subdir = \"bin\"\n",
        )
        .unwrap();
        assert_eq!(cfg.toolchain["go"].bin_subdir.as_deref(), Some("bin"));
    }

    #[test]
    fn a_toolchain_with_nowhere_to_look_is_refused_rather_than_silently_finding_nothing() {
        let err = Config::from_toml_str("[toolchain.php]\nbinary = \"php.exe\"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn a_toolchain_that_does_not_say_what_binary_to_look_for_is_refused() {
        let err = Config::from_toml_str("[toolchain.php]\nsearch = [\"C:/php\"]\n").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn a_project_can_be_pinned_when_its_manifest_says_nothing() {
        let cfg = Config::from_toml_str(
            "[pin.legacy-billing]\nphp = \"5.6\"\n\n[pin.old-shop]\nphp = \"7.2\"\nnode = \"10\"\n",
        )
        .unwrap();
        assert_eq!(cfg.pin["legacy-billing"]["php"], "5.6");
        assert_eq!(cfg.pin["old-shop"]["node"], "10");
    }

    #[test]
    fn a_config_beside_you_is_found_without_being_named() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("aether.toml"), "").unwrap();
        assert_eq!(
            discover(dir.path(), None).as_deref(),
            Some(dir.path().join("aether.toml").as_path())
        );
    }

    #[test]
    fn a_config_further_up_is_found_from_inside_a_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("aether.toml"), "").unwrap();
        let deep = dir.path().join("devivace").join("some-app");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            discover(&deep, None).as_deref(),
            Some(dir.path().join("aether.toml").as_path()),
            "running adev from inside a project should still find the workspace config"
        );
    }

    #[test]
    fn the_nearest_config_wins_over_one_further_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("aether.toml"), "").unwrap();
        let inner = dir.path().join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("aether.toml"), "").unwrap();
        assert_eq!(
            discover(&inner, None).as_deref(),
            Some(inner.join("aether.toml").as_path())
        );
    }

    #[test]
    fn the_machine_wide_config_is_used_when_nothing_is_nearby() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home").join("aether.toml");
        std::fs::create_dir_all(home.parent().unwrap()).unwrap();
        std::fs::write(&home, "").unwrap();
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        assert_eq!(
            discover(&empty, Some(&home)).as_deref(),
            Some(home.as_path())
        );
    }

    #[test]
    fn a_machine_wide_path_that_does_not_exist_is_not_returned() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nowhere").join("aether.toml");
        assert_eq!(discover(dir.path(), Some(&missing)), None);
    }

    #[test]
    fn nothing_about_running_is_configured_by_default() {
        assert!(Config::default().recipe.is_empty());
        assert!(Config::default().run.is_empty());
    }

    #[test]
    fn a_recipe_can_be_replaced_for_every_project_that_uses_it() {
        let cfg = Config::from_toml_str(
            "[recipe.laravel]\ncommand = \"php artisan serve --host=0.0.0.0\"\nport = 8001\n",
        )
        .unwrap();
        assert_eq!(
            cfg.recipe["laravel"].command.as_deref(),
            Some("php artisan serve --host=0.0.0.0")
        );
        assert_eq!(cfg.recipe["laravel"].port, Some(8001));
    }

    #[test]
    fn one_project_can_be_given_its_own_command() {
        let cfg = Config::from_toml_str(
            "[run.old-billing]\ncommand = \"php -S localhost:9000 -t public\"\nport = 9000\n",
        )
        .unwrap();
        assert_eq!(cfg.run["old-billing"].port, Some(9000));
        assert!(cfg.recipe.is_empty(), "the two are separate on purpose");
    }

    #[test]
    fn an_entry_that_changes_nothing_is_refused_rather_than_ignored() {
        let err = Config::from_toml_str("[run.shop]\n").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn a_starter_config_only_offers_toolchains_that_are_actually_there() {
        let dir = tempfile::tempdir().unwrap();
        let php = dir.path().join("php").join("php-8.3");
        std::fs::create_dir_all(&php).unwrap();
        std::fs::write(php.join("php.exe"), "").unwrap();

        let written = starter(
            &[dir.path().join("nowhere-at-all")],
            &[(
                "php".to_string(),
                dir.path().join("php"),
                "php.exe".to_string(),
            )],
            Some("tcp://localhost:2375"),
        );

        assert!(written.contains("[toolchain.php]"));
        assert!(written.contains("php.exe"));
        assert!(
            !written.contains("nowhere-at-all"),
            "a root that is not there would be written as a fact about this machine"
        );
        assert!(written.contains("tcp://localhost:2375"));
    }

    #[test]
    fn a_toolchain_directory_with_no_versions_in_it_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("php")).unwrap();
        let written = starter(
            &[],
            &[(
                "php".to_string(),
                dir.path().join("php"),
                "php.exe".to_string(),
            )],
            None,
        );
        assert!(
            !written.contains("[toolchain.php]"),
            "an empty directory is not an installation, and saying so would send \
             somebody looking for a bug that is not there"
        );
    }

    #[test]
    fn a_starter_config_is_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects")).unwrap();
        let written = starter(&[dir.path().join("projects")], &[], None);
        Config::from_toml_str(&written).expect("what init writes must load");
    }

    #[test]
    fn no_service_is_declared_by_default_because_docker_is_asked_instead() {
        assert!(Config::default().service.is_empty());
    }

    #[test]
    fn a_service_can_name_its_container_port_domain_and_credentials() {
        let cfg = Config::from_toml_str(
            "[service.mysql]
container = \"mysql-db\"
port = 3306
domain = \"db.test\"
panel = \"http://localhost:8080\"
user = \"root\"
password_env = \"MYSQL_ROOT_PASSWORD\"
",
        )
        .unwrap();
        let mysql = &cfg.service["mysql"];
        assert_eq!(mysql.container.as_deref(), Some("mysql-db"));
        assert_eq!(mysql.port, Some(3306));
        assert_eq!(mysql.domain.as_deref(), Some("db.test"));
        assert_eq!(mysql.user.as_deref(), Some("root"));
        assert_eq!(mysql.password_env.as_deref(), Some("MYSQL_ROOT_PASSWORD"));
        assert_eq!(mysql.password, None);
    }

    #[test]
    fn a_service_may_be_declared_with_nothing_but_a_name() {
        let cfg = Config::from_toml_str("[service.redis]\n").unwrap();
        assert_eq!(
            cfg.service["redis"].container_for("redis"),
            "redis",
            "naming a service is enough to want it listed"
        );
    }

    #[test]
    fn a_service_that_gives_a_password_two_ways_is_refused() {
        let err = Config::from_toml_str(
            "[service.mysql]
password = \"literal\"
password_env = \"MYSQL_ROOT_PASSWORD\"
",
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn a_service_whose_container_name_is_blank_is_refused() {
        let err = Config::from_toml_str("[service.mysql]\ncontainer = \" \"\n").unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "got {err:?}");
    }

    #[test]
    fn declared_services_come_back_in_a_stable_order() {
        let cfg = Config::from_toml_str("[service.redis]\n[service.mysql]\n[service.postgres]\n")
            .unwrap();
        let names: Vec<String> = cfg
            .services_declared()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            vec!["mysql", "postgres", "redis"],
            "a map has no order, and a list that reshuffles itself is unusable"
        );
    }

    #[test]
    fn a_starter_config_says_where_it_came_from() {
        let written = starter(&[], &[], None);
        assert!(
            written.starts_with('#'),
            "a generated file should say it was generated on line one"
        );
    }
}
