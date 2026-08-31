//! The `adev` command.

use aether_dev::cli::{Cli, Command};
use aether_dev::config::Config;
use aether_dev::docker::{probe_all, DockerClient, HttpDockerClient};
use aether_dev::domain::{Project, ServiceStatus};
use aether_dev::git::GitCli;
use aether_dev::ports::ScanEvent;
use aether_dev::ports::{collect, ProjectScanner};
use aether_dev::scan::FsProjectScanner;
use aether_dev::tui::{Dashboard, Tab, Update};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

/// Prints a line, treating a reader that walked away the way every other
/// command-line tool does: as a normal end, not a crash. `adev scan | head`
/// closes the pipe on purpose, and panicking there looks like a bug in us.
macro_rules! outln {
    ($($arg:tt)*) => {
        if let Err(error) = writeln!(std::io::stdout(), $($arg)*) {
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                return ExitCode::SUCCESS;
            }
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match Config::load(cli.config.as_deref()) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };

    match cli.command {
        Command::Scan { json } => scan(&config, json),
        Command::Services { json } => services(&config, json),
        Command::Ports { json } => ports(&config, json),
        Command::Db(_) => not_built_yet("db"),
        Command::Domains(_) => not_built_yet("domains"),
        Command::Tui => tui(&config),
    }
}

/// Commands whose surface is settled but whose implementation has not landed.
/// They refuse loudly with their own exit code, so a script can tell "not
/// built" apart from "ran and found nothing".
fn not_built_yet(name: &str) -> ExitCode {
    eprintln!(
        "adev: `{name}` has no implementation in this build; `scan` is the only command that runs"
    );
    ExitCode::from(3)
}

#[derive(Serialize)]
struct ScanReport<'a> {
    projects: &'a [Project],
    failures: Vec<Failure<'a>>,
    directories_examined: Option<usize>,
    elapsed_ms: u128,
}

#[derive(Serialize)]
struct Failure<'a> {
    path: &'a PathBuf,
    reason: &'a str,
}

