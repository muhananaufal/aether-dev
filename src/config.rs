//! Everything the user is allowed to change. No path, port, or root directory
//! is a constant in the code: the previous tool hard-coded one machine's
//! layout and could therefore never run on anyone else's.

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
}
