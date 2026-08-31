//! What the container host is costing this machine.
//!
//! Two numbers, because on Windows they are genuinely different things: how
//! much memory is in use inside the virtual machine docker runs in, and how
//! much that virtual machine is costing Windows. The second is usually the one
//! that explains why the laptop is struggling, and it is invisible from inside.
//!
//! Both are read by running a command and parsing its output. The commands are
//! configurable because the distribution name, the process name and even
//! whether there is a virtual machine at all differ per machine - on native
//! Linux docker there is no second number to show.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MemoryConfig {
    /// How often the numbers are re-read, in seconds. Zero turns the whole
    /// thing off.
    pub interval_secs: u64,
    /// A command whose output is read as `free -m`: memory in use inside
    /// whatever docker runs in.
    pub guest: Vec<String>,
    /// Process names whose memory is summed to show what that costs this
    /// machine. Empty where the question does not apply.
    pub host_process: Vec<String>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            interval_secs: 5,
            guest: default_guest(),
            host_process: default_host_process(),
        }
    }
}

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

fn default_guest() -> Vec<String> {
    #[cfg(windows)]
    {
        // No distribution is named: `wsl` uses the default one, and naming
        // somebody else's would be a fact about one machine.
        words(&["wsl", "free", "-m"])
    }
    #[cfg(not(windows))]
    {
        words(&["free", "-m"])
    }
}

fn default_host_process() -> Vec<String> {
    #[cfg(windows)]
    {
        // WSL 2 names it one way, the older Hyper-V backend the other.
        words(&["vmmemWSL", "vmmem"])
    }
    #[cfg(not(windows))]
    {
        // Docker Desktop for macOS has a backing process too, but it is named
        // differently between versions and guessing wrong shows a confident
        // zero. Left empty until somebody sets it.
        Vec::new()
    }
}

/// What the two probes came back with. Either can be absent without the other
/// being useless.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    /// Bytes in use inside the machine docker runs in.
    pub guest_bytes: Option<u64>,
    /// The process that backs it, and what it costs this machine.
    pub host: Option<(String, u64)>,
}

impl Reading {
    pub fn is_empty(&self) -> bool {
        self.guest_bytes.is_none() && self.host.is_none()
    }
}

/// Reads the "used" column from `free -m` output.
///
/// The second column is total and the third is used; a `free` that prints
/// fewer than three is not one this can read, and says so rather than
/// reporting whichever number happened to be there.
pub fn parse_free_used(output: &str) -> Option<u64> {
    let line = output
        .lines()
        .find(|line| line.trim_start().starts_with("Mem:"))?;
    let mut fields = line.split_whitespace();
    let _label = fields.next()?;
    let _total = fields.next()?;
    let used: u64 = fields.next()?.parse().ok()?;
    Some(used * 1024 * 1024)
}

/// Sums the memory of the named processes from `tasklist /FO CSV /NH` output.
///
/// The size field is grouped for display, and which character does the
/// grouping depends on the machine's locale - this one writes 855.312 K where
/// another writes 855,312 K. Every character that is not a digit is dropped,
/// which is the only reading that survives both.
pub fn parse_tasklist(output: &str, wanted: &[String]) -> Option<(String, u64)> {
    let mut found: Option<(String, u64)> = None;
    for line in output.lines() {
        let fields: Vec<&str> = line.split("\",\"").collect();
        if fields.len() < 5 {
            continue;
        }
        let name = fields[0].trim_start_matches('"').trim();
        if !wanted.iter().any(|want| want.eq_ignore_ascii_case(name)) {
            continue;
        }
        let digits: String = fields[4].chars().filter(char::is_ascii_digit).collect();
        let Ok(kilobytes) = digits.parse::<u64>() else {
            continue;
        };
        let bytes = kilobytes * 1024;
        found = Some(match found {
            // Several processes of the same name are one cost, not two rows.
            Some((existing, total)) => (existing, total + bytes),
            None => (name.to_string(), bytes),
        });
    }
    found
}

