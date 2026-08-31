//! Choosing which installed version of a toolchain a project should run with.
//!
//! This exists for projects that are not in containers: a machine holding PHP
//! 5.6, 7.4 and 8.3 at once, and a directory full of projects that each need a
//! different one. Docker answers that question by isolating; without it,
//! something has to read what the project asks for and put the right binary in
//! front of the wrong one on PATH.
//!
//! Everything here is pure. Where the versions live on disk is configuration,
//! not a guess about where somebody installed laragon.

use semver::{Version, VersionReq};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One version of a tool found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// How the directory named it, kept for display: "php-8.3.6-Win32-vs16-x64"
    /// tells the user more than "8.3.6" about which install this is.
    pub label: String,
    pub version: Version,
    /// The directory to put on PATH, not the binary itself.
    pub path: PathBuf,
}

/// Reads a version out of whatever a directory happens to be called:
/// `php-8.3.6-Win32-vs16-x64`, `v18.20.4`, `go1.24.3`.
///
/// A two-part version is padded, because `8.1` and `8.1.0` are the same
/// install and only one of them parses. A name with no version at all - like
/// the `active` symlink some setups keep - yields nothing rather than a guess.
pub fn version_in(name: &str) -> Option<Version> {
    let mut digits = String::new();
    let mut parts: Vec<String> = Vec::new();

    let flush = |digits: &mut String, parts: &mut Vec<String>| {
        if !digits.is_empty() {
            parts.push(std::mem::take(digits));
        }
    };

    for character in name.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
        } else if character == '.' && !digits.is_empty() {
            flush(&mut digits, &mut parts);
        } else {
            // Any other character ends the run. A version already two parts
            // long is the answer; anything shorter was just a number in a name.
            if parts.len() >= 2 {
                flush(&mut digits, &mut parts);
                break;
            }
            digits.clear();
            parts.clear();
        }
    }
    flush(&mut digits, &mut parts);

    if parts.len() < 2 {
        return None;
    }
    parts.truncate(3);
    while parts.len() < 3 {
        parts.push("0".to_string());
    }
    Version::parse(&parts.join(".")).ok()
}

/// Makes a bare version mean that version, not anything newer.
///
/// The semver crate reads `8.1` as `^8.1`, which allows 8.3. Composer and npm
/// both read it as 8.1.x, and so does anyone who types it into a config file
/// meaning "run this project on 8.1". Left alone, pinning a legacy project to
/// 7.4 would hand it 7.4 or anything above within the major - and the whole
/// point of pinning was that the newer one does not work.
fn pin_bare_version(token: &str) -> String {
    let bare = token
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_digit())
        && !token.contains('*');
    if bare {
        format!("={token}")
    } else {
        token.to_string()
    }
}

/// Turns what a manifest writes into something comparable.
///
/// Composer and npm both allow spaces for "and" and `||` for "or", while the
/// semver crate wants commas and has no "or", so the text is normalised and
/// the alternatives are kept as separate requirements.
fn requirements(constraint: &str) -> Vec<VersionReq> {
    constraint
        .split("||")
        .filter_map(|branch| {
            let normalised = branch
                .trim()
                .replace(".x", ".*")
                .split_whitespace()
                .map(pin_bare_version)
                .collect::<Vec<_>>()
                .join(",");
            if normalised.is_empty() {
                return None;
            }
            VersionReq::parse(&normalised).ok()
        })
        .collect()
}

/// The highest installed version the constraint allows.
///
/// Highest rather than first found, and compared as numbers rather than text:
/// sorting "8.9" against "8.10" as strings picks the older one, which is how a
/// version switcher quietly runs the wrong binary for months.
///
/// Nothing matching answers with nothing. Falling back to the nearest version
/// would run a project on an interpreter it said it could not use.
pub fn select<'a>(installed: &'a [Installed], constraint: &str) -> Option<&'a Installed> {
    let allowed = requirements(constraint);
    if allowed.is_empty() {
        return None;
    }
    installed
        .iter()
        .filter(|candidate| {
            allowed
                .iter()
                .any(|requirement| requirement.matches(&candidate.version))
        })
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// The newest of what is installed, for a project that asks for nothing.
pub fn newest(installed: &[Installed]) -> Option<&Installed> {
    installed.iter().max_by(|a, b| a.version.cmp(&b.version))
}

