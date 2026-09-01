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

use serde::Deserialize;
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

/// Reads `netstat -lntp` as Linux prints it, which is not how Windows prints
/// it: seven columns instead of five, the state spelled LISTEN rather than
/// LISTENING, and the owner given as `1085/docker-proxy` in one field instead
/// of a bare number.
///
/// Worth having as well as `parse_ss` because `ss` comes from iproute2, which
/// slim images and older distributions often leave out - and a machine without
/// it is exactly the machine where "what is on 8000" cannot be answered any
/// other way.
pub fn parse_linux_netstat(output: &str) -> Vec<Listener> {
    let mut found: Vec<Listener> = Vec::new();

    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // tcp6 as well as tcp: a socket bound to :: holds the port for
        // everything that reaches it, whichever family the caller uses.
        if fields.len() < 6 || !(fields[0] == "tcp" || fields[0] == "tcp6") {
            continue;
        }
        if fields[5] != "LISTEN" {
            continue;
        }
        let Some(port) = port_of(fields[3]) else {
            continue;
        };
        if found.iter().any(|seen| seen.port == port) {
            continue;
        }

        // "1085/docker-proxy", or "-" when netstat was not run as root and
        // will not say. A dash is an absence, not a process of that name.
        let owner = fields.get(6).filter(|owner| **owner != "-");
        let (pid, process) = match owner.and_then(|owner| owner.split_once('/')) {
            Some((pid, name)) => (pid.parse::<u32>().ok(), Some(name.to_string())),
            None => (None, None),
        };

        found.push(Listener { port, pid, process });
    }

    found.sort_by_key(|listener| listener.port);
    found
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

/// The shape of a tool's output, which is a separate fact from which tool
/// produced it.
///
/// A probe has to name one. Telling this only which command to run would let a
/// wrong pairing return an empty list rather than an error, and an empty list
/// is indistinguishable from a machine with nothing listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    /// `netstat -ano -p TCP` on Windows.
    WindowsNetstat,
    /// `netstat -lntp` on Linux, which prints neither the same columns nor
    /// the same words.
    LinuxNetstat,
    /// `ss -lntpH`, from iproute2.
    Ss,
}

/// One way of asking what is listening.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub command: Vec<String>,
    pub format: Format,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PortsConfig {
    /// Tried in order until one of them answers. More than one entry is how a
    /// machine without iproute2 still gets an answer.
    pub probe: Vec<Probe>,
    /// The process list, for the formats that give a pid but no name. Empty
    /// where the probe already carries names, and never fatal: losing the
    /// names costs less than losing the ports.
    pub names: Vec<String>,
}

