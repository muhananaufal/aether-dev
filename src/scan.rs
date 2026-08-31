//! Walking the project roots and asking git about each result.
//!
//! Two phases on purpose. Finding candidate directories is cheap and
//! sequential; asking git about them is expensive and runs across a bounded
//! pool. Measured on 24 repositories: 5,983 ms one at a time, 850 ms this way.

use crate::config::ProjectConfig;
use crate::domain::{Project, Stack};
use crate::framework;
use crate::git::GitReader;
use crate::ports::{ProjectScanner, ScanEvent};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Files whose presence makes a directory a project.
const MARKERS: &[&str] = &[
    "artisan",
    "composer.json",
    "package.json",
    "go.mod",
    "Cargo.toml",
    "pyproject.toml",
];

/// A directory that looks like a project, before git has been consulted.
struct Candidate {
    path: PathBuf,
    name: String,
    category: Option<String>,
    stack: Stack,
}

pub struct FsProjectScanner<G: GitReader> {
    git: Arc<G>,
    workers: usize,
}

impl<G: GitReader + 'static> FsProjectScanner<G> {
    pub fn new(git: G, workers: usize) -> Self {
        Self {
            git: Arc::new(git),
            workers: workers.max(1),
        }
    }
}

impl<G: GitReader + 'static> ProjectScanner for FsProjectScanner<G> {
    fn scan(&self, config: &ProjectConfig, sink: Sender<ScanEvent>) {
        let mut candidates = Vec::new();
        let mut examined = 0usize;

        for root in &config.roots {
            if !root.is_dir() {
                let _ = sink.send(ScanEvent::Failed {
                    path: root.clone(),
                    reason: "not a readable directory".to_string(),
                });
                continue;
            }
            walk(root, root, 1, config, &mut candidates, &mut examined);
        }

        let queue = Arc::new(Mutex::new(VecDeque::from(candidates)));
        let mut workers = Vec::with_capacity(self.workers);
        for _ in 0..self.workers {
            let queue = Arc::clone(&queue);
            let git = Arc::clone(&self.git);
            let sink = sink.clone();
            workers.push(std::thread::spawn(move || loop {
                let next = queue.lock().expect("scan queue poisoned").pop_front();
                let Some(candidate) = next else { break };
                let event = match git.status(&candidate.path) {
                    Ok(status) => ScanEvent::Found(Project {
                        name: candidate.name,
                        category: candidate.category,
                        framework: read_framework(&candidate.path),
                        path: candidate.path,
                        stack: candidate.stack,
                        git: status,
                    }),
                    // An unknown answer is reported as a failure rather than
                    // as a clean repository. Rendering "0 changed" for a
                    // repository nobody could read is a lie the user acts on.
                    Err(error) => ScanEvent::Failed {
                        path: candidate.path,
                        reason: error.to_string(),
                    },
                };
                let _ = sink.send(event);
            }));
        }
        for worker in workers {
            let _ = worker.join();
        }

        let _ = sink.send(ScanEvent::Finished { scanned: examined });
    }
}

/// Collects candidate directories. A directory holding a marker is a project
/// and is not descended into further, so a project's own dependencies never
/// appear as projects of their own.
fn walk(
    dir: &Path,
    root: &Path,
    depth: usize,
    config: &ProjectConfig,
    out: &mut Vec<Candidate>,
    examined: &mut usize,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || config.ignore.contains(&name) {
            continue;
        }

        *examined += 1;
        let markers = markers_in(&path);
        if markers.is_empty() {
            if depth < config.max_depth {
                walk(&path, root, depth + 1, config, out, examined);
            }
            continue;
        }

        let marker_refs: Vec<&str> = markers.iter().map(String::as_str).collect();
        let category = path
            .parent()
            .filter(|parent| *parent != root)
            .and_then(Path::file_name)
            .map(|c| c.to_string_lossy().into_owned());

        out.push(Candidate {
            name,
            category,
            stack: Stack::from_markers(&marker_refs),
            path,
        });
    }
}

fn markers_in(dir: &Path) -> Vec<String> {
    MARKERS
        .iter()
        .filter(|marker| dir.join(marker).is_file())
        .map(|marker| (*marker).to_string())
        .collect()
}