#[derive(Deserialize)]
struct ComposerRequire {
    #[serde(default)]
    require: HashMap<String, String>,
}

#[derive(Deserialize)]
struct PackageEngines {
    #[serde(default)]
    engines: HashMap<String, String>,
}

/// The PHP constraint a composer manifest declares, if it declares one.
pub fn wanted_php(composer_json: &str) -> Option<String> {
    let composer: ComposerRequire = serde_json::from_str(composer_json).ok()?;
    composer.require.get("php").cloned()
}

/// The Node constraint a package manifest declares, if it declares one.
pub fn wanted_node(package_json: &str) -> Option<String> {
    let package: PackageEngines = serde_json::from_str(package_json).ok()?;
    package.engines.get("node").cloned()
}

/// Where one tool's versions live on this machine.
///
/// Nothing here is discovered by convention. The previous tool hard-coded
/// three laragon paths and a list of version numbers it knew about, so a
/// fourth install location or a PHP 8.4 simply did not exist as far as it was
/// concerned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ToolConfig {
    /// Directories whose subdirectories are versions.
    pub search: Vec<PathBuf>,
    /// The file that has to be present for a directory to count as an install.
    pub binary: String,
    /// Where that file sits inside a version directory, when it is not at the
    /// top: Go keeps its binaries in `bin`, Node does not.
    pub bin_subdir: Option<String>,
    /// Versions pointed at by hand, for installs whose directory name says
    /// nothing useful.
    pub versions: HashMap<String, PathBuf>,
}

/// Finds every usable version of one tool.
///
/// A directory only counts when it both names a version and actually holds the
/// binary. Offering an install whose binary is missing just moves the failure
/// to the moment somebody tries to use it.
pub fn discover(config: &ToolConfig) -> Vec<Installed> {
    let mut found: Vec<Installed> = Vec::new();

    let bin_dir = |version_dir: &Path| match &config.bin_subdir {
        Some(sub) => version_dir.join(sub),
        None => version_dir.to_path_buf(),
    };
    let usable = |version_dir: &Path| bin_dir(version_dir).join(&config.binary).is_file();

    for root in &config.search {
        let Ok(entries) = std::fs::read_dir(root) else {
            // A search path that is not there is somebody's other machine, not
            // an error on this one.
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let label = entry.file_name().to_string_lossy().into_owned();
            let Some(version) = version_in(&label) else {
                continue;
            };
            if !usable(&path) {
                continue;
            }
            found.push(Installed {
                label,
                version,
                path: bin_dir(&path),
            });
        }
    }

    // Named by hand last, so an explicit entry replaces whatever discovery
    // made of the same version.
    for (version, path) in &config.versions {
        let Some(version) = version_in(version) else {
            continue;
        };
        if !usable(path) {
            continue;
        }
        found.retain(|existing| existing.version != version);
        found.push(Installed {
            label: version.to_string(),
            version,
            path: bin_dir(path),
        });
    }

    found.sort_by(|a, b| a.version.cmp(&b.version));
    found
}

/// Why a version was chosen, or why none was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The configuration pinned this project to a version.
    Pinned,
    /// The project's own manifest asked for it.
    Declared,
    /// The project asked for nothing, so the newest install was taken.
    Newest,
    /// Something is installed, but none of it satisfies what was asked for.
    NothingSatisfies,
    /// No version of this tool was found at all.
    NoneInstalled,
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub chosen: Option<Installed>,
    pub constraint: Option<String>,
    pub reason: Reason,
}