fn scan(config: &Config, json: bool) -> ExitCode {
    let started = Instant::now();
    let scanner =
        FsProjectScanner::new(GitCli::new(config.scan.git_timeout_ms), config.scan.workers);
    let project_config = config.project.clone();
    let (sender, receiver) = std::sync::mpsc::channel();

    // The scan runs elsewhere and this thread drains the channel, which is the
    // same arrangement the terminal UI will use to keep drawing while results
    // arrive. Doing it inline would rebuild the freeze this tool replaces.
    std::thread::spawn(move || scanner.scan(&project_config, sender));
    let outcome = collect(receiver);
    let elapsed = started.elapsed();

    if json {
        let report = ScanReport {
            projects: &outcome.projects,
            failures: outcome
                .failures
                .iter()
                .map(|(path, reason)| Failure { path, reason })
                .collect(),
            directories_examined: outcome.scanned,
            elapsed_ms: elapsed.as_millis(),
        };
        match serde_json::to_string_pretty(&report) {
            Ok(text) => outln!("{text}"),
            Err(error) => {
                eprintln!("adev: could not render JSON: {error}");
                return ExitCode::from(2);
            }
        }
        return ExitCode::SUCCESS;
    }

    for project in &outcome.projects {
        outln!(
            "{:<30} {:<14} {:<9} {:<26} {}",
            clip(&project.name, 30),
            clip(project.category.as_deref().unwrap_or("-"), 14),
            format!("{:?}", project.stack),
            clip(project.git.branch.as_deref().unwrap_or("-"), 26),
            project.git.badge()
        );
    }
    for (path, reason) in &outcome.failures {
        outln!("{:<30} {} ({})", "! unreadable", path.display(), reason);
    }

    // The denominator is printed on purpose: "no projects" and "nothing was
    // examined" are different answers and must not read the same.
    let examined = outcome
        .scanned
        .map_or_else(|| "?".to_string(), |count| count.to_string());
    outln!(
        "\n{} projects, {} failures, {examined} directories examined in {:.2}s",
        outcome.projects.len(),
        outcome.failures.len(),
        elapsed.as_secs_f64()
    );
    ExitCode::SUCCESS
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

/// How long a service port is given to answer before it counts as closed.
/// Short on purpose: this runs on every refresh, and a stopped service must
/// not make the whole listing wait.
const PORT_PROBE: Duration = Duration::from_millis(300);

fn docker_client(config: &Config) -> Result<HttpDockerClient, ExitCode> {
    let docker_host = std::env::var("DOCKER_HOST").ok();
    HttpDockerClient::new(&config.docker.endpoint, docker_host.as_deref()).map_err(|error| {
        eprintln!("adev: {error}");
        ExitCode::from(2)
    })
}

fn load_services(config: &Config) -> Result<Vec<ServiceStatus>, ExitCode> {
    let client = docker_client(config)?;
    let mut services = client.services().map_err(|error| {
        eprintln!("adev: {error}");
        ExitCode::from(2)
    })?;
    probe_all(&mut services, PORT_PROBE);
    services.sort_by(|a, b| a.service.cmp(&b.service));
    Ok(services)
}

fn services(config: &Config, json: bool) -> ExitCode {
    let services = match load_services(config) {
        Ok(services) => services,
        Err(code) => return code,
    };

    if json {
        return match serde_json::to_string_pretty(&services) {
            Ok(text) => {
                outln!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("adev: could not render JSON: {error}");
                ExitCode::from(2)
            }
        };
    }

    for service in &services {
        outln!(
            "{:<18} {:<24} {:<7} {}",
            clip(&service.service, 18),
            clip(&service.container, 24),
            service
                .port
                .map_or_else(|| "-".to_string(), |p| p.to_string()),
            service.condition()
        );
    }
    let ready = services.iter().filter(|s| s.is_reachable()).count();
    outln!("\n{} services, {ready} ready", services.len());
    ExitCode::SUCCESS
}

fn ports(config: &Config, json: bool) -> ExitCode {
    let services = match load_services(config) {
        Ok(services) => services,
        Err(code) => return code,
    };
    let published: Vec<&ServiceStatus> = services.iter().filter(|s| s.port.is_some()).collect();

    if json {
        return match serde_json::to_string_pretty(&published) {
            Ok(text) => {
                outln!("{text}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("adev: could not render JSON: {error}");
                ExitCode::from(2)
            }
        };
    }

    for service in &published {
        outln!(
            "{:>6}  {:<18} {}",
            service.port.unwrap_or_default(),
            clip(&service.service, 18),
            if service.port_open {
                "answering"
            } else {
                "no answer"
            }
        );
    }
    let answering = published.iter().filter(|s| s.port_open).count();
    outln!(
        "\n{} published ports, {answering} answering",
        published.len()
    );
    ExitCode::SUCCESS
}

/// Starts every collector on its own thread and returns at once. Nothing here
/// waits: results reach the dashboard as messages, which is the arrangement
/// that keeps the screen answering while work is still running.
fn spawn_collectors(config: &Config, updates: Sender<Update>) {
    let project_config = config.project.clone();
    let workers = config.scan.workers;
    let git_timeout = config.scan.git_timeout_ms;
    let scan_updates = updates.clone();
    std::thread::spawn(move || {
        let scanner = FsProjectScanner::new(GitCli::new(git_timeout), workers);
        let (found, results) = mpsc::channel();
        std::thread::spawn(move || scanner.scan(&project_config, found));
        for event in results {
            let update = match event {
                ScanEvent::Found(project) => Update::Project(project),
                ScanEvent::Failed { path, reason } => Update::ScanFailed { path, reason },
                ScanEvent::Finished { scanned } => Update::ScanFinished { scanned },
            };
            if scan_updates.send(update).is_err() {
                break;
            }
        }
    });

    let endpoint = config.docker.endpoint.clone();
    let docker_host = std::env::var("DOCKER_HOST").ok();
    std::thread::spawn(move || {
        let outcome = HttpDockerClient::new(&endpoint, docker_host.as_deref())
            .and_then(|client| client.services());
        let update = match outcome {
            Ok(mut services) => {
                probe_all(&mut services, PORT_PROBE);
                services.sort_by(|a, b| a.service.cmp(&b.service));
                Update::Services(services)
            }
            Err(error) => Update::ServicesFailed(error.to_string()),
        };
        let _ = updates.send(update);
    });
}

fn tui(config: &Config) -> ExitCode {
    let mut dashboard = Dashboard::new();
    let (updates, incoming) = mpsc::channel();
    spawn_collectors(config, updates.clone());

    let outcome: std::io::Result<()> = ratatui::run(|terminal| {
        loop {
            // Take whatever has arrived and move on. Blocking here to wait for
            // a collector is exactly the mistake that froze the tool this
            // replaces for six seconds at a time.
            while let Ok(update) = incoming.try_recv() {
                dashboard.apply(update);
            }

            terminal.draw(|frame| {
                let area = frame.area();
                let view = dashboard.render(area.width, area.height);
                frame.render_widget(view.as_str(), area);
            })?;

            if !event::poll(Duration::from_millis(80))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            // Windows sends a press and a release for one keystroke, so without
            // this filter every movement counts twice.
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                KeyCode::Tab | KeyCode::Right => dashboard.next_tab(),
                KeyCode::Char('1') => dashboard.set_tab(Tab::Projects),
                KeyCode::Char('2') => dashboard.set_tab(Tab::Services),
                KeyCode::Char('3') => dashboard.set_tab(Tab::Ports),
                KeyCode::Char('j') | KeyCode::Down => dashboard.move_selection(1),
                KeyCode::Char('k') | KeyCode::Up => dashboard.move_selection(-1),
                KeyCode::PageDown => dashboard.move_selection(10),
                KeyCode::PageUp => dashboard.move_selection(-10),
                // The new collectors report into the same channel the loop is
                // already draining, so their results land like any other.
                KeyCode::Char('r') => {
                    dashboard.begin_refresh();
                    spawn_collectors(config, updates.clone());
                }
                _ => {}
            }
        }
    });

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("adev: {error}");
            ExitCode::from(2)
        }
    }
}