/// Runs both probes. Anything that fails - no WSL, no such process, a command
/// that is not installed - is simply absent from the reading; the footer is
/// not somewhere to report that a probe did not apply to this machine.
pub fn read(config: &MemoryConfig) -> Reading {
    Reading {
        guest_bytes: config
            .guest
            .split_first()
            .and_then(|(program, arguments)| output_of(program, arguments))
            .as_deref()
            .and_then(parse_free_used),
        host: if config.host_process.is_empty() {
            None
        } else {
            output_of(
                "tasklist",
                &["/FO".to_string(), "CSV".to_string(), "/NH".to_string()],
            )
            .as_deref()
            .and_then(|output| parse_tasklist(output, &config.host_process))
        },
    }
}

fn output_of(program: &str, arguments: &[String]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_used_column_is_read_from_free_output() {
        let output = "               total        used        free      shared  buff/cache   available\n\
                      Mem:            7946        1423        5367           3        1155        6276\n\
                      Swap:           4096           0        4096\n";
        assert_eq!(parse_free_used(output), Some(1423 * 1024 * 1024));
    }

    #[test]
    fn free_output_without_a_mem_line_reports_nothing_rather_than_a_guess() {
        assert_eq!(parse_free_used("wsl: no distribution\n"), None);
        assert_eq!(parse_free_used(""), None);
        assert_eq!(parse_free_used("Mem:  7946\n"), None);
    }

    #[test]
    fn a_grouping_separator_does_not_change_the_number() {
        let wanted = vec!["vmmemWSL".to_string()];
        let dots = "\"vmmemWSL\",\"16880\",\"Services\",\"0\",\"855.312 K\"\n";
        let commas = "\"vmmemWSL\",\"16880\",\"Services\",\"0\",\"855,312 K\"\n";
        assert_eq!(
            parse_tasklist(dots, &wanted),
            parse_tasklist(commas, &wanted),
            "which character groups the digits is a locale setting, not a different number"
        );
        assert_eq!(
            parse_tasklist(dots, &wanted),
            Some(("vmmemWSL".to_string(), 855_312 * 1024))
        );
    }

    #[test]
    fn processes_sharing_a_name_are_summed_into_one_cost() {
        let wanted = vec!["vmmem".to_string()];
        let output = "\"vmmem\",\"1\",\"Services\",\"0\",\"100 K\"\n\
                      \"vmmem\",\"2\",\"Services\",\"0\",\"200 K\"\n";
        assert_eq!(
            parse_tasklist(output, &wanted),
            Some(("vmmem".to_string(), 300 * 1024))
        );
    }

    #[test]
    fn a_process_nobody_asked_about_is_not_counted() {
        let wanted = vec!["vmmemWSL".to_string()];
        let output = "\"chrome\",\"1\",\"Console\",\"1\",\"900.000 K\"\n\
                      \"vmmemWSL\",\"2\",\"Services\",\"0\",\"100 K\"\n";
        assert_eq!(
            parse_tasklist(output, &wanted),
            Some(("vmmemWSL".to_string(), 100 * 1024))
        );
    }

    #[test]
    fn the_first_name_that_matches_is_the_one_reported() {
        // vmmemWSL and vmmem are two names for the same thing on different
        // backends; whichever exists is the answer.
        let wanted = vec!["vmmemWSL".to_string(), "vmmem".to_string()];
        let output = "\"vmmem\",\"2\",\"Services\",\"0\",\"512 K\"\n";
        assert_eq!(
            parse_tasklist(output, &wanted),
            Some(("vmmem".to_string(), 512 * 1024))
        );
    }

    #[test]
    fn no_matching_process_reports_nothing() {
        assert_eq!(
            parse_tasklist("INFO: No tasks are running.\n", &["vmmem".to_string()]),
            None
        );
        assert_eq!(parse_tasklist("", &["vmmem".to_string()]), None);
    }

    #[test]
    fn a_reading_with_neither_number_knows_it_is_empty() {
        assert!(Reading::default().is_empty());
        assert!(!Reading {
            guest_bytes: Some(1),
            host: None
        }
        .is_empty());
    }

    #[test]
    fn the_defaults_poll_and_name_a_command_to_poll_with() {
        let config = MemoryConfig::default();
        assert!(config.interval_secs > 0);
        assert!(
            !config.guest.is_empty(),
            "a default that reads nothing would show an empty footer forever"
        );
    }
}