/// Reads whichever manifests a project happens to have and names its
/// framework. Runs inside the worker pool alongside the git call, because it
/// is file reading and belongs with the other slow part rather than in the
/// sequential walk.
fn read_framework(path: &Path) -> Option<String> {
    let at = |relative: &str| std::fs::read_to_string(path.join(relative)).ok();

    let composer_json = at("composer.json");
    let laravel_application_php = composer_json
        .is_some()
        .then(|| at("vendor/laravel/framework/src/Illuminate/Foundation/Application.php"))
        .flatten();
    let package_json = at("package.json");
    let codeigniter_php = at("system/core/CodeIgniter.php");
    let go_mod = at("go.mod");
    let cargo_toml = at("Cargo.toml");

    framework::detect(&framework::Manifests {
        composer_json: composer_json.as_deref(),
        laravel_application_php: laravel_application_php.as_deref(),
        package_json: package_json.as_deref(),
        codeigniter_php: codeigniter_php.as_deref(),
        go_mod: go_mod.as_deref(),
        cargo_toml: cargo_toml.as_deref(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::GitStatus;
    use crate::git::{GitError, GitReader};
    use crate::ports::collect;
    use std::fs;
    use tempfile::{tempdir, TempDir};

    /// Answers without touching git, so these tests prove the walking logic
    /// rather than the state of whatever repositories exist on the machine.
    struct FakeGit;
    impl GitReader for FakeGit {
        fn status(&self, _path: &Path) -> Result<GitStatus, GitError> {
            Ok(GitStatus::clean("main"))
        }
    }

    /// Every repository is unreadable, the way a wedged git looks.
    struct TimingOutGit;
    impl GitReader for TimingOutGit {
        fn status(&self, _path: &Path) -> Result<GitStatus, GitError> {
            Err(GitError::Timeout(2000))
        }
    }

    /// root/alpha/Cargo.toml, root/group/beta/go.mod, root/node_modules/pkg/package.json
    fn a_tree() -> TempDir {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("alpha")).unwrap();
        fs::write(root.join("alpha/Cargo.toml"), "").unwrap();
        fs::create_dir_all(root.join("group/beta")).unwrap();
        fs::write(root.join("group/beta/go.mod"), "").unwrap();
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("node_modules/pkg/package.json"), "").unwrap();
        dir
    }

    fn config_for(root: &Path, max_depth: usize) -> ProjectConfig {
        ProjectConfig {
            roots: vec![root.to_path_buf()],
            max_depth,
            ignore: vec!["node_modules".to_string(), ".git".to_string()],
        }
    }

    fn scan_with<G: GitReader + 'static>(
        git: G,
        config: &ProjectConfig,
    ) -> crate::ports::ScanOutcome {
        let (tx, rx) = std::sync::mpsc::channel();
        FsProjectScanner::new(git, 4).scan(config, tx);
        collect(rx)
    }

    #[test]
    fn a_directory_holding_a_marker_file_is_recognised_as_a_project() {
        let dir = a_tree();
        let outcome = scan_with(FakeGit, &config_for(dir.path(), 3));
        let alpha = outcome
            .projects
            .iter()
            .find(|p| p.name == "alpha")
            .expect("alpha");
        assert_eq!(alpha.stack, Stack::Rust);
        assert_eq!(alpha.category, None);
    }

    #[test]
    fn a_project_nested_under_a_plain_folder_takes_that_folder_as_its_category() {
        let dir = a_tree();
        let outcome = scan_with(FakeGit, &config_for(dir.path(), 3));
        let beta = outcome
            .projects
            .iter()
            .find(|p| p.name == "beta")
            .expect("beta");
        assert_eq!(beta.stack, Stack::Go);
        assert_eq!(beta.category, Some("group".to_string()));
    }

    #[test]
    fn ignored_directory_names_are_never_descended_into() {
        let dir = a_tree();
        let outcome = scan_with(FakeGit, &config_for(dir.path(), 3));
        assert!(
            !outcome.projects.iter().any(|p| p.name == "pkg"),
            "node_modules was walked into; a real machine would drown in dependencies"
        );
    }

    #[test]
    fn the_walk_stops_at_the_configured_depth() {
        let dir = a_tree();
        let shallow = scan_with(FakeGit, &config_for(dir.path(), 1));
        assert!(shallow.projects.iter().any(|p| p.name == "alpha"));
        assert!(!shallow.projects.iter().any(|p| p.name == "beta"));
    }

    #[test]
    fn the_scan_reports_how_many_directories_it_examined() {
        let dir = a_tree();
        let outcome = scan_with(FakeGit, &config_for(dir.path(), 3));
        assert_eq!(outcome.projects.len(), 2);
        assert_eq!(
            outcome.scanned,
            Some(3),
            "alpha, group and beta were examined; node_modules was skipped whole"
        );
    }

    #[test]
    fn a_root_that_does_not_exist_is_reported_as_a_failure_not_a_panic() {
        let missing = PathBuf::from("definitely-not-a-real-directory-9f2a1c4");
        let config = ProjectConfig {
            roots: vec![missing.clone()],
            max_depth: 3,
            ignore: vec![],
        };
        let outcome = scan_with(FakeGit, &config);
        assert!(outcome.projects.is_empty());
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].0, missing);
    }

    #[test]
    fn a_repository_git_could_not_read_is_a_failure_not_a_clean_repository() {
        let dir = a_tree();
        let outcome = scan_with(TimingOutGit, &config_for(dir.path(), 3));
        assert!(
            outcome.projects.is_empty(),
            "reporting an unreadable repository as having no changes is a lie the user acts on"
        );
        assert_eq!(outcome.failures.len(), 2);
        assert!(outcome
            .failures
            .iter()
            .all(|(_, reason)| reason.contains("2000 ms")));
    }

    #[test]
    fn every_project_carries_the_git_status_the_reader_gave_it() {
        let dir = a_tree();
        let outcome = scan_with(FakeGit, &config_for(dir.path(), 3));
        assert!(outcome
            .projects
            .iter()
            .all(|p| p.git.branch.as_deref() == Some("main")));
    }
}
