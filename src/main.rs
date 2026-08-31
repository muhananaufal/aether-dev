//! The `adev` command.

use aether_dev::catalog;
use aether_dev::cli::{Cli, Command, DbCommand, DomainCommand};
use aether_dev::config::{self, Config};
use aether_dev::db::{
    backup_filename, dump_all_plan, dump_plan, restore_plan, Account, Engine, ExecPlan,
};
use aether_dev::docker::{probe_all, DockerClient, DockerError, HttpDockerClient};
use aether_dev::domain::{Project, ServiceState, ServiceStatus};
use aether_dev::dotenv;
use aether_dev::git::GitCli;
use aether_dev::listen;
use aether_dev::memory;
use aether_dev::open;
use aether_dev::ports::ScanEvent;
use aether_dev::ports::{collect, ProjectScanner};
use aether_dev::proxy::DomainSet;
use aether_dev::recipe;
use aether_dev::scan::FsProjectScanner;
use aether_dev::toolchain::{self, Reason, Resolution};
use aether_dev::tui::{Dashboard, Notice, Pane, Update};
use clap::Parser;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use serde::Serialize;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
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

/// Where this machine keeps its own configuration, when nothing nearer exists.
fn machine_config() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("aether-dev").join(config::CONFIG_NAME))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Named on the command line, else the nearest aether.toml, else this
    // machine's own. Having to pass --config every time is the kind of
    // friction that stops a tool being used at all.
    let chosen = cli.config.clone().or_else(|| {
        let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        config::discover(&here, machine_config().as_deref())
    });
    // The one command whose job is to create the file cannot require it to
    // already exist, or --config somewhere-new fails before it can write there.
    let writing_one = matches!(cli.command, Command::Config { init: true, .. });
    let config = match Config::load(chosen.as_deref().filter(|_| !writing_one)) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };

    match cli.command {
        Command::Scan { json } => scan(&config, json),
        Command::Services { json, memory } => services(&config, json, memory),
        Command::Ports { json } => ports(&config, json),
        Command::Kill { port, dry_run } => kill(port, dry_run),
        Command::Open { target } => open(&config, &target),
        Command::Dotenv { project, use_file } => dotenv(&config, &project, use_file.as_deref()),
        Command::Db(command) => db(&config, command),
        Command::Domains(command) => domains(&config, command),
        Command::Start { services, all } => lifecycle(&config, &services, all, Action::Start),
        Command::Stop { services, all } => lifecycle(&config, &services, all, Action::Stop),
        Command::Restart { services, all } => lifecycle(&config, &services, all, Action::Restart),
        Command::Logs {
            service,
            follow,
            tail,
        } => logs(&config, &service, follow, tail),
        Command::Run { project, print } => run(&config, &project, print),
        Command::Env { project } => env(&config, &project),
        Command::Exec { project, command } => exec(&config, &project, &command),
        Command::Shell {
            project,
            shell: named,
        } => shell(&config, &project, named.as_deref()),
        Command::Config { init, edit, force } => {
            settings(&config, chosen.as_deref(), init, edit, force)
        }
        Command::Tui => tui(&config, chosen.as_deref()),
    }
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
            "{:<30} {:<14} {:<18} {:<22} {}",
            clip(&project.name, 30),
            clip(project.category.as_deref().unwrap_or("-"), 14),
            clip(
                &project
                    .framework
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", project.stack)),
                18,
            ),
            clip(project.git.branch.as_deref().unwrap_or("-"), 22),
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

fn load_services(config: &Config, memory: bool) -> Result<Vec<ServiceStatus>, ExitCode> {
    let client = docker_client(config)?;
    let observed = client.services().map_err(|error| {
        eprintln!("adev: {error}");
        ExitCode::from(2)
    })?;
    let mut services = catalog::merge(&config.services_declared(), &observed);
    probe_all(&mut services, PORT_PROBE);
    if memory {
        fill_memory(&client, &mut services);
    }
    Ok(services)
}