impl Default for PortsConfig {
    fn default() -> Self {
        #[cfg(windows)]
        {
            Self {
                probe: vec![Probe {
                    command: words(&["netstat", "-ano", "-p", "TCP"]),
                    format: Format::WindowsNetstat,
                }],
                names: words(&["tasklist", "/NH", "/FO", "CSV"]),
            }
        }
        #[cfg(target_os = "linux")]
        {
            Self {
                // ss first because it names the process itself. netstat after
                // it, because iproute2 is missing often enough that a machine
                // without it should still get an answer.
                probe: vec![
                    Probe {
                        command: words(&["ss", "-lntpH"]),
                        format: Format::Ss,
                    },
                    Probe {
                        command: words(&["netstat", "-lntp"]),
                        format: Format::LinuxNetstat,
                    },
                ],
                names: Vec::new(),
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            // Nothing, rather than a parser for a format nobody here has ever
            // captured from a real machine. The error says where to set one,
            // which is more use than a confident wrong answer.
            Self {
                probe: Vec::new(),
                names: Vec::new(),
            }
        }
    }
}

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

/// Reads output known to be in `format`.
pub fn read(output: &str, format: Format) -> Vec<Listener> {
    match format {
        Format::WindowsNetstat => parse_netstat(output),
        Format::LinuxNetstat => parse_linux_netstat(output),
        Format::Ss => parse_ss(output),
    }
}

/// Asks each configured probe in turn, and takes the first answer.
pub fn listening(config: &PortsConfig) -> Result<Vec<Listener>, ListenError> {
    if config.probe.is_empty() {
        return Err(ListenError::Unavailable(
            "nothing is configured to list ports on this platform; set [ports].probe".to_string(),
        ));
    }

    let mut refused = Vec::new();
    for probe in &config.probe {
        let Some((program, arguments)) = probe.command.split_first() else {
            refused.push("a probe with no command in it".to_string());
            continue;
        };
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        match capture(program, &arguments) {
            Ok(output) => {
                let mut listeners = read(&output, probe.format);
                if !config.names.is_empty() {
                    if let Some((program, arguments)) = config.names.split_first() {
                        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
                        // A missing process list costs the names, not the
                        // ports, so it is not allowed to fail the answer.
                        if let Ok(processes) = capture(program, &arguments) {
                            attach_names(&mut listeners, &parse_tasklist(&processes));
                        }
                    }
                }
                return Ok(listeners);
            }
            // Kept rather than replaced: with only the last one shown, a
            // machine missing every tool cannot be told from one where the
            // first tool merely errored. The inner text only, because the
            // wrapper below says once what each of these would repeat.
            Err(ListenError::Unavailable(reason)) => refused.push(reason),
            Err(other) => refused.push(other.to_string()),
        }
    }

    Err(ListenError::Unavailable(format!(
        "nothing could list the ports: {}",
        refused.join("; ")
    )))
}

/// Ends a process. Refuses the handful of numbers that are the operating
/// system itself, because "free this port" should never mean "reboot".
pub fn terminate(command: &[String], pid: u32) -> Result<(), ListenError> {
    if pid <= 4 {
        return Err(ListenError::Unavailable(format!(
            "{pid} belongs to the operating system"
        )));
    }
    // An empty command means somebody deliberately removed it. Falling back to
    // a default they had just deleted would be the opposite of obeying them.
    let Some((program, arguments)) = command.split_first() else {
        return Err(ListenError::Unavailable(
            "nothing is configured to end a process; set [tools].kill".to_string(),
        ));
    };

    let pid = pid.to_string();
    let arguments: Vec<String> = arguments
        .iter()
        .map(|argument| argument.replace(PID, &pid))
        .collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    capture(program, &arguments).map(|_| ())
}

/// The placeholder a kill command puts the process number in.
pub const PID: &str = "{pid}";

/// What ends a process on this platform, when nobody has said otherwise.
pub fn default_kill() -> Vec<String> {
    #[cfg(windows)]
    {
        words(&["taskkill", "/PID", PID, "/F"])
    }
    #[cfg(not(windows))]
    {
        words(&["kill", "-TERM", PID])
    }
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

    #[test]
    fn each_format_is_read_by_the_parser_that_understands_it() {
        // Four, not five: the Windows capture lists 445 on both address
        // families and one service holding one port is one row.
        assert_eq!(read(WINDOWS_NETSTAT, Format::WindowsNetstat).len(), 4);
        assert_eq!(read(LINUX_NETSTAT, Format::LinuxNetstat).len(), 4);
        assert_eq!(read(LINUX_SS, Format::Ss).len(), 4);
    }

    /// Why a probe has to name its format rather than only a command.
    ///
    /// The two failures are not the same, and the quieter one is worse. Read
    /// as linux-netstat, ss output produces nothing - loud, and obvious. Read
    /// as ss, netstat output still produces the right ports, because the
    /// address happens to sit in the same column, but every name is lost. That
    /// second one looks like a working list.
    #[test]
    fn a_mismatched_format_fails_quietly_enough_to_be_worth_preventing() {
        assert!(read(LINUX_SS, Format::LinuxNetstat).is_empty());

        let misread = read(LINUX_NETSTAT, Format::Ss);
        let ports: Vec<u16> = misread.iter().map(|l| l.port).collect();
        assert!(
            ports.contains(&3306) && ports.contains(&8080),
            "the real ports survive, which is what makes this hard to notice"
        );
        assert!(
            misread.iter().all(|l| l.process.is_none()),
            "every name is gone: a list that looks complete and is not"
        );
        assert!(
            ports.contains(&52233),
            "and worse, a merely connected socket is reported as listening - \
             the ss reader does not filter by state because its own output has \
             already done that"
        );
    }

    #[test]
    fn every_probe_that_failed_is_named_in_the_error() {
        let config = PortsConfig {
            probe: vec![
                Probe {
                    command: vec!["adev-no-such-tool-a".to_string()],
                    format: Format::Ss,
                },
                Probe {
                    command: vec!["adev-no-such-tool-b".to_string()],
                    format: Format::LinuxNetstat,
                },
            ],
            names: Vec::new(),
        };
        let error = listening(&config).unwrap_err().to_string();
        assert!(
            error.contains("adev-no-such-tool-a") && error.contains("adev-no-such-tool-b"),
            "an empty port list with only the last reason shown cannot be diagnosed; got {error}"
        );
    }

    #[test]
    fn a_chain_with_nothing_in_it_says_so_instead_of_reporting_no_ports() {
        let error = listening(&PortsConfig {
            probe: Vec::new(),
            names: Vec::new(),
        })
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("[ports]"),
            "zero probes and zero ports look identical unless one of them says why; got {error}"
        );
    }