/// Decides which installed version a project should run with.
///
/// A pin beats a manifest, because a pin is somebody looking at this project
/// and deciding; a manifest is what the project claimed, which for a legacy
/// codebase is often wrong or missing.
///
/// When a constraint is given and nothing satisfies it, nothing is chosen.
/// Falling back to the nearest version would run the project on an interpreter
/// it said it could not use, which is exactly the failure this exists to
/// prevent - and it would do it silently.
pub fn resolve(
    installed: &[Installed],
    pinned: Option<&str>,
    declared: Option<&str>,
) -> Resolution {
    let (constraint, reason) = match (pinned, declared) {
        (Some(pin), _) => (Some(pin.to_string()), Reason::Pinned),
        (None, Some(asked)) => (Some(asked.to_string()), Reason::Declared),
        (None, None) => (None, Reason::Newest),
    };

    if installed.is_empty() {
        return Resolution {
            chosen: None,
            constraint,
            // Told apart from "nothing matches" on purpose: one is fixed by
            // installing a version, the other by changing what is asked for.
            reason: Reason::NoneInstalled,
        };
    }

    match &constraint {
        None => Resolution {
            chosen: newest(installed).cloned(),
            constraint,
            reason,
        },
        Some(wanted) => match select(installed, wanted) {
            Some(found) => Resolution {
                chosen: Some(found.clone()),
                constraint,
                reason,
            },
            None => Resolution {
                chosen: None,
                constraint,
                reason: Reason::NothingSatisfies,
            },
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn installed(versions: &[&str]) -> Vec<Installed> {
        versions
            .iter()
            .map(|label| Installed {
                label: (*label).to_string(),
                version: version_in(label).expect("fixture version"),
                path: PathBuf::from(format!("/tools/{label}")),
            })
            .collect()
    }

    #[test]
    fn a_version_is_found_inside_the_name_a_directory_happens_to_have() {
        assert_eq!(version_in("php-8.3.6-Win32-vs16-x64"), version_in("8.3.6"));
        assert_eq!(version_in("v18.20.4"), version_in("18.20.4"));
        assert_eq!(version_in("php74"), None, "no separator means no version");
        assert_eq!(
            version_in("8.1"),
            version_in("8.1.0"),
            "a short version is padded"
        );
        assert_eq!(version_in("go1.24.3"), version_in("1.24.3"));
        assert_eq!(version_in("active"), None);
    }

    #[test]
    fn versions_are_compared_as_numbers_not_as_text() {
        let choices = installed(&["8.9.0", "8.10.0"]);
        let chosen = select(&choices, "^8.0").expect("something matches");
        assert_eq!(
            chosen.label, "8.10.0",
            "sorting these as strings picks 8.9, which is how a version switcher \
             quietly runs the wrong one"
        );
    }

    #[test]
    fn a_caret_constraint_stays_inside_its_major() {
        let choices = installed(&["7.4.33", "8.1.2", "8.3.6"]);
        assert_eq!(select(&choices, "^7.4").unwrap().label, "7.4.33");
        assert_eq!(select(&choices, "^8.0").unwrap().label, "8.3.6");
    }

    #[test]
    fn the_highest_version_that_satisfies_wins_rather_than_the_first_found() {
        let choices = installed(&["8.1.2", "8.2.10", "8.3.6"]);
        assert_eq!(select(&choices, ">=8.1").unwrap().label, "8.3.6");
    }

    #[test]
    fn composer_style_constraints_are_understood() {
        let choices = installed(&["5.6.40", "7.4.33", "8.1.2", "8.3.6"]);
        assert_eq!(select(&choices, ">=7.4 <8.0").unwrap().label, "7.4.33");
        assert_eq!(select(&choices, "8.*").unwrap().label, "8.3.6");
        assert_eq!(select(&choices, "~7.4.0").unwrap().label, "7.4.33");
        assert_eq!(select(&choices, "5.6.*").unwrap().label, "5.6.40");
    }

    #[test]
    fn an_alternatives_constraint_accepts_either_side() {
        let choices = installed(&["7.4.33", "8.3.6"]);
        assert_eq!(
            select(&choices, "^7.4 || ^8.0").unwrap().label,
            "8.3.6",
            "when both branches match, the higher version is still the answer"
        );
        let only_old = installed(&["7.4.33"]);
        assert_eq!(select(&only_old, "^7.4 || ^8.0").unwrap().label, "7.4.33");
    }

    #[test]
    fn node_style_constraints_are_understood_too() {
        let choices = installed(&["18.20.4", "20.11.1", "22.3.0"]);
        assert_eq!(select(&choices, ">=18").unwrap().label, "22.3.0");
        assert_eq!(select(&choices, "18.x").unwrap().label, "18.20.4");
        assert_eq!(select(&choices, "20.11.1").unwrap().label, "20.11.1");
    }

    #[test]
    fn nothing_installed_satisfies_a_constraint_nothing_can_satisfy() {
        let choices = installed(&["8.1.2", "8.3.6"]);
        assert_eq!(
            select(&choices, "^5.6"),
            None,
            "reporting the nearest version instead would silently run the wrong one"
        );
    }

    #[test]
    fn a_constraint_nobody_can_parse_selects_nothing_rather_than_guessing() {
        let choices = installed(&["8.3.6"]);
        assert_eq!(select(&choices, "whatever the latest is"), None);
        assert_eq!(select(&choices, ""), None);
    }

    #[test]
    fn with_no_constraint_at_all_the_newest_is_the_reasonable_default() {
        let choices = installed(&["7.4.33", "8.3.6", "8.1.2"]);
        assert_eq!(newest(&choices).unwrap().label, "8.3.6");
        assert_eq!(newest(&[]), None);
    }

    #[test]
    fn a_php_requirement_is_read_from_composer() {
        let composer = r#"{"require":{"php":"^8.1","laravel/framework":"^11.0"}}"#;
        assert_eq!(wanted_php(composer).as_deref(), Some("^8.1"));
        assert_eq!(wanted_php(r#"{"require":{}}"#), None);
        assert_eq!(wanted_php("not json"), None);
    }

    #[test]
    fn a_node_requirement_is_read_from_the_engines_field() {
        let package = r#"{"engines":{"node":">=18"},"dependencies":{}}"#;
        assert_eq!(wanted_node(package).as_deref(), Some(">=18"));
        assert_eq!(wanted_node(r#"{"dependencies":{"next":"14"}}"#), None);
    }

    use std::fs;
    use tempfile::{tempdir, TempDir};

    /// A directory laid out the way a version manager leaves one.
    fn a_php_installation() -> TempDir {
        let dir = tempdir().unwrap();
        for name in [
            "php-7.4.33-Win32",
            "php-8.3.6-Win32",
            "not-a-version",
            "active",
        ] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
            fs::write(dir.path().join(name).join("php.exe"), "").unwrap();
        }
        // A directory that looks like a version but holds no binary.
        fs::create_dir_all(dir.path().join("php-9.9.9-broken")).unwrap();
        dir
    }

    fn php_config(search: &Path) -> ToolConfig {
        ToolConfig {
            search: vec![search.to_path_buf()],
            binary: "php.exe".to_string(),
            bin_subdir: None,
            versions: HashMap::new(),
        }
    }

    #[test]
    fn every_directory_holding_the_binary_and_naming_a_version_is_found() {
        let dir = a_php_installation();
        let found = discover(&php_config(dir.path()));
        let labels: Vec<&str> = found.iter().map(|f| f.label.as_str()).collect();
        assert!(labels.contains(&"php-7.4.33-Win32"));
        assert!(labels.contains(&"php-8.3.6-Win32"));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn a_directory_that_names_no_version_is_left_out_rather_than_guessed_at() {
        let dir = a_php_installation();
        let found = discover(&php_config(dir.path()));
        assert!(
            !found
                .iter()
                .any(|f| f.label == "active" || f.label == "not-a-version"),
            "an 'active' symlink points at one of the others; counting it twice \
             would offer the same install under two names"
        );
    }

    #[test]
    fn a_version_directory_without_the_binary_is_not_offered() {
        let dir = a_php_installation();
        let found = discover(&php_config(dir.path()));
        assert!(
            !found.iter().any(|f| f.label.contains("broken")),
            "offering a version whose binary is missing moves the failure to \
             the moment somebody tries to use it"
        );
    }

    #[test]
    fn a_search_path_that_does_not_exist_is_skipped_rather_than_fatal() {
        let config = ToolConfig {
            search: vec![
                PathBuf::from("nowhere-9f2a1c4"),
                PathBuf::from("also-nowhere"),
            ],
            binary: "php.exe".to_string(),
            bin_subdir: None,
            versions: HashMap::new(),
        };
        assert!(discover(&config).is_empty());
    }

    #[test]
    fn a_version_named_explicitly_is_taken_even_when_the_directory_says_nothing() {
        let dir = a_php_installation();
        let mut config = php_config(dir.path());
        config
            .versions
            .insert("5.6.40".to_string(), dir.path().join("not-a-version"));
        let found = discover(&config);
        assert!(
            found
                .iter()
                .any(|f| f.version.major == 5 && f.version.minor == 6),
            "a directory named anything at all can still be pointed at by hand"
        );
    }

    #[test]
    fn a_tool_whose_binary_sits_in_a_subdirectory_is_still_found() {
        let dir = tempdir().unwrap();
        let go = dir.path().join("go1.24.3");
        fs::create_dir_all(go.join("bin")).unwrap();
        fs::write(go.join("bin").join("go.exe"), "").unwrap();

        let config = ToolConfig {
            search: vec![dir.path().to_path_buf()],
            binary: "go.exe".to_string(),
            bin_subdir: Some("bin".to_string()),
            versions: HashMap::new(),
        };
        let found = discover(&config);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].path,
            go.join("bin"),
            "PATH wants the directory the binary is in, not the one above it"
        );
    }

    #[test]
    fn a_pin_wins_over_whatever_the_manifest_asks_for() {
        let choices = installed(&["5.6.40", "8.3.6"]);
        let resolved = resolve(&choices, Some("5.6"), Some("^8.0"));
        assert_eq!(resolved.chosen.map(|c| c.label), Some("5.6.40".to_string()));
        assert_eq!(resolved.reason, Reason::Pinned);
    }

    #[test]
    fn without_a_pin_the_manifest_decides() {
        let choices = installed(&["7.4.33", "8.3.6"]);
        let resolved = resolve(&choices, None, Some("^7.4"));
        assert_eq!(resolved.chosen.map(|c| c.label), Some("7.4.33".to_string()));
        assert_eq!(resolved.reason, Reason::Declared);
    }

    #[test]
    fn a_project_that_asks_for_nothing_gets_the_newest_installed() {
        let choices = installed(&["7.4.33", "8.3.6"]);
        let resolved = resolve(&choices, None, None);
        assert_eq!(resolved.chosen.map(|c| c.label), Some("8.3.6".to_string()));
        assert_eq!(resolved.reason, Reason::Newest);
    }

    #[test]
    fn a_constraint_nothing_installed_can_satisfy_chooses_nothing_and_says_why() {
        let choices = installed(&["8.1.2", "8.3.6"]);
        let resolved = resolve(&choices, None, Some("^5.6"));
        assert!(
            resolved.chosen.is_none(),
            "running a legacy project on an interpreter it said it cannot use is \
             the failure this whole feature exists to prevent"
        );
        assert_eq!(resolved.reason, Reason::NothingSatisfies);
        assert_eq!(resolved.constraint.as_deref(), Some("^5.6"));
    }

    #[test]
    fn nothing_installed_at_all_is_told_apart_from_nothing_matching() {
        let resolved = resolve(&[], None, Some("^8.0"));
        assert_eq!(
            resolved.reason,
            Reason::NoneInstalled,
            "'you have none' and 'yours are all wrong' need different fixes"
        );
    }

    #[test]
    fn a_bare_version_means_that_version_and_not_anything_newer() {
        let choices = installed(&["8.1.2", "8.3.6"]);
        assert_eq!(
            select(&choices, "8.1").unwrap().label,
            "8.1.2",
            "composer and npm both read a bare 8.1 as 8.1.x; pinning 8.1 and being \
             handed 8.3 is the switcher quietly running the wrong interpreter"
        );
        assert_eq!(
            select(&choices, "8").unwrap().label,
            "8.3.6",
            "a bare major is still satisfied by any minor inside it"
        );
        assert_eq!(select(&choices, "8.3.6").unwrap().label, "8.3.6");
    }

    #[test]
    fn an_operator_still_means_what_it_says() {
        let choices = installed(&["8.1.2", "8.3.6"]);
        assert_eq!(
            select(&choices, "^8.1").unwrap().label,
            "8.3.6",
            "a caret was always allowed to move up within the major"
        );
        assert_eq!(select(&choices, ">=8.1").unwrap().label, "8.3.6");
    }
}