fn services(config: &Config, json: bool, memory: bool) -> ExitCode {
    let services = match load_services(config, memory) {
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
        // The column only appears when it was asked for, so the usual listing
        // is not padded with dashes for a reading nobody wanted.
        let usage = if memory {
            format!("  {:>9}", megabytes(service.memory_bytes))
        } else {
            String::new()
        };
        outln!(
            "{:<18} {:<24} {:<7} {:<9}{usage}",
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

    // The same two numbers the dashboard keeps in its footer. Asked for with
    // --memory, because both probes shell out and nobody wants that on a
    // command they run in a loop.
    if memory {
        let reading = memory::read(&config.memory);
        if let Some(bytes) = reading.guest_bytes {
            outln!("{} in use where the containers run", megabytes(Some(bytes)));
        }
        if let Some((process, bytes)) = reading.host {
            outln!("{} of this machine, as {process}", megabytes(Some(bytes)));
        }
    }
    ExitCode::SUCCESS
}

#[derive(Serialize)]
struct PortRow<'a> {
    port: u16,
    process: Option<&'a str>,
    pid: Option<u32>,
    service: Option<&'a str>,
    answering: bool,
}

/// Everything listening on this machine, with the docker service named where
/// one of them is docker.
///
/// The question a developer asks is "what is on 8000", and the answer is often
/// a stray dev server rather than a container. Listing only what docker
/// published answers a narrower question than the one being asked.
fn ports(config: &Config, json: bool) -> ExitCode {
    let listeners = match listen::listening() {
        Ok(listeners) => listeners,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };

    // Docker is asked for names, not for the list. If the daemon is down the
    // ports are still there, and still the thing being asked about.
    let services = load_services(config, false).unwrap_or_default();
    let named: std::collections::HashMap<u16, &ServiceStatus> = services
        .iter()
        .filter_map(|service| service.port.map(|port| (port, service)))
        .collect();

    if json {
        let rows: Vec<PortRow> = listeners
            .iter()
            .map(|listener| PortRow {
                port: listener.port,
                process: listener.process.as_deref(),
                pid: listener.pid,
                service: named.get(&listener.port).map(|s| s.service.as_str()),
                answering: named
                    .get(&listener.port)
                    .is_some_and(|service| service.port_open),
            })
            .collect();
        return match serde_json::to_string_pretty(&rows) {
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

    for listener in &listeners {
        let service = named
            .get(&listener.port)
            .map_or("", |service| service.service.as_str());
        outln!(
            "{:>6}  {:<28} {:<8} {}",
            listener.port,
            clip(listener.process.as_deref().unwrap_or("-"), 28),
            listener
                .pid
                .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
            service
        );
    }
    outln!(
        "\n{} ports listening, {} of them docker services",
        listeners.len(),
        listeners
            .iter()
            .filter(|listener| named.contains_key(&listener.port))
            .count()
    );
    ExitCode::SUCCESS
}

fn kill(port: u16, dry_run: bool) -> ExitCode {
    let listeners = match listen::listening() {
        Ok(listeners) => listeners,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };
    let Some(listener) = listeners.iter().find(|listener| listener.port == port) else {
        eprintln!("adev: nothing is listening on {port}");
        return ExitCode::from(2);
    };
    let Some(pid) = listener.pid else {
        eprintln!("adev: {port} is held by a process this system would not name");
        return ExitCode::from(2);
    };
    let name = listener.process.as_deref().unwrap_or("unnamed process");

    if dry_run {
        outln!("would end {name} ({pid}), which holds {port}");
        return ExitCode::SUCCESS;
    }
    match listen::terminate(pid) {
        Ok(()) => {
            outln!("ended {name} ({pid}); {port} is free");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("adev: {error}");
            ExitCode::from(2)
        }
    }
}

/// Starts every collector on its own thread and returns at once. Nothing here
/// waits: results reach the dashboard as messages, which is the arrangement
/// that keeps the screen answering while work is still running.
fn spawn_collectors(config: &Config, updates: Sender<Update>) {
    spawn_project_collector(config, updates.clone());
    spawn_service_collector(config, updates);
}

/// Rescans the project roots. Separate from the services because a rescan of a
/// few dozen repositories is the slowest thing the dashboard does, and asking
/// for it every time somebody wants to see whether a container came back up
/// would make the quick answer wait on the slow one.
fn spawn_project_collector(config: &Config, updates: Sender<Update>) {
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
}

/// Asks docker what it has, and merges it with what the configuration says
/// this machine is meant to have.
fn spawn_service_collector(config: &Config, updates: Sender<Update>) {
    let endpoint = config.docker.endpoint.clone();
    let docker_host = std::env::var("DOCKER_HOST").ok();
    let declared = config.services_declared();
    std::thread::spawn(move || {
        let outcome = HttpDockerClient::new(&endpoint, docker_host.as_deref())
            .and_then(|client| client.services());
        let update = match outcome {
            Ok(observed) => {
                let mut services = catalog::merge(&declared, &observed);
                probe_all(&mut services, PORT_PROBE);
                Update::Services(services)
            }
            Err(error) => Update::ServicesFailed(error.to_string()),
        };
        let _ = updates.send(update);
    });
}

/// Re-reads what the container host costs, on its own timer.
///
/// The probes shell out and can be slow, so they get a thread rather than a
/// place in the draw loop. It ends when the dashboard drops the receiving end,
/// which is how every other collector here stops too.
fn spawn_memory_poller(config: &Config, updates: Sender<Update>) {
    let settings = config.memory.clone();
    if settings.interval_secs == 0 {
        return;
    }
    std::thread::spawn(move || {
        let interval = Duration::from_secs(settings.interval_secs);
        loop {
            let reading = memory::read(&settings);
            // A reading with nothing in it means neither probe applies to this
            // machine. Polling on would spend a process every few seconds to
            // learn the same thing again.
            if reading.is_empty() {
                break;
            }
            if updates.send(Update::Memory(reading)).is_err() {
                break;
            }
            std::thread::sleep(interval);
        }
    });
}

fn tui(config: &Config, chosen: Option<&Path>) -> ExitCode {
    let mut dashboard = Dashboard::new();
    let (updates, incoming) = mpsc::channel();
    spawn_collectors(config, updates.clone());
    spawn_memory_poller(config, updates.clone());
    // Held so closing the view can unwind the reader, which is otherwise
    // blocked waiting for a line that may never come.
    let mut watching: Option<Arc<AtomicBool>> = None;
    dashboard.set_settings(settings_lines(config, chosen));

    let outcome: std::io::Result<Leave> = ratatui::run(|terminal| {
        loop {
            // Take whatever has arrived and move on. Blocking here to wait for
            // a collector is exactly the mistake that froze the tool this
            // replaces for six seconds at a time.
            while let Ok(update) = incoming.try_recv() {
                dashboard.apply(update);
            }

            terminal.draw(|frame| dashboard.draw(frame))?;

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
            // A log takes the screen, so it takes the keys too. Leaving the
            // pane keys live behind it would move a list nobody can see.
            if dashboard.logs().is_some() {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('l') => {
                        if let Some(stop) = watching.take() {
                            stop.store(true, Ordering::Relaxed);
                        }
                        dashboard.close_logs();
                    }
                    KeyCode::Char('?') => dashboard.toggle_help(),
                    KeyCode::Char('j') | KeyCode::Down => dashboard.scroll_logs(1),
                    KeyCode::Char('k') | KeyCode::Up => dashboard.scroll_logs(-1),
                    KeyCode::PageDown => dashboard.scroll_logs(10),
                    KeyCode::PageUp => dashboard.scroll_logs(-10),
                    _ => {}
                }
                continue;
            }

            match key.code {
                KeyCode::Char('?') => dashboard.toggle_help(),
                KeyCode::Char('g') => dashboard.toggle_settings(),
                KeyCode::Char('l') => match dashboard.selected_service() {
                    Some(service) => {
                        let container = service.container.clone();
                        watching =
                            Some(spawn_log_stream(config, container.clone(), updates.clone()));
                        dashboard.open_logs(container);
                    }
                    None => dashboard.apply(Update::Notice(Notice::failed(
                        "l reads a service's log — move to the services or ports pane first",
                    ))),
                },
                // Esc closes the key list when it is open, and only quits when
                // there is nothing left to close.
                KeyCode::Esc if dashboard.showing_help() => dashboard.toggle_help(),
                KeyCode::Esc if dashboard.showing_settings() => dashboard.toggle_settings(),
                KeyCode::Char('q') | KeyCode::Esc => break Ok(Leave::Quit),
                KeyCode::Tab | KeyCode::Right => dashboard.focus_next(),
                KeyCode::BackTab | KeyCode::Left => dashboard.focus_previous(),
                KeyCode::Char('1') => dashboard.focus_on(Pane::Projects),
                KeyCode::Char('2') => dashboard.focus_on(Pane::Services),
                KeyCode::Char('3') => dashboard.focus_on(Pane::Ports),
                KeyCode::Char('j') | KeyCode::Down => dashboard.move_selection(1),
                KeyCode::Char('k') | KeyCode::Up => dashboard.move_selection(-1),
                KeyCode::PageDown => dashboard.move_selection(10),
                KeyCode::PageUp => dashboard.move_selection(-10),
                // The new collectors report into the same channel the loop is
                // already draining, so their results land like any other.
                // Refresh what you are looking at. Rescanning every repository
                // to find out whether a container restarted is a wait nobody
                // asked for, so the focused pane decides what is redone.
                KeyCode::Char('r') => match dashboard.focus() {
                    Pane::Projects => {
                        dashboard.begin_refresh();
                        dashboard.apply(Update::Notice(Notice::working("rescanning projects")));
                        spawn_project_collector(config, updates.clone());
                    }
                    Pane::Services | Pane::Ports => {
                        dashboard.apply(Update::Notice(Notice::working("rereading services")));
                        spawn_service_collector(config, updates.clone());
                    }
                },
                KeyCode::Char('R') => {
                    dashboard.begin_refresh();
                    dashboard.apply(Update::Notice(Notice::working("refreshing everything")));
                    spawn_collectors(config, updates.clone());
                }

                // Container actions stay here: they finish in a moment and the
                // answer belongs beside the row that changed.
                KeyCode::Char('s') | KeyCode::Char('x') | KeyCode::Char('S') => {
                    let action = match key.code {
                        KeyCode::Char('s') => Action::Start,
                        KeyCode::Char('x') => Action::Stop,
                        _ => Action::Restart,
                    };
                    match dashboard.selected_service() {
                        Some(service) => spawn_service_action(
                            config,
                            service.container.clone(),
                            action,
                            updates.clone(),
                        ),
                        None => dashboard.apply(Update::Notice(Notice::failed(
                            "no service here — move to the services or ports pane first",
                        ))),
                    }
                }

                KeyCode::Char('o') => {
                    // A service can name the page worth opening. A port with no
                    // page behind it is still better than nothing, so it stays
                    // as the fallback.
                    let url = dashboard.selected_service().and_then(|service| {
                        panel_for(config, &service.service)
                            .or_else(|| service.port.map(|port| format!("http://localhost:{port}")))
                    });
                    let notice = match url {
                        Some(url) => match spawn_opener(&config.open.browser, &url) {
                            Ok(()) => Notice::done(format!("opened {url}")),
                            Err(error) => Notice::failed(format!("{url}: {error}")),
                        },
                        None => Notice::failed("nothing here publishes a port or a page to open"),
                    };
                    dashboard.apply(Update::Notice(notice));
                }

                // The project's directory, in whatever this machine uses to
                // look at directories.
                KeyCode::Char('e') => {
                    let notice = match dashboard.selected_project() {
                        Some(project) => {
                            let path = project.path.display().to_string();
                            match spawn_opener(&config.open.file_manager, &path) {
                                Ok(()) => Notice::done(format!("opened {path}")),
                                Err(error) => Notice::failed(format!("{path}: {error}")),
                            }
                        }
                        None => Notice::failed(
                            "e opens a project's folder — move to the projects pane first",
                        ),
                    };
                    dashboard.apply(Update::Notice(notice));
                }

                // Anything that wants the terminal gets it, once the dashboard
                // has handed it back. Fighting over stdout would garble both.
                KeyCode::Enter => match dashboard.selected_project() {
                    Some(project) => break Ok(Leave::Run(project.name.clone())),
                    None => dashboard.apply(Update::Notice(Notice::failed(
                        "enter starts a project — move to the projects pane first",
                    ))),
                },
                KeyCode::Char('t') => match dashboard.selected_project() {
                    Some(project) => break Ok(Leave::Shell(project.name.clone())),
                    None => dashboard.apply(Update::Notice(Notice::failed(
                        "t opens a shell in a project — move to the projects pane first",
                    ))),
                },
                _ => {}
            }
        }
    });

    match outcome {
        Ok(Leave::Quit) => ExitCode::SUCCESS,
        Ok(Leave::Run(project)) => run(config, &project, false),
        Ok(Leave::Shell(project)) => shell(config, &project, None),
        Err(error) => {
            eprintln!("adev: {error}");
            ExitCode::from(2)
        }
    }
}

/// Reads the domains file. A file that is not there yet is an empty set, not a
/// failure: nothing has been routed, which is a valid state to start from.
fn load_domains(path: &Path) -> Result<DomainSet, ExitCode> {
    match std::fs::read_to_string(path) {
        Ok(text) => DomainSet::from_toml_str(&text).map_err(|error| {
            eprintln!("adev: {}: {error}", path.display());
            ExitCode::from(2)
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(DomainSet::default()),
        Err(error) => {
            eprintln!("adev: cannot read {}: {error}", path.display());
            Err(ExitCode::from(2))
        }
    }
}

/// Writes the source of truth first, then the file generated from it. In that
/// order a crash in between leaves the record of intent intact and the proxy
/// serving what it served before, rather than the other way round.
/// Every route the proxy should serve: the ones in the domains file, plus the
/// ones the declared services ask for.
///
/// The two are merged here rather than in the file, so a service's hostname
/// stays a fact about that service instead of being copied into an artefact
/// that would then disagree with it. A host claimed by both is refused, which
/// is the same answer the file gives when it names one host twice.
fn all_routes(config: &Config, set: &DomainSet) -> Result<DomainSet, ExitCode> {
    let declared = catalog::service_domains(&config.services_declared()).map_err(|error| {
        eprintln!("adev: {error}");
        ExitCode::from(2)
    })?;

    let mut merged = set.clone();
    for (host, upstream) in declared {
        if let Err(error) = merged.add(&host, &upstream) {
            eprintln!(
                "adev: {error} — {host} is named both by a service and in {}",
                config.caddy.domains.display()
            );
            return Err(ExitCode::from(2));
        }
    }
    Ok(merged)
}

fn save_domains(config: &Config, set: &DomainSet, no_reload: bool) -> ExitCode {
    let routes = match all_routes(config, set) {
        Ok(routes) => routes,
        Err(code) => return code,
    };
    // The file keeps only what was edited through it. What the services ask
    // for is written into the generated config and nowhere else.
    if let Err(error) = std::fs::write(&config.caddy.domains, set.to_toml_string()) {
        eprintln!(
            "adev: cannot write {}: {error}",
            config.caddy.domains.display()
        );
        return ExitCode::from(2);
    }
    if let Err(error) = std::fs::write(&config.caddy.caddyfile, routes.to_caddyfile()) {
        eprintln!(
            "adev: cannot write {}: {error}",
            config.caddy.caddyfile.display()
        );
        return ExitCode::from(2);
    }

    if no_reload {
        outln!(
            "wrote {} and {}; the proxy still serves the previous config until it restarts",
            config.caddy.domains.display(),
            config.caddy.caddyfile.display()
        );
        return ExitCode::SUCCESS;
    }

    let client = match docker_client(config) {
        Ok(client) => client,
        Err(code) => return code,
    };
    if let Err(error) = client.restart(&config.caddy.container) {
        eprintln!(
            "adev: wrote the config but could not restart {}: {error}",
            config.caddy.container
        );
        return ExitCode::from(2);
    }
    outln!("wrote the config and restarted {}", config.caddy.container);
    ExitCode::SUCCESS
}

fn domains(config: &Config, command: DomainCommand) -> ExitCode {
    let mut set = match load_domains(&config.caddy.domains) {
        Ok(set) => set,
        Err(code) => return code,
    };

    match command {
        DomainCommand::List => {
            let routes = match all_routes(config, &set) {
                Ok(routes) => routes,
                Err(code) => return code,
            };
            let from_file: Vec<&str> = set.entries().iter().map(|d| d.host.as_str()).collect();
            for domain in routes.entries() {
                // Which of the two named it decides where to go to change it.
                let source = if from_file.contains(&domain.host.as_str()) {
                    ""
                } else {
                    "  (from its service)"
                };
                outln!(
                    "{:<30} -> {}{source}",
                    clip(&domain.host, 30),
                    domain.upstream
                );
            }
            // Saying where the answer came from matters when the answer is
            // empty: an unconfigured file and no routes look the same.
            outln!(
                "\n{} routed, from {} and the declared services",
                routes.entries().len(),
                config.caddy.domains.display()
            );
            ExitCode::SUCCESS
        }
        DomainCommand::Add {
            host,
            upstream,
            no_reload,
        } => {
            if let Err(error) = set.add(&host, &upstream) {
                eprintln!("adev: {error}");
                return ExitCode::from(2);
            }
            save_domains(config, &set, no_reload)
        }
        DomainCommand::Remove { host, no_reload } => {
            if let Err(error) = set.remove(&host) {
                eprintln!("adev: {error}");
                return ExitCode::from(2);
            }
            save_domains(config, &set, no_reload)
        }
    }
}

/// The database account configured for a service, or an empty one that leaves
/// the container's own environment to answer.
///
/// The name is matched against both the declared service and the container it
/// names, because either is a reasonable thing to have typed.
fn account_for(config: &Config, service: &str) -> Account {
    config
        .service
        .iter()
        .find(|(name, settings)| {
            name.as_str() == service || settings.container_for(name) == service
        })
        .map(|(_, settings)| Account {
            user: settings.user.clone(),
            password: settings.password.clone(),
            password_env: settings.password_env.clone(),
        })
        .unwrap_or_default()
}

/// Finds the container behind a service name, accepting the container name too
/// since that is what `docker ps` shows and what people often type.
///
/// Declared services are searched alongside the running ones, so a name that
/// only exists in the configuration still resolves — and a service that was
/// declared but never created says so, instead of being reported as unknown.
fn container_for(
    client: &HttpDockerClient,
    config: &Config,
    service: &str,
) -> Result<String, ExitCode> {
    let observed = client.services().map_err(|error| {
        eprintln!("adev: {error}");
        ExitCode::from(2)
    })?;
    let services = catalog::merge(&config.services_declared(), &observed);

    let found = services
        .iter()
        .find(|candidate| candidate.service == service || candidate.container == service);

    match found {
        Some(found) if found.state == ServiceState::Absent => {
            eprintln!(
                "adev: {service:?} is declared but has no container called {:?} — create it first \
                 (docker compose up -d {service})",
                found.container
            );
            Err(ExitCode::from(2))
        }
        Some(found) => Ok(found.container.clone()),
        None => {
            let known: Vec<&str> = services.iter().map(|s| s.service.as_str()).collect();
            eprintln!(
                "adev: no service called {service:?}; known: {}",
                known.join(", ")
            );
            Err(ExitCode::from(2))
        }
    }
}

fn db(config: &Config, command: DbCommand) -> ExitCode {
    match command {
        DbCommand::Export {
            service,
            database,
            out,
            gzip,
            force,
        } => export(config, &service, &database, &out, gzip, force),
        DbCommand::Backup { out, gzip } => backup(config, &out, gzip),
        DbCommand::Import {
            service,
            database,
            file,
        } => import(config, &service, &database, &file),
    }
}

fn export(
    config: &Config,
    service: &str,
    database: &str,
    out: &Path,
    gzip: bool,
    force: bool,
) -> ExitCode {
    if out.exists() && !force {
        eprintln!(
            "adev: {} already exists; pass --force to replace it",
            out.display()
        );
        return ExitCode::from(2);
    }

    let client = match docker_client(config) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let container = match container_for(&client, config, service) {
        Ok(container) => container,
        Err(code) => return code,
    };

    let detail = match client.inspect(&container) {
        Ok(detail) => detail,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };
    let Some(engine) = Engine::from_image(&detail.image) else {
        eprintln!(
            "adev: {container} runs {}, which is not a database this build knows how to dump",
            detail.image
        );
        return ExitCode::from(2);
    };
    let plan = match dump_plan(engine, database, &detail.env, &account_for(config, service)) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };

    let started = Instant::now();
    match dump_to_file(&client, &container, &plan, out, gzip) {
        Ok((bytes, warnings)) => {
            if !warnings.is_empty() {
                eprintln!("adev: {}", String::from_utf8_lossy(&warnings).trim());
            }
            outln!(
                "wrote {} ({bytes} bytes) from {container} in {:.2}s",
                out.display(),
                started.elapsed().as_secs_f64()
            );
            ExitCode::SUCCESS
        }
        Err(reason) => abandon(out, &reason),
    }
}

/// Removes a dump that did not finish and says why, so the failure cannot be
/// mistaken later for a backup that exists.
fn abandon(out: &Path, reason: &str) -> ExitCode {
    let _ = std::fs::remove_file(out);
    eprintln!("adev: dump failed, {} removed: {reason}", out.display());
    ExitCode::from(2)
}

/// Reads a dump from disk, decompressing it when it is gzipped.
///
/// The whole file is held in memory because the archive handed to the daemon
/// has to be one body. That is fine for the dumps a local development stack
/// produces and would not be for a production sized one.
fn read_dump(path: &Path) -> Result<Vec<u8>, ExitCode> {
    let bytes = std::fs::read(path).map_err(|error| {
        eprintln!("adev: cannot read {}: {error}", path.display());
        ExitCode::from(2)
    })?;

    // Detected from the content rather than the file name, because a dump does
    // not stop being gzipped when somebody renames it.
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut plain = Vec::new();
        return GzDecoder::new(&bytes[..])
            .read_to_end(&mut plain)
            .map(|_| plain)
            .map_err(|error| {
                eprintln!(
                    "adev: {} is gzipped but unreadable: {error}",
                    path.display()
                );
                ExitCode::from(2)
            });
    }
    Ok(bytes)
}

/// Wraps the dump in a tar, which is the only shape the daemon accepts for
/// putting a file into a container.
fn tar_of(name: &str, content: &[u8]) -> Result<Vec<u8>, ExitCode> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o600);
    header.set_cksum();

    builder
        .append_data(&mut header, name, content)
        .and_then(|()| builder.into_inner())
        .map_err(|error| {
            eprintln!("adev: could not package the dump: {error}");
            ExitCode::from(2)
        })
}