    /// Taken from `netstat -lntp` inside this machine's WSL as root, so the
    /// owner column is filled, plus one line from the unprivileged run where
    /// it is a bare dash. Both forms happen and the parser has to survive the
    /// second without inventing an owner for it.
    const LINUX_NETSTAT: &str = "\
Active Internet connections (only servers)
Proto Recv-Q Send-Q Local Address           Foreign Address         State       PID/Program name
tcp        0      0 0.0.0.0:3306            0.0.0.0:*               LISTEN      1085/docker-proxy
tcp        0      0 127.0.0.53:53           0.0.0.0:*               LISTEN      147/systemd-resolve
tcp        0      0 10.255.255.254:53       0.0.0.0:*               LISTEN      -
tcp        0      0 127.0.0.1:2375          0.0.0.0:*               LISTEN      689/docker-proxy
tcp6       0      0 :::8080                 :::*                    LISTEN      1462/docker-proxy
tcp        0      0 192.168.1.5:52233       52.1.2.3:443            ESTABLISHED 900/curl
";

    #[test]
    fn linux_netstat_gives_the_port_the_pid_and_the_name_in_one_pass() {
        let listeners = parse_linux_netstat(LINUX_NETSTAT);
        let ports: Vec<u16> = listeners.iter().map(|l| l.port).collect();
        assert_eq!(
            ports,
            vec![53, 2375, 3306, 8080],
            "every listening socket once, and nothing that is merely connected"
        );

        let mysql = listeners.iter().find(|l| l.port == 3306).unwrap();
        assert_eq!(mysql.pid, Some(1085));
        assert_eq!(
            mysql.process.as_deref(),
            Some("docker-proxy"),
            "this format carries the name already, so nothing else need be asked"
        );
    }

    #[test]
    fn a_socket_linux_netstat_will_not_name_still_holds_its_port() {
        // Without root, netstat prints a dash for anything it does not own.
        let listeners = parse_linux_netstat(
            "tcp        0      0 10.255.255.254:53       0.0.0.0:*               LISTEN      -\n",
        );
        assert_eq!(listeners.len(), 1, "the port is held either way");
        assert_eq!(listeners[0].pid, None);
        assert_eq!(
            listeners[0].process, None,
            "a dash is an absence, not a process called '-'"
        );
    }

    #[test]
    fn linux_netstat_headers_are_not_read_as_sockets() {
        let listeners = parse_linux_netstat(
            "Active Internet connections (only servers)\n\
             Proto Recv-Q Send-Q Local Address Foreign Address State PID/Program name\n",
        );
        assert!(listeners.is_empty());
        assert!(parse_linux_netstat("").is_empty());
    }

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
