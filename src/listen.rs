//! Which process is holding a port.
//!
//! The predecessor answered this and it is the question a developer actually
//! asks: not "what has docker published" but "what is on 8000, and can I have
//! it back". Docker's own ports are only part of the answer.
//!
//! No crate for it. The one that reads sockets directly pulls in the whole
//! Windows API bindings, which failed to compile here for want of memory - a
//! steep price for a list of numbers. Instead the system's own tools are asked
//! and their output is parsed, which keeps the parsing pure and testable
//! against real captures.

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listener {
    pub port: u16,
    pub pid: Option<u32>,
    pub process: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ListenError {
    #[error("cannot ask this system what is listening: {0}")]
    Unavailable(String),
    #[error("listing ports is not implemented for this platform yet")]
    Unsupported,
}

/// Reads `netstat -ano -p TCP`, the shape Windows prints.
///
/// A port listening on both address families is one service, so it appears
/// once: listing it twice makes a short list look long and a duplicate look
/// like a conflict.
pub fn parse_netstat(output: &str) -> Vec<Listener> {
    let mut found: Vec<Listener> = Vec::new();

    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 5 || !fields[0].eq_ignore_ascii_case("tcp") {
            continue;
        }
        if !fields[3].eq_ignore_ascii_case("listening") {
            continue;
        }
        let Some(port) = port_of(fields[1]) else {
            continue;
        };
        let pid = fields[4].parse::<u32>().ok();
        if found
            .iter()
            .any(|seen| seen.port == port && seen.pid == pid)
        {
            continue;
        }
        found.push(Listener {
            port,
            pid,
            process: None,
        });
    }

    found.sort_by_key(|listener| listener.port);
    found
}

/// Reads `tasklist /NH /FO CSV` into a lookup from number to name.
pub fn parse_tasklist(output: &str) -> HashMap<u32, String> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split("\",\"");
            let name = fields.next()?.trim_start_matches('"');
            let pid = fields.next()?.trim_matches('"').parse::<u32>().ok()?;
            Some((pid, name.to_string()))
        })
        .collect()
}

/// Reads `ss -lntpH`, which unlike netstat already knows the name.
pub fn parse_ss(output: &str) -> Vec<Listener> {
    let mut found: Vec<Listener> = Vec::new();

    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let Some(port) = port_of(fields[3]) else {
            continue;
        };
        // A socket with no owner still holds the port; it just cannot say who.
        let owner = line.split_once("users:((\"").and_then(|(_, rest)| {
            let (name, rest) = rest.split_once('"')?;
            let pid = rest
                .split_once("pid=")
                .and_then(|(_, tail)| tail.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|digits| digits.parse::<u32>().ok());
            Some((name.to_string(), pid))
        });

        if found.iter().any(|seen| seen.port == port) {
            continue;
        }
        found.push(Listener {
            port,
            pid: owner.as_ref().and_then(|(_, pid)| *pid),
            process: owner.map(|(name, _)| name),
        });
    }

    found.sort_by_key(|listener| listener.port);
    found
}

/// Fills in the names for the numbers that have one. A pid the process list
/// did not report stays a number rather than becoming a guess.
pub fn attach_names(listeners: &mut [Listener], names: &HashMap<u32, String>) {
    for listener in listeners {
        if let Some(pid) = listener.pid {
            listener.process = names.get(&pid).cloned();
        }
    }
}

/// The port out of `0.0.0.0:8000`, `[::]:445` or `*:8080`.
fn port_of(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok()
}

/// Asks this system what is listening, using its own tools.
pub fn listening() -> Result<Vec<Listener>, ListenError> {
    #[cfg(windows)]
    {
        let sockets = capture("netstat", &["-ano", "-p", "TCP"])?;
        let mut listeners = parse_netstat(&sockets);
        // A missing tasklist costs the names, not the ports, so it is not
        // allowed to fail the whole answer.
        if let Ok(processes) = capture("tasklist", &["/NH", "/FO", "CSV"]) {
            attach_names(&mut listeners, &parse_tasklist(&processes));
        }
        Ok(listeners)
    }
    #[cfg(target_os = "linux")]
    {
        Ok(parse_ss(&capture("ss", &["-lntpH"])?))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        // Rather than ship a parser for a format nobody here can check
        // against a real machine.
        Err(ListenError::Unsupported)
    }
}

/// Ends a process. Refuses the handful of numbers that are the operating
/// system itself, because "free this port" should never mean "reboot".
pub fn terminate(pid: u32) -> Result<(), ListenError> {
    if pid <= 4 {
        return Err(ListenError::Unavailable(format!(
            "{pid} belongs to the operating system"
        )));
    }

    #[cfg(windows)]
    let outcome = capture("taskkill", &["/PID", &pid.to_string(), "/F"]);
    #[cfg(not(windows))]
    let outcome = capture("kill", &["-TERM", &pid.to_string()]);

    outcome.map(|_| ())
}

