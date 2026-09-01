//! Reading git state for a working tree.
//!
//! The branch is read straight from `.git/HEAD` rather than by running
//! `git rev-parse`: it is a single small file, and spawning a process per
//! repository is the cost this project exists to avoid. Only the change
//! counts need git itself.

use crate::domain::GitStatus;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GitError {
    #[error("git status did not answer within {0} ms")]
    Timeout(u64),
    #[error("could not run git: {0}")]
    Unavailable(String),
}

pub trait GitReader: Send + Sync {
    /// A directory that is not a repository is not an error; it reports a
    /// [`GitStatus::not_a_repository`]. `Err` means the answer is genuinely
    /// unknown, which callers must not render as "clean".
    fn status(&self, path: &Path) -> Result<GitStatus, GitError>;
}

/// Counts tracked changes and untracked entries in `git status --porcelain`
/// output. Pure, so the counting rules are testable without a repository.
pub fn parse_porcelain(output: &str) -> (usize, usize) {
    let mut modified = 0;
    let mut untracked = 0;
    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("??") {
            untracked += 1;
        } else {
            modified += 1;
        }
    }
    (modified, untracked)
}

/// Reads the current branch from `.git/HEAD`, following the `gitdir:` pointer
/// that worktrees and submodules leave behind. Returns `None` when the
/// directory is not a repository or the file cannot be read.
pub fn read_branch(path: &Path) -> Option<String> {
    let git_entry = path.join(".git");
    let head_path = if git_entry.is_dir() {
        git_entry.join("HEAD")
    } else if git_entry.is_file() {
        let pointer = std::fs::read_to_string(&git_entry).ok()?;
        let target = pointer.trim().strip_prefix("gitdir:")?.trim().to_string();
        Path::new(&target).join("HEAD")
    } else {
        return None;
    };

    let head = std::fs::read_to_string(head_path).ok()?;
    let head = head.trim();
    if let Some(branch) = head.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else if head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("detached:{}", &head[..7]))
    } else {
        None
    }
}

/// Reads status by running `git status --porcelain`.
pub struct GitCli {
    timeout_ms: u64,
    /// The git to run. A bare "git" is found on PATH, which is right almost
    /// always and wrong on a machine with more than one.
    program: String,
}

impl GitCli {
    pub fn new(timeout_ms: u64, program: impl Into<String>) -> Self {
        Self {
            timeout_ms,
            program: program.into(),
        }
    }
}

impl GitReader for GitCli {
    fn status(&self, path: &Path) -> Result<GitStatus, GitError> {
        let Some(branch) = read_branch(path) else {
            return Ok(GitStatus::not_a_repository());
        };

        // The work runs on its own thread so the timeout protects the scan
        // rather than the process: if git is wedged, we stop waiting for it
        // and let it exit on its own. Blocking here is what froze the tool
        // this one replaces.
        let dir = path.to_path_buf();
        let program = self.program.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = Command::new(&program)
                .args(["-C", &dir.to_string_lossy(), "status", "--porcelain"])
                .output();
            let _ = tx.send(result);
        });

        match rx.recv_timeout(Duration::from_millis(self.timeout_ms)) {
            Ok(Ok(output)) => {
                let text = String::from_utf8_lossy(&output.stdout);
                let (modified, untracked) = parse_porcelain(&text);
                Ok(GitStatus {
                    branch: Some(branch),
                    modified,
                    untracked,
                })
            }
            Ok(Err(e)) => Err(GitError::Unavailable(e.to_string())),
            Err(_) => Err(GitError::Timeout(self.timeout_ms)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn a_clean_worktree_reports_nothing_changed() {
        assert_eq!(parse_porcelain(""), (0, 0));
        assert_eq!(parse_porcelain("\n\n  \n"), (0, 0));
    }

    #[test]
    fn tracked_changes_and_untracked_files_are_counted_apart() {
        let output = " M src/main.rs\nA  src/new.rs\n?? notes.txt\n?? scratch/\n";
        assert_eq!(parse_porcelain(output), (2, 2));
    }

    #[test]
    fn a_renamed_file_counts_as_a_tracked_change() {
        assert_eq!(parse_porcelain("R  old.rs -> new.rs\n"), (1, 0));
    }

    #[test]
    fn crlf_output_is_counted_the_same_as_lf_output() {
        assert_eq!(parse_porcelain(" M a.rs\r\n?? b.rs\r\n"), (1, 1));
    }

    #[test]
    fn a_branch_is_read_from_the_head_file_without_running_git() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".git/HEAD"),
            "ref: refs/heads/feature/thing\n",
        )
        .unwrap();
        assert_eq!(read_branch(dir.path()), Some("feature/thing".to_string()));
    }

    #[test]
    fn a_detached_head_reports_the_short_commit_rather_than_a_branch() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".git/HEAD"),
            "9f2a1c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b\n",
        )
        .unwrap();
        assert_eq!(
            read_branch(dir.path()),
            Some("detached:9f2a1c4".to_string())
        );
    }

    #[test]
    fn a_directory_without_a_git_entry_has_no_branch() {
        let dir = tempdir().unwrap();
        assert_eq!(read_branch(dir.path()), None);
    }

    #[test]
    fn a_worktree_whose_git_is_a_file_is_followed_to_the_real_head() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("realgit");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let work = dir.path().join("work");
        fs::create_dir(&work).unwrap();
        fs::write(work.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();
        assert_eq!(read_branch(&work), Some("main".to_string()));
    }

    #[test]
    fn an_unreadable_head_is_reported_as_absent_rather_than_panicking() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(read_branch(dir.path()), None);
    }
}