fn import(config: &Config, service: &str, database: &str, file: &Path) -> ExitCode {
    let dump = match read_dump(file) {
        Ok(dump) => dump,
        Err(code) => return code,
    };

    let client = match docker_client(config) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let container = match container_for(&client, config, service) {
        Ok(container) => container,
        Err(code) => return code,
    };
    let detail = match client.inspect(&container) {
        Ok(detail) => detail,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };
    let Some(engine) = Engine::from_image(&detail.image) else {
        eprintln!(
            "adev: {container} runs {}, which is not a database this build knows how to load",
            detail.image
        );
        return ExitCode::from(2);
    };

    // A name of its own per run, so two imports at once cannot read each
    // other's file and a leftover from a crash is recognisable.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default();
    let name = format!("adev-import-{stamp}");
    let remote = format!("/tmp/{name}");

    let plan = match restore_plan(
        engine,
        database,
        &remote,
        &detail.env,
        &account_for(config, service),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };
    let archive = match tar_of(&name, &dump) {
        Ok(archive) => archive,
        Err(code) => return code,
    };

    let started = Instant::now();
    if let Err(error) = client.upload(&container, "/tmp", &archive) {
        eprintln!("adev: could not put the dump into {container}: {error}");
        return ExitCode::from(2);
    }

    let mut discard = Vec::new();
    let outcome = client.exec(&container, &plan.command, &plan.env, &mut discard);
    // The copy inside the container goes whatever happened, so a dump does not
    // sit in /tmp of a running database until somebody notices.
    let _ = client.exec(
        &container,
        &["rm".to_string(), "-f".to_string(), remote.clone()],
        &[],
        &mut Vec::new(),
    );

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };
    if outcome.exit_code != 0 {
        eprintln!(
            "adev: import failed, exit {}: {}",
            outcome.exit_code,
            String::from_utf8_lossy(&outcome.stderr).trim()
        );
        return ExitCode::from(2);
    }
    if !outcome.stderr.is_empty() {
        eprintln!("adev: {}", String::from_utf8_lossy(&outcome.stderr).trim());
    }

    outln!(
        "loaded {} ({} bytes) into {database} on {container} in {:.2}s",
        file.display(),
        dump.len(),
        started.elapsed().as_secs_f64()
    );
    ExitCode::SUCCESS
}

