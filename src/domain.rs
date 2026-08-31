//! What the collectors produce. No I/O happens here.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What a project is built with, decided from the marker files in its directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stack {
    Laravel,
    Php,
    Node,
    Go,
    Rust,
    Python,
    Unknown,
}

impl Stack {
    /// Precedence is deliberate rather than incidental. A Laravel project also
    /// ships `composer.json`, so the more specific marker has to win no matter
    /// what order the directory listing happened to return.
    pub fn from_markers(markers: &[&str]) -> Stack {
        let has = |name: &str| markers.contains(&name);
        if has("artisan") && has("composer.json") {
            Stack::Laravel
        } else if has("composer.json") {
            Stack::Php
        } else if has("go.mod") {
            Stack::Go
        } else if has("Cargo.toml") {
            Stack::Rust
        } else if has("pyproject.toml") {
            Stack::Python
        } else if has("package.json") {
            Stack::Node
        } else {
            Stack::Unknown
        }
    }
}

/// Git state of one working tree. `branch` is `None` when the directory is not
/// a repository at all, which is a different thing from a repository with no
/// commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub modified: usize,
    pub untracked: usize,
}

impl GitStatus {
    pub fn clean(branch: &str) -> Self {
        Self {
            branch: Some(branch.to_string()),
            modified: 0,
            untracked: 0,
        }
    }

    pub fn not_a_repository() -> Self {
        Self {
            branch: None,
            modified: 0,
            untracked: 0,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.modified > 0 || self.untracked > 0
    }

    /// Compact marker for a list row. Empty when there is nothing to report,
    /// so callers can render it unconditionally.
    pub fn badge(&self) -> String {
        match (self.modified, self.untracked) {
            (0, 0) => String::new(),
            (m, 0) => format!("*{m}"),
            (0, u) => format!("?{u}"),
            (m, u) => format!("*{m} ?{u}"),
        }
    }
}

/// One directory recognised as a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Project {
    pub name: String,
    /// The directory the project was grouped under, when the root has one
    /// level of categories. `None` when the project sits directly in a root.
    pub category: Option<String>,
    pub path: PathBuf,
    pub stack: Stack,
    pub git: GitStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ServiceState {
    Running,
    Stopped,
}

/// One container from the compose file, as last observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceStatus {
    pub container: String,
    pub service: String,
    pub port: Option<u16>,
    pub state: ServiceState,
    pub port_open: bool,
    pub memory_bytes: Option<u64>,
}

impl ServiceStatus {
    /// Running is not the same as usable. A database container reports itself
    /// up long before it accepts connections, and showing it as ready during
    /// that window is how the previous tool lied to its user.
    pub fn is_reachable(&self) -> bool {
        self.state == ServiceState::Running && self.port_open
    }

    /// One word for what this service is actually doing. "running" and
    /// "usable" are deliberately different words: the tool this replaces used
    /// one word for both and told the user a database was available while it
    /// was still refusing connections.
    pub fn condition(&self) -> &'static str {
        match (self.state, self.port_open, self.port.is_some()) {
            (ServiceState::Stopped, _, _) => "stopped",
            (ServiceState::Running, true, _) => "ready",
            (ServiceState::Running, false, true) => "starting",
            (ServiceState::Running, false, false) => "running",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_repository_is_not_dirty_and_has_an_empty_badge() {
        let status = GitStatus::clean("main");
        assert!(!status.is_dirty());
        assert_eq!(status.badge(), "");
    }

    #[test]
    fn a_badge_reports_modified_and_untracked_counts_separately() {
        let status = GitStatus {
            branch: Some("main".to_string()),
            modified: 3,
            untracked: 2,
        };
        assert!(status.is_dirty());
        assert_eq!(status.badge(), "*3 ?2");
    }

    #[test]
    fn a_badge_omits_the_half_that_is_zero() {
        let only_modified = GitStatus {
            branch: Some("main".to_string()),
            modified: 3,
            untracked: 0,
        };
        let only_untracked = GitStatus {
            branch: Some("main".to_string()),
            modified: 0,
            untracked: 2,
        };
        assert_eq!(only_modified.badge(), "*3");
        assert_eq!(only_untracked.badge(), "?2");
    }

    #[test]
    fn a_directory_that_is_not_a_repository_has_no_branch() {
        let status = GitStatus::not_a_repository();
        assert_eq!(status.branch, None);
        assert!(!status.is_dirty());
        assert_eq!(status.badge(), "");
    }

    #[test]
    fn a_stack_is_decided_by_the_marker_files_present() {
        assert_eq!(Stack::from_markers(&["composer.json"]), Stack::Php);
        assert_eq!(Stack::from_markers(&["package.json"]), Stack::Node);
        assert_eq!(Stack::from_markers(&["go.mod"]), Stack::Go);
        assert_eq!(Stack::from_markers(&["Cargo.toml"]), Stack::Rust);
        assert_eq!(Stack::from_markers(&["pyproject.toml"]), Stack::Python);
        assert_eq!(Stack::from_markers(&["README.md"]), Stack::Unknown);
        assert_eq!(Stack::from_markers(&[]), Stack::Unknown);
    }

    #[test]
    fn laravel_outranks_plain_php_when_both_markers_are_present() {
        assert_eq!(
            Stack::from_markers(&["composer.json", "artisan"]),
            Stack::Laravel
        );
        assert_eq!(
            Stack::from_markers(&["artisan", "composer.json"]),
            Stack::Laravel
        );
    }

    #[test]
    fn a_service_is_reachable_only_when_running_and_its_port_answers() {
        let up = ServiceStatus {
            container: "mysql-db".into(),
            service: "mysql".into(),
            port: Some(3306),
            state: ServiceState::Running,
            port_open: true,
            memory_bytes: None,
        };
        let booting = ServiceStatus {
            state: ServiceState::Running,
            port_open: false,
            ..up.clone()
        };
        let down = ServiceStatus {
            state: ServiceState::Stopped,
            port_open: false,
            ..up.clone()
        };
        assert!(up.is_reachable());
        assert!(!booting.is_reachable());
        assert!(!down.is_reachable());
    }
    #[test]
    fn a_condition_word_separates_running_from_usable() {
        let base = ServiceStatus {
            container: "mysql-db".to_string(),
            service: "mysql".to_string(),
            port: Some(3306),
            state: ServiceState::Running,
            port_open: true,
            memory_bytes: None,
        };
        assert_eq!(base.condition(), "ready");
        assert_eq!(
            ServiceStatus {
                port_open: false,
                ..base.clone()
            }
            .condition(),
            "starting"
        );
        assert_eq!(
            ServiceStatus {
                port_open: false,
                port: None,
                ..base.clone()
            }
            .condition(),
            "running",
            "a container with nothing published is running, not stuck starting"
        );
        assert_eq!(
            ServiceStatus {
                state: ServiceState::Stopped,
                port_open: false,
                ..base
            }
            .condition(),
            "stopped"
        );
    }
}
