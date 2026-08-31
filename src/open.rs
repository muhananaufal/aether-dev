//! Handing something to the rest of the desktop: a URL to a browser, a
//! directory to a file manager.
//!
//! Each platform has an obvious default and every one of them is wrong for
//! somebody - a different browser, a file manager that is not the one the
//! desktop ships, a terminal file manager inside the same window. The default
//! is a starting point rather than a rule, so the command is a template the
//! user can replace.

use serde::Deserialize;

/// The placeholder replaced by whatever is being opened. Without it the target
/// is appended, which is what almost every opener wants.
pub const TARGET: &str = "{}";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OpenConfig {
    /// The command that opens a URL.
    pub browser: Vec<String>,
    /// The command that opens a directory.
    pub file_manager: Vec<String>,
}

impl Default for OpenConfig {
    fn default() -> Self {
        Self {
            browser: default_browser(),
            file_manager: default_file_manager(),
        }
    }
}

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

/// What opens a URL on this platform when nobody has said otherwise.
pub fn default_browser() -> Vec<String> {
    #[cfg(windows)]
    {
        // The empty argument is the window title `start` expects; without it
        // the URL is taken as the title and nothing opens.
        words(&["cmd", "/C", "start", "", TARGET])
    }
    #[cfg(target_os = "macos")]
    {
        words(&["open", TARGET])
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        words(&["xdg-open", TARGET])
    }
}

/// What opens a directory on this platform when nobody has said otherwise.
pub fn default_file_manager() -> Vec<String> {
    #[cfg(windows)]
    {
        words(&["explorer", TARGET])
    }
    #[cfg(target_os = "macos")]
    {
        words(&["open", TARGET])
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        words(&["xdg-open", TARGET])
    }
}

/// Builds the command line that opens `target`.
///
/// Returns `None` for an empty template, which is how a user says "do not open
/// anything" - a deliberate choice that should not be answered by falling back
/// to a default they had just removed.
pub fn command_line(template: &[String], target: &str) -> Option<Vec<String>> {
    let (program, rest) = template.split_first()?;
    if program.trim().is_empty() {
        return None;
    }

    let mut line = vec![program.clone()];
    let mut substituted = false;
    for argument in rest {
        if argument.contains(TARGET) {
            line.push(argument.replace(TARGET, target));
            substituted = true;
        } else {
            line.push(argument.clone());
        }
    }
    // A template that never mentions the target still means to open it. The
    // alternative is a command that silently opens nothing.
    if !substituted {
        line.push(target.to_string());
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_placeholder_is_replaced_by_the_target() {
        let line = command_line(&words(&["firefox", TARGET]), "http://localhost:8000").unwrap();
        assert_eq!(line, vec!["firefox", "http://localhost:8000"]);
    }

    #[test]
    fn a_template_that_never_mentions_the_target_still_opens_it() {
        let line = command_line(&words(&["xdg-open"]), "/home/dev/shop").unwrap();
        assert_eq!(
            line,
            vec!["xdg-open", "/home/dev/shop"],
            "a command that opens nothing is not a reasonable reading of the config"
        );
    }

    #[test]
    fn the_placeholder_may_sit_inside_a_larger_argument() {
        let line = command_line(&words(&["code", "--folder-uri=file://{}"]), "/srv/app").unwrap();
        assert_eq!(line, vec!["code", "--folder-uri=file:///srv/app"]);
    }

    #[test]
    fn arguments_before_the_target_keep_their_order() {
        let line = command_line(&words(&["cmd", "/C", "start", "", TARGET]), "http://x").unwrap();
        assert_eq!(line, vec!["cmd", "/C", "start", "", "http://x"]);
    }

    #[test]
    fn an_empty_template_means_do_not_open_anything() {
        assert_eq!(command_line(&[], "http://x"), None);
        assert_eq!(command_line(&words(&[" "]), "http://x"), None);
    }

    #[test]
    fn every_platform_default_names_a_program_and_opens_the_target() {
        for template in [default_browser(), default_file_manager()] {
            let line = command_line(&template, "TARGET-VALUE")
                .expect("a built-in default that opens nothing would be a bug");
            assert!(!line[0].is_empty());
            assert!(
                line.iter()
                    .any(|argument| argument.contains("TARGET-VALUE")),
                "got {line:?}"
            );
        }
    }

    #[test]
    fn the_defaults_are_what_an_unconfigured_machine_gets() {
        let config = OpenConfig::default();
        assert_eq!(config.browser, default_browser());
        assert_eq!(config.file_manager, default_file_manager());
    }
}