fn logs(config: &Config, service: &str, follow: bool, tail: Option<u32>) -> ExitCode {
    let client = match docker_client(config) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let container = match container_for(&client, config, service) {
        Ok(container) => container,
        Err(code) => return code,
    };
    // Asked rather than guessed: whether the output is framed depends on how
    // the container was started, and reading the wrong shape turns a log into
    // a screen of control bytes.
    let framed = match client.inspect(&container) {
        Ok(detail) => !detail.tty,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match client.logs(&container, follow, tail, framed, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        // Piping into head closes the stream on purpose; so does interrupting
        // a follow. Neither is a failure of ours.
        Err(DockerError::Unreachable { reason, .. }) if reason.contains("pipe") => {
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("adev: {error}");
            ExitCode::from(2)
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Start,
    Stop,
    Restart,
}

impl Action {
    fn done(self) -> &'static str {
        match self {
            Action::Start => "started",
            Action::Stop => "stopped",
            Action::Restart => "restarted",
        }
    }
}

fn lifecycle(config: &Config, names: &[String], all: bool, action: Action) -> ExitCode {
    let client = match docker_client(config) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let observed = match client.services() {
        Ok(services) => services,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };
    let services = catalog::merge(&config.services_declared(), &observed);

    // --all means every container there is, so the names are taken from what
    // docker reports rather than from the command line. A declared service with
    // no container is skipped rather than attempted: --all is a convenience,
    // and it should not turn into a row of failures for services the user never
    // asked about by name.
    let chosen: Vec<String> = if all {
        services
            .iter()
            .filter(|s| s.state != ServiceState::Absent)
            .map(|s| s.service.clone())
            .collect()
    } else {
        names.to_vec()
    };

    let mut refused = false;
    for name in &chosen {
        let found = services
            .iter()
            .find(|candidate| candidate.service == *name || candidate.container == *name);
        let Some(service) = found else {
            // One name nobody recognises must not cancel the others: a typo in
            // the third argument should not leave the first two untouched.
            eprintln!("adev: no service called {name:?}");
            refused = true;
            continue;
        };
        if service.state == ServiceState::Absent {
            eprintln!(
                "adev: {name:?} is declared but has no container called {:?} — create it first \
                 (docker compose up -d {name})",
                service.container
            );
            refused = true;
            continue;
        }

        let outcome = match action {
            Action::Start => client.start(&service.container),
            Action::Stop => client.stop(&service.container),
            Action::Restart => client.restart(&service.container),
        };
        match outcome {
            Ok(()) => outln!("{} {}", action.done(), service.container),
            Err(error) => {
                eprintln!(
                    "adev: {} could not be {}: {error}",
                    service.container,
                    action.done()
                );
                refused = true;
            }
        }
    }

    if refused {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// Fills in memory use for the containers that are running, all at once.
///
/// Sequentially this would cost about a second and a half per container. Only
/// running containers are asked, because a stopped one has nothing to report
/// and would spend the same time saying so.
fn fill_memory(client: &HttpDockerClient, services: &mut [ServiceStatus]) {
    let readings: Vec<_> = services
        .iter()
        .map(|service| {
            let container = service.container.clone();
            let running = service.state == ServiceState::Running;
            let endpoint = client.endpoint().to_string();
            std::thread::spawn(move || {
                if !running {
                    return None;
                }
                HttpDockerClient::new(&endpoint, None)
                    .ok()?
                    .memory(&container)
                    .ok()
                    .flatten()
            })
        })
        .collect();

    for (service, reading) in services.iter_mut().zip(readings) {
        service.memory_bytes = reading.join().unwrap_or(None);
    }
}

/// Bytes as something a person reads at a glance.
fn megabytes(bytes: Option<u64>) -> String {
    match bytes {
        Some(bytes) => format!("{} MB", bytes / 1_048_576),
        // Absent rather than zero: nobody should read a missing reading as a
        // measurement of nothing.
        None => "-".to_string(),
    }
}

/// A directory name that sorts chronologically and never collides with the
/// last run. UTC rather than local time, because a backup taken across a
/// daylight-saving change should still sort after the one before it.
fn backup_directory_name() -> String {
    let now = time::OffsetDateTime::now_utc();
    format!(
        "backup-{:04}{:02}{:02}-{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

fn backup(config: &Config, out: &Path, gzip: bool) -> ExitCode {
    let client = match docker_client(config) {
        Ok(client) => client,
        Err(code) => return code,
    };
    let services = match client.services() {
        Ok(services) => services,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };

    let directory = out.join(backup_directory_name());
    if let Err(error) = std::fs::create_dir_all(&directory) {
        eprintln!("adev: cannot create {}: {error}", directory.display());
        return ExitCode::from(2);
    }

    let started = Instant::now();
    let mut written = 0usize;
    let mut failed = false;

    for service in &services {
        if service.state != ServiceState::Running {
            // Said out loud rather than skipped in silence: a backup that
            // quietly leaves out a stopped database is the kind of gap nobody
            // finds until they need the file.
            outln!("skipped {} (not running)", service.service);
            continue;
        }
        let detail = match client.inspect(&service.container) {
            Ok(detail) => detail,
            Err(error) => {
                eprintln!("adev: {}: {error}", service.container);
                failed = true;
                continue;
            }
        };
        let Some(engine) = Engine::from_image(&detail.image) else {
            continue;
        };
        let plan = match dump_all_plan(engine, &detail.env, &account_for(config, &service.service))
        {
            Ok(plan) => plan,
            Err(error) => {
                eprintln!("adev: {}: {error}", service.service);
                failed = true;
                continue;
            }
        };

        let path = directory.join(backup_filename(&service.service, engine, gzip));
        match dump_to_file(&client, &service.container, &plan, &path, gzip) {
            Ok((bytes, warnings)) => {
                if !warnings.is_empty() {
                    eprintln!(
                        "adev: {}: {}",
                        service.service,
                        String::from_utf8_lossy(&warnings).trim()
                    );
                }
                outln!("{} -> {} ({bytes} bytes)", service.service, path.display());
                written += 1;
            }
            Err(reason) => {
                let _ = std::fs::remove_file(&path);
                eprintln!(
                    "adev: {} failed, its file removed: {reason}",
                    service.service
                );
                failed = true;
            }
        }
    }

    outln!(
        "\n{written} dumps in {} in {:.2}s",
        directory.display(),
        started.elapsed().as_secs_f64()
    );
    if failed {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// Runs one dump into one file. Shared by `db export` and `db backup` so the
/// two cannot drift into handling a failed dump differently.
fn dump_to_file(
    client: &HttpDockerClient,
    container: &str,
    plan: &ExecPlan,
    path: &Path,
    gzip: bool,
) -> Result<(u64, Vec<u8>), String> {
    let file = std::fs::File::create(path).map_err(|error| error.to_string())?;
    let mut sink = BufWriter::new(file);

    let outcome = if gzip {
        let mut encoder = GzEncoder::new(&mut sink, Compression::default());
        client
            .exec(container, &plan.command, &plan.env, &mut encoder)
            .map_err(|error| error.to_string())
            .and_then(|outcome| encoder.finish().map(|_| outcome).map_err(|e| e.to_string()))
    } else {
        client
            .exec(container, &plan.command, &plan.env, &mut sink)
            .map_err(|error| error.to_string())
    }?;

    sink.flush().map_err(|error| error.to_string())?;

    if outcome.exit_code != 0 {
        return Err(format!(
            "{} exited {}: {}",
            plan.command[0],
            outcome.exit_code,
            String::from_utf8_lossy(&outcome.stderr).trim()
        ));
    }
    let bytes = std::fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|error| error.to_string())?;
    // Warnings are not failures, but they are not nothing either, so they
    // travel back rather than being swallowed here.
    Ok((bytes, outcome.stderr))
}

/// Finds a project by name, or takes a path directly when one is given.
fn locate_project(config: &Config, wanted: &str) -> Result<(String, PathBuf), ExitCode> {
    let direct = Path::new(wanted);
    if direct.is_dir() {
        let name = direct
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| wanted.to_string());
        return Ok((name, direct.to_path_buf()));
    }

    let scanner =
        FsProjectScanner::new(GitCli::new(config.scan.git_timeout_ms), config.scan.workers);
    let (sender, receiver) = mpsc::channel();
    let roots = config.project.clone();
    std::thread::spawn(move || scanner.scan(&roots, sender));

    collect(receiver)
        .projects
        .into_iter()
        .find(|project| project.name == wanted)
        .map(|project| (project.name, project.path))
        .ok_or_else(|| {
            eprintln!("adev: no project called {wanted:?} under the configured roots");
            ExitCode::from(2)
        })
}

/// Works out which version of each configured tool this project should use.
///
/// A tool is considered when the project was pinned to a version of it, or
/// when the project carries the manifest that tool reads. Everything else is
/// left alone: putting a Node on PATH for a project that never mentions Node
/// would be this tool deciding something nobody asked it to.
fn toolchains_for(config: &Config, name: &str, path: &Path) -> Vec<(String, Resolution)> {
    let pins = config.pin.get(name);
    let read = |file: &str| std::fs::read_to_string(path.join(file)).ok();
    let composer = read("composer.json");
    let package = read("package.json");

    let mut resolved = Vec::new();
    for (tool, settings) in &config.toolchain {
        let pinned = pins.and_then(|pins| pins.get(tool)).map(String::as_str);
        let declared = match tool.as_str() {
            "php" => composer.as_deref().and_then(toolchain::wanted_php),
            "node" => package.as_deref().and_then(toolchain::wanted_node),
            _ => None,
        };
        let relevant = pinned.is_some()
            || match tool.as_str() {
                "php" => composer.is_some(),
                "node" => package.is_some(),
                _ => false,
            };
        if !relevant {
            continue;
        }

        let installed = toolchain::discover(settings);
        resolved.push((
            tool.clone(),
            toolchain::resolve(&installed, pinned, declared.as_deref()),
        ));
    }
    resolved.sort_by(|a, b| a.0.cmp(&b.0));
    resolved
}

fn why(reason: Reason) -> &'static str {
    match reason {
        Reason::Pinned => "pinned in config",
        Reason::Declared => "asked for by the project",
        Reason::Newest => "newest installed; the project asks for nothing",
        Reason::NothingSatisfies => "NOTHING INSTALLED SATISFIES THIS",
        Reason::NoneInstalled => "NO VERSION OF THIS TOOL WAS FOUND",
    }
}

fn env(config: &Config, project: &str) -> ExitCode {
    let (name, path) = match locate_project(config, project) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let resolved = toolchains_for(config, &name, &path);

    if resolved.is_empty() {
        outln!(
            "{name} resolves no toolchain. Either nothing is configured under \
             [toolchain], or this project names none."
        );
        return ExitCode::SUCCESS;
    }

    outln!("{name}  {}", path.display());
    let mut unresolved = false;
    for (tool, resolution) in &resolved {
        match &resolution.chosen {
            Some(chosen) => outln!(
                "  {:<8} {:<24} {}\n           {}",
                tool,
                chosen.version.to_string(),
                why(resolution.reason),
                chosen.path.display()
            ),
            None => {
                unresolved = true;
                outln!(
                    "  {:<8} {:<24} {} ({})",
                    tool,
                    "-",
                    why(resolution.reason),
                    resolution.constraint.as_deref().unwrap_or("no constraint")
                );
            }
        }
    }
    // A resolution that found nothing is reported in the exit code too, so a
    // script that sets up an environment can stop rather than carry on with
    // whatever happened to be on PATH already.
    if unresolved {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// Builds a PATH with the project's toolchains in front of whatever was there.
///
/// In front rather than instead: a project needs its own PHP, but it still
/// needs git, and the tools it shells out to.
fn path_with(resolved: &[(String, Resolution)]) -> Result<std::ffi::OsString, ExitCode> {
    let mut entries: Vec<PathBuf> = Vec::new();
    for (tool, resolution) in resolved {
        match &resolution.chosen {
            Some(chosen) => entries.push(chosen.path.clone()),
            None => {
                eprintln!(
                    "adev: no usable {tool}: {} ({})",
                    why(resolution.reason),
                    resolution.constraint.as_deref().unwrap_or("no constraint")
                );
                return Err(ExitCode::from(2));
            }
        }
    }
    if let Some(existing) = std::env::var_os("PATH") {
        entries.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(entries).map_err(|error| {
        eprintln!("adev: cannot build a PATH: {error}");
        ExitCode::from(2)
    })
}

fn exec(config: &Config, project: &str, argv: &[String]) -> ExitCode {
    let (name, path) = match locate_project(config, project) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let resolved = toolchains_for(config, &name, &path);
    let path_value = match path_with(&resolved) {
        Ok(value) => value,
        Err(code) => return code,
    };

    let Some((program, arguments)) = argv.split_first() else {
        eprintln!("adev: nothing to run");
        return ExitCode::from(2);
    };
    let status = std::process::Command::new(program)
        .args(arguments)
        .env("PATH", &path_value)
        .current_dir(&path)
        .status();

    match status {
        // The child's exit code is passed through rather than replaced: a
        // wrapper that always exits 0 breaks every script that wraps it.
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("adev: cannot run {program}: {error}");
            ExitCode::from(2)
        }
    }
}

fn shell(config: &Config, project: &str, shell: Option<&str>) -> ExitCode {
    let program = shell.map(str::to_string).or_else(default_shell);
    let Some(program) = program else {
        eprintln!("adev: no shell to start; name one with --shell");
        return ExitCode::from(2);
    };
    exec(config, project, &[program])
}

fn default_shell() -> Option<String> {
    std::env::var("SHELL")
        .ok()
        .or_else(|| std::env::var("ComSpec").ok())
        .filter(|shell| !shell.trim().is_empty())
}

/// The names of everything sitting directly in a project, which is what the
/// recipes are recognised from.
fn entries_in(path: &Path) -> Vec<String> {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Runs `argv` inside a project with its toolchain in front on PATH.
/// Shared by `exec` and `run` so the two cannot drift apart in how they set an
/// environment up or in what they do with the child's exit code.
fn run_with_toolchain(config: &Config, name: &str, path: &Path, argv: &[String]) -> ExitCode {
    let resolved = toolchains_for(config, name, path);
    let path_value = match path_with(&resolved) {
        Ok(value) => value,
        Err(code) => return code,
    };

    let Some((program, arguments)) = argv.split_first() else {
        eprintln!("adev: nothing to run");
        return ExitCode::from(2);
    };
    let status = std::process::Command::new(program)
        .args(arguments)
        .env("PATH", &path_value)
        .current_dir(path)
        .status();

    match status {
        // The child's exit code is passed through rather than replaced: a
        // wrapper that always exits 0 breaks every script that wraps it.
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(error) => {
            eprintln!("adev: cannot run {program}: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(config: &Config, project: &str, print: bool) -> ExitCode {
    let (name, path) = match locate_project(config, project) {
        Ok(found) => found,
        Err(code) => return code,
    };

    let entries = entries_in(&path);
    let present: Vec<&str> = entries.iter().map(String::as_str).collect();
    let Some(plan) = recipe::plan_for(&name, &present, &config.recipe, &config.run) else {
        eprintln!(
            "adev: nothing here says how {name} starts. Give it a command with \
             [run.{name}] in the config."
        );
        return ExitCode::from(2);
    };

    if print {
        outln!("{name}  {}", path.display());
        outln!(
            "  recipe   {}",
            plan.recipe.unwrap_or("none, configured by hand")
        );
        outln!("  command  {}", plan.command);
        match plan.port {
            Some(port) => outln!("  address  http://localhost:{port}"),
            None => outln!("  address  not known for this kind of project"),
        }
        for (tool, resolution) in toolchains_for(config, &name, &path) {
            match &resolution.chosen {
                Some(chosen) => outln!("  {:<8} {}", tool, chosen.version),
                None => outln!("  {:<8} {}", tool, why(resolution.reason)),
            }
        }
        return ExitCode::SUCCESS;
    }

    let argv = recipe::split(&plan.command);
    if let Some(port) = plan.port {
        // Printed before the child takes the terminal, because once a dev
        // server owns stdout this line would never be seen.
        eprintln!("adev: {} · http://localhost:{port}", plan.command);
    }
    run_with_toolchain(config, &name, &path, &argv)
}

/// Why the dashboard closed. Anything that takes over the terminal happens
/// after it has been handed back, rather than fighting the drawing for it.
enum Leave {
    Quit,
    Run(String),
    Shell(String),
}

impl Action {
    fn doing(self) -> &'static str {
        match self {
            Action::Start => "starting",
            Action::Stop => "stopping",
            Action::Restart => "restarting",
        }
    }
}

/// Acts on a container and then re-reads the service list, on its own thread.
///
/// Both halves matter: without the refresh the row the user just changed would
/// keep showing what it said before, which is the same kind of lie as calling a
/// starting database ready.
fn spawn_service_action(
    config: &Config,
    container: String,
    action: Action,
    updates: Sender<Update>,
) {
    let _ = updates.send(Update::Notice(Notice::working(format!(
        "{} {container}…",
        action.doing()
    ))));

    let endpoint = config.docker.endpoint.clone();
    let docker_host = std::env::var("DOCKER_HOST").ok();
    std::thread::spawn(move || {
        let client = match HttpDockerClient::new(&endpoint, docker_host.as_deref()) {
            Ok(client) => client,
            Err(error) => {
                let _ = updates.send(Update::Notice(Notice::failed(error.to_string())));
                return;
            }
        };
        let outcome = match action {
            Action::Start => client.start(&container),
            Action::Stop => client.stop(&container),
            Action::Restart => client.restart(&container),
        };
        let notice = match outcome {
            Ok(()) => Notice::done(format!("{} {container}", action.done())),
            Err(error) => Notice::failed(format!("{container}: {error}")),
        };
        let _ = updates.send(Update::Notice(notice));

        if let Ok(mut services) = client.services() {
            probe_all(&mut services, PORT_PROBE);
            services.sort_by(|a, b| a.service.cmp(&b.service));
            let _ = updates.send(Update::Services(services));
        }
    });
}

/// Hands a URL to whatever the system opens links with.
/// Hands `target` to whichever command the configuration says opens that kind
/// of thing.
fn spawn_opener(template: &[String], target: &str) -> std::io::Result<()> {
    let line = open::command_line(template, target).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "nothing is configured to open this",
        )
    })?;
    let (program, arguments) = line.split_first().expect("command_line never yields none");
    std::process::Command::new(program)
        .args(arguments)
        .spawn()
        .map(|_| ())
}

/// The page a service says is worth opening, if it named one.
fn panel_for(config: &Config, service: &str) -> Option<String> {
    config
        .service
        .iter()
        .find(|(name, settings)| {
            name.as_str() == service || settings.container_for(name) == service
        })
        .and_then(|(_, settings)| settings.panel.clone())
}

/// Opens a service or a project in a browser.
///
/// Services are looked at first because they are the shorter, more definite
/// list; a project only has an address if something knows how it starts.
fn open(config: &Config, target: &str) -> ExitCode {
    if let Ok(services) = load_services(config, false) {
        if let Some(service) = services
            .iter()
            .find(|s| s.service == target || s.container == target)
        {
            return match service.port {
                Some(port) => open_url(config, &format!("http://localhost:{port}")),
                None => {
                    eprintln!("adev: {target} publishes no port to open");
                    ExitCode::from(2)
                }
            };
        }
    }

    let (name, path) = match locate_project(config, target) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let entries = entries_in(&path);
    let present: Vec<&str> = entries.iter().map(String::as_str).collect();
    match recipe::plan_for(&name, &present, &config.recipe, &config.run).and_then(|plan| plan.port)
    {
        Some(port) => open_url(config, &format!("http://localhost:{port}")),
        None => {
            eprintln!("adev: nothing here says which address {name} would serve on");
            ExitCode::from(2)
        }
    }
}

fn open_url(config: &Config, url: &str) -> ExitCode {
    match spawn_opener(&config.open.browser, url) {
        Ok(()) => {
            outln!("opened {url}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("adev: cannot open {url}: {error}");
            ExitCode::from(2)
        }
    }
}

fn dotenv(config: &Config, project: &str, use_file: Option<&str>) -> ExitCode {
    let (name, path) = match locate_project(config, project) {
        Ok(found) => found,
        Err(code) => return code,
    };

    let entries = entries_in(&path);
    let names = dotenv::candidates(&entries);
    let variants: Vec<(String, String)> = names
        .iter()
        .filter_map(|candidate| {
            std::fs::read_to_string(path.join(candidate))
                .ok()
                .map(|contents| (candidate.clone(), contents))
        })
        .collect();
    let live = path.join(".env");
    let current = std::fs::read_to_string(&live).ok();

    let Some(wanted) = use_file else {
        if names.is_empty() {
            outln!("{name} has no .env variants to switch between");
            return ExitCode::SUCCESS;
        }
        let in_use = current
            .as_deref()
            .and_then(|contents| dotenv::active(contents, &variants));
        for candidate in &names {
            let marker = if Some(candidate.as_str()) == in_use.as_deref() {
                "*"
            } else {
                " "
            };
            outln!("{marker} {candidate}");
        }
        match (current.is_some(), in_use) {
            (false, _) => outln!("\n{name} has no .env at all"),
            // Named only when the contents actually match. A .env edited by
            // hand is nobody's copy, and saying otherwise would be a guess.
            (true, None) => {
                outln!("\n.env matches none of these; it was edited or written by hand")
            }
            (true, Some(_)) => outln!("\n* is the one .env currently matches"),
        }
        return ExitCode::SUCCESS;
    };

    if !names.iter().any(|candidate| candidate == wanted) {
        eprintln!(
            "adev: {name} has no {wanted}; it has {}",
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        );
        return ExitCode::from(2);
    }

    // The file being replaced is kept first. Overwriting somebody's working
    // .env with no way back is not a switch, it is a loss.
    if current.is_some() {
        if let Err(error) = std::fs::copy(&live, path.join(dotenv::BACKUP)) {
            eprintln!("adev: cannot keep the current .env: {error}");
            return ExitCode::from(2);
        }
    }
    match std::fs::copy(path.join(wanted), &live) {
        Ok(_) => {
            outln!("{name} now uses {wanted}");
            if current.is_some() {
                outln!("the previous .env is {}", dotenv::BACKUP);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("adev: cannot write .env: {error}");
            ExitCode::from(2)
        }
    }
}

/// Turns a container's output into lines on the dashboard as they arrive.
///
/// A writer rather than a buffer, because the log reader hands bytes to
/// whatever it is given and a following log never ends: collecting it first
/// would mean showing nothing until it did.
struct LogSink {
    container: String,
    updates: Sender<Update>,
    stop: Arc<AtomicBool>,
    partial: Vec<u8>,
}

impl Write for LogSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) {
            // The view was closed. Failing here is what unwinds the reader,
            // which is otherwise blocked waiting for a line that may never come.
            return Err(std::io::Error::other("log closed"));
        }
        self.partial.extend_from_slice(bytes);

        while let Some(at) = self.partial.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=at).collect();
            let text = String::from_utf8_lossy(&line).trim_end().to_string();
            if self
                .updates
                .send(Update::LogLine {
                    container: self.container.clone(),
                    line: text,
                })
                .is_err()
            {
                return Err(std::io::Error::other("nobody is reading"));
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Follows a container's log on its own thread, until the returned flag is set.
fn spawn_log_stream(
    config: &Config,
    container: String,
    updates: Sender<Update>,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let endpoint = config.docker.endpoint.clone();
    let docker_host = std::env::var("DOCKER_HOST").ok();
    let flag = Arc::clone(&stop);

    std::thread::spawn(move || {
        let client = match HttpDockerClient::new(&endpoint, docker_host.as_deref()) {
            Ok(client) => client,
            Err(error) => {
                let _ = updates.send(Update::Notice(Notice::failed(error.to_string())));
                return;
            }
        };
        // Asked rather than guessed: a container with a terminal sends one raw
        // stream and one without sends the multiplexed pair.
        let framed = client.inspect(&container).map(|d| !d.tty).unwrap_or(true);

        let mut sink = LogSink {
            container: container.clone(),
            updates: updates.clone(),
            stop: flag,
            partial: Vec::new(),
        };
        // A few hundred lines of history, then whatever comes next.
        if let Err(error) = client.logs(&container, true, Some(300), framed, &mut sink) {
            let _ = updates.send(Update::Notice(Notice::failed(format!(
                "{container}: {error}"
            ))));
        }
    });

    stop
}

/// The places a toolchain is commonly installed on this platform. Used only by
/// `--init`, and only to decide what to look at: a directory that turns out to
/// hold nothing is left out of what gets written.
fn likely_toolchains() -> Vec<(String, PathBuf, String)> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);

    #[cfg(windows)]
    let candidates = vec![
        ("php", PathBuf::from("C:/ProgramData/php"), "php.exe"),
        ("php", PathBuf::from("C:/laragon/bin/php"), "php.exe"),
        (
            "php",
            PathBuf::from("C:/ProgramData/laragon/bin/php"),
            "php.exe",
        ),
        (
            "node",
            home.clone()
                .map(|home| home.join("AppData/Local/nvm"))
                .unwrap_or_default(),
            "node.exe",
        ),
    ];
    #[cfg(not(windows))]
    let candidates = vec![
        (
            "node",
            home.clone()
                .map(|home| home.join(".nvm/versions/node"))
                .unwrap_or_default(),
            "node",
        ),
        ("php", PathBuf::from("/usr/local/opt"), "php"),
    ];

    candidates
        .into_iter()
        .map(|(tool, path, binary)| (tool.to_string(), path, binary.to_string()))
        .collect()
}

fn settings(
    config: &Config,
    chosen: Option<&Path>,
    init: bool,
    edit: bool,
    force: bool,
) -> ExitCode {
    let target = chosen
        .map(Path::to_path_buf)
        .or_else(machine_config)
        .unwrap_or_else(|| PathBuf::from(config::CONFIG_NAME));

    if init {
        if target.exists() && !force {
            eprintln!(
                "adev: {} already exists; pass --force to replace it",
                target.display()
            );
            return ExitCode::from(2);
        }
        let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let written = config::starter(
            &[here],
            &likely_toolchains(),
            std::env::var("DOCKER_HOST").ok().as_deref(),
        );
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        return match std::fs::write(&target, &written) {
            Ok(()) => {
                outln!("wrote {}", target.display());
                outln!("\n{written}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("adev: cannot write {}: {error}", target.display());
                ExitCode::from(2)
            }
        };
    }

    if edit {
        let Some(editor) = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .ok()
            .filter(|name| !name.trim().is_empty())
        else {
            eprintln!("adev: no EDITOR or VISUAL is set, so there is nothing to open it with");
            return ExitCode::from(2);
        };
        // Edited in a real editor rather than a form, so the comments
        // explaining each choice survive being changed.
        return match std::process::Command::new(&editor).arg(&target).status() {
            Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
            Err(error) => {
                eprintln!("adev: cannot run {editor}: {error}");
                ExitCode::from(2)
            }
        };
    }

    match chosen {
        Some(path) => outln!("read from {}", path.display()),
        // Said plainly, because "why is it not seeing my roots" is almost
        // always this.
        None => outln!("no configuration file was found — every default is in force"),
    }
    outln!("would be written to {}\n", target.display());

    outln!("[project]");
    outln!("  roots       {:?}", config.project.roots);
    outln!("  max_depth   {}", config.project.max_depth);
    outln!("[scan]");
    outln!("  workers     {}", config.scan.workers);
    outln!("[docker]");
    outln!("  endpoint    {}", config.docker.endpoint);
    outln!("[caddy]");
    outln!("  container   {}", config.caddy.container);
    outln!("  caddyfile   {}", config.caddy.caddyfile.display());
    outln!("  domains     {}", config.caddy.domains.display());

    if config.toolchain.is_empty() {
        outln!("\n[toolchain]  nothing configured — adev config --init finds what is here");
    } else {
        for (tool, settings) in &config.toolchain {
            let found = toolchain::discover(settings);
            outln!(
                "\n[toolchain.{tool}]  {} installed",
                if found.is_empty() {
                    "none".to_string()
                } else {
                    found.len().to_string()
                }
            );
            for installed in &found {
                outln!("  {:<12} {}", installed.version, installed.path.display());
            }
        }
    }
    ExitCode::SUCCESS
}

/// The settings the dashboard shows, flattened to lines here so the dashboard
/// itself never has to know what a configuration file is.
fn settings_lines(config: &Config, chosen: Option<&Path>) -> Vec<(String, String)> {
    let mut lines = vec![(
        "file".to_string(),
        match chosen {
            Some(path) => path.display().to_string(),
            // Almost always the answer to "why is it not seeing my roots".
            None => "none found — every default is in force".to_string(),
        },
    )];

    lines.push((
        "project.roots".to_string(),
        config
            .project
            .roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    ));
    lines.push(("scan.workers".to_string(), config.scan.workers.to_string()));
    lines.push((
        "docker.endpoint".to_string(),
        config.docker.endpoint.clone(),
    ));
    lines.push((
        "caddy.container".to_string(),
        config.caddy.container.clone(),
    ));

    if config.toolchain.is_empty() {
        lines.push((
            "toolchain".to_string(),
            "none — adev config --init finds what is here".to_string(),
        ));
    } else {
        let mut tools: Vec<&String> = config.toolchain.keys().collect();
        tools.sort();
        for tool in tools {
            let found = toolchain::discover(&config.toolchain[tool]);
            // The count is the point: a path that turns out to hold nothing
            // looks identical to one that was never set until you see a zero.
            lines.push((
                format!("toolchain.{tool}"),
                format!(
                    "{} installed · {}",
                    found.len(),
                    config.toolchain[tool]
                        .search
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    lines
}