fn capture(program: &str, args: &[&str]) -> Result<String, ListenError> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|error| ListenError::Unavailable(format!("{program}: {error}")))?;

    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ListenError::Unavailable(format!(
            "{program} exited {}: {reason}",
            output.status.code().unwrap_or(-1)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Taken from this machine's `netstat -ano -p TCP`.
    const WINDOWS_NETSTAT: &str = "\
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:22             0.0.0.0:0              LISTENING       4764
  TCP    0.0.0.0:445            0.0.0.0:0              LISTENING       4
  TCP    127.0.0.1:2375         0.0.0.0:0              LISTENING       9128
  TCP    0.0.0.0:8000           0.0.0.0:0              LISTENING       16784
  TCP    192.168.1.5:52233      52.1.2.3:443           ESTABLISHED     3312
  TCP    [::]:445               [::]:0                 LISTENING       4
";

    /// Taken from this machine's `tasklist /NH /FO CSV`.
    const WINDOWS_TASKLIST: &str = "\
\"System Idle Process\",\"0\",\"Services\",\"0\",\"8 K\"
\"System\",\"4\",\"Services\",\"0\",\"168 K\"
\"php.exe\",\"16784\",\"Console\",\"1\",\"42.000 K\"
\"com.docker.backend.exe\",\"9128\",\"Console\",\"1\",\"90.000 K\"
";

    /// Taken from `ss -lntpH` inside this machine's WSL.
    const LINUX_SS: &str = "\
LISTEN 0      4096          0.0.0.0:443   0.0.0.0:* users:((\"docker-proxy\",pid=1172,fd=9))
LISTEN 0      4096          0.0.0.0:80    0.0.0.0:* users:((\"docker-proxy\",pid=1145,fd=9))
LISTEN 0      4096        127.0.0.1:2375  0.0.0.0:* users:((\"socat\",pid=1205,fd=5))
LISTEN 0      511                 *:8080  *:*
";

    #[test]
    fn only_listening_sockets_are_taken_from_netstat() {
        let listeners = parse_netstat(WINDOWS_NETSTAT);
        let ports: Vec<u16> = listeners.iter().map(|l| l.port).collect();
        assert!(ports.contains(&8000) && ports.contains(&2375));
        assert!(
            !ports.contains(&52233),
            "an established connection is not something holding a port"
        );
    }

    #[test]
    fn a_port_listening_on_two_address_families_is_reported_once() {
        let listeners = parse_netstat(WINDOWS_NETSTAT);
        assert_eq!(
            listeners.iter().filter(|l| l.port == 445).count(),
            1,
            "0.0.0.0:445 and [::]:445 are one service, and listing it twice \
             makes a short list look long"
        );
    }

    #[test]
    fn the_owning_process_is_carried_through_from_netstat() {
        let listeners = parse_netstat(WINDOWS_NETSTAT);
        let php = listeners.iter().find(|l| l.port == 8000).expect("8000");
        assert_eq!(php.pid, Some(16784));
        assert_eq!(php.process, None, "netstat only knows the number");
    }

    #[test]
    fn tasklist_supplies_the_name_the_number_alone_does_not() {
        let names = parse_tasklist(WINDOWS_TASKLIST);
        assert_eq!(names.get(&16784).map(String::as_str), Some("php.exe"));
        assert_eq!(
            names.get(&9128).map(String::as_str),
            Some("com.docker.backend.exe")
        );
        assert_eq!(names.get(&1).map(String::as_str), None);
    }

    #[test]
    fn a_name_is_attached_to_the_port_it_belongs_to() {
        let mut listeners = parse_netstat(WINDOWS_NETSTAT);
        attach_names(&mut listeners, &parse_tasklist(WINDOWS_TASKLIST));
        let php = listeners.iter().find(|l| l.port == 8000).expect("8000");
        assert_eq!(php.process.as_deref(), Some("php.exe"));

        let unknown = listeners.iter().find(|l| l.port == 22).expect("22");
        assert_eq!(
            unknown.process, None,
            "a pid tasklist did not report stays a number rather than becoming a guess"
        );
    }

    #[test]
    fn ss_carries_the_name_and_the_number_together() {
        let listeners = parse_ss(LINUX_SS);
        let socat = listeners.iter().find(|l| l.port == 2375).expect("2375");
        assert_eq!(socat.process.as_deref(), Some("socat"));
        assert_eq!(socat.pid, Some(1205));
    }

    #[test]
    fn a_listener_ss_reports_without_an_owner_is_still_a_listener() {
        let listeners = parse_ss(LINUX_SS);
        let anonymous = listeners.iter().find(|l| l.port == 8080).expect("8080");
        assert_eq!(anonymous.process, None);
        assert_eq!(anonymous.pid, None);
    }

    #[test]
    fn ports_come_back_in_order_so_the_list_reads_the_same_way_twice() {
        let listeners = parse_ss(LINUX_SS);
        let ports: Vec<u16> = listeners.iter().map(|l| l.port).collect();
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        assert_eq!(ports, sorted);
    }

    #[test]
    fn nothing_listening_is_an_empty_list_rather_than_a_failure() {
        assert!(parse_netstat("").is_empty());
        assert!(parse_ss("").is_empty());
        assert!(parse_tasklist("").is_empty());
    }
}
