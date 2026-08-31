//! How a collector is allowed to report.
//!
//! The shape here is the architectural rule made unavoidable: a scan pushes
//! results through a channel as it finds them, it does not hand back a
//! finished list. The predecessor blocked its own draw loop for 5,983 ms
//! precisely because gathering and drawing were the same call.

use crate::config::ProjectConfig;
use crate::domain::Project;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

/// One thing a scan learned. Failures travel alongside results rather than
/// replacing them, so an unreadable directory costs one row, not the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEvent {
    Found(Project),
    Failed {
        path: PathBuf,
        reason: String,
    },
    /// Sent once, last: how many directories were examined. Without it, a
    /// caller cannot tell "found nothing" apart from "looked at nothing".
    Finished {
        scanned: usize,
    },
}

pub trait ProjectScanner: Send + Sync {
    /// Reports each project through `sink` as soon as it is known. Callers
    /// that want everything at once use [`collect`]; callers that want to draw
    /// while waiting read the channel themselves.
    fn scan(&self, config: &ProjectConfig, sink: Sender<ScanEvent>);
}

/// Everything a finished scan produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScanOutcome {
    pub projects: Vec<Project>,
    pub failures: Vec<(PathBuf, String)>,
    /// `None` when the scan ended without reporting a total, which means it
    /// was cut short rather than completed.
    pub scanned: Option<usize>,
}

/// Drains a scan to completion. This is what the non-interactive commands use;
/// the terminal UI reads the same channel incrementally instead.
pub fn collect(rx: Receiver<ScanEvent>) -> ScanOutcome {
    let mut outcome = ScanOutcome::default();
    for event in rx {
        match event {
            ScanEvent::Found(project) => outcome.projects.push(project),
            ScanEvent::Failed { path, reason } => outcome.failures.push((path, reason)),
            ScanEvent::Finished { scanned } => outcome.scanned = Some(scanned),
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectConfig;
    use crate::domain::{GitStatus, Project, Stack};
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Sender};

    fn a_project(name: &str) -> Project {
        Project {
            name: name.to_string(),
            category: None,
            path: PathBuf::from(name),
            framework: None,
            stack: Stack::Rust,
            git: GitStatus::clean("main"),
        }
    }

    /// A scanner that reports two projects and one unreadable directory.
    struct FakeScanner;

    impl ProjectScanner for FakeScanner {
        fn scan(&self, _config: &ProjectConfig, sink: Sender<ScanEvent>) {
            sink.send(ScanEvent::Found(a_project("alpha"))).unwrap();
            sink.send(ScanEvent::Failed {
                path: PathBuf::from("locked"),
                reason: "permission denied".to_string(),
            })
            .unwrap();
            sink.send(ScanEvent::Found(a_project("beta"))).unwrap();
            sink.send(ScanEvent::Finished { scanned: 3 }).unwrap();
        }
    }

    #[test]
    fn collecting_a_scan_separates_projects_from_failures() {
        let (tx, rx) = mpsc::channel();
        FakeScanner.scan(&ProjectConfig::default(), tx);
        let outcome = collect(rx);

        assert_eq!(outcome.projects.len(), 2);
        assert_eq!(outcome.projects[0].name, "alpha");
        assert_eq!(outcome.projects[1].name, "beta");
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].0, PathBuf::from("locked"));
    }

    #[test]
    fn one_unreadable_directory_does_not_abort_the_rest_of_the_scan() {
        let (tx, rx) = mpsc::channel();
        FakeScanner.scan(&ProjectConfig::default(), tx);
        let outcome = collect(rx);
        assert!(
            !outcome.projects.is_empty(),
            "a failure in one directory must not cost the projects already found"
        );
    }

    #[test]
    fn a_scan_reports_how_many_directories_it_actually_looked_at() {
        let (tx, rx) = mpsc::channel();
        FakeScanner.scan(&ProjectConfig::default(), tx);
        let outcome = collect(rx);
        assert_eq!(
            outcome.scanned,
            Some(3),
            "without a denominator, 'found nothing' cannot be told apart from 'looked at nothing'"
        );
    }

    #[test]
    fn a_scan_that_ends_without_finishing_reports_no_denominator() {
        let (tx, rx) = mpsc::channel::<ScanEvent>();
        tx.send(ScanEvent::Found(a_project("alpha"))).unwrap();
        drop(tx);
        let outcome = collect(rx);
        assert_eq!(outcome.projects.len(), 1);
        assert_eq!(outcome.scanned, None);
    }
}
