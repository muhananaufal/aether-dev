//! The `adev` command.

use aether_dev::cli::{Cli, Command, DbCommand, DomainCommand};
use aether_dev::config::Config;
use aether_dev::db::{backup_filename, dump_all_plan, dump_plan, restore_plan, Engine, ExecPlan};
use aether_dev::docker::{probe_all, DockerClient, DockerError, HttpDockerClient};
use aether_dev::domain::{Project, ServiceState, ServiceStatus};
use aether_dev::git::GitCli;
use aether_dev::ports::ScanEvent;
use aether_dev::ports::{collect, ProjectScanner};
use aether_dev::proxy::DomainSet;
use aether_dev::scan::FsProjectScanner;
use aether_dev::tui::{Dashboard, Tab, Update};
use clap::Parser;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use serde::Serialize;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
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
        Command::Services { json, memory } => services(&config, json, memory),
        Command::Ports { json } => ports(&config, json),
        Command::Db(command) => db(&config, command),
        Command::Domains(command) => domains(&config, command),
        Command::Start { services } => lifecycle(&config, &services, Action::Start),
        Command::Stop { services } => lifecycle(&config, &services, Action::Stop),
        Command::Restart { services } => lifecycle(&config, &services, Action::Restart),
        Command::Logs {
            service,
            follow,
            tail,
        } => logs(&config, &service, follow, tail),
        Command::Tui => tui(&config),
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
    let mut services = client.services().map_err(|error| {
        eprintln!("adev: {error}");
        ExitCode::from(2)
    })?;
    probe_all(&mut services, PORT_PROBE);
    if memory {
        fill_memory(&client, &mut services);
    }
    services.sort_by(|a, b| a.service.cmp(&b.service));
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
    ExitCode::SUCCESS
}

fn ports(config: &Config, json: bool) -> ExitCode {
    let services = match load_services(config, false) {
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
fn save_domains(config: &Config, set: &DomainSet, no_reload: bool) -> ExitCode {
    if let Err(error) = std::fs::write(&config.caddy.domains, set.to_toml_string()) {
        eprintln!(
            "adev: cannot write {}: {error}",
            config.caddy.domains.display()
        );
        return ExitCode::from(2);
    }
    if let Err(error) = std::fs::write(&config.caddy.caddyfile, set.to_caddyfile()) {
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
            for domain in set.entries() {
                outln!("{:<30} -> {}", clip(&domain.host, 30), domain.upstream);
            }
            // Saying where the answer came from matters when the answer is
            // empty: an unconfigured file and no routes look the same.
            outln!(
                "\n{} routed, from {}",
                set.entries().len(),
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

/// Finds the container behind a compose service name, accepting the container
/// name too since that is what `docker ps` shows and what people often type.
fn container_for(client: &HttpDockerClient, service: &str) -> Result<String, ExitCode> {
    let services = client.services().map_err(|error| {
        eprintln!("adev: {error}");
        ExitCode::from(2)
    })?;

    services
        .iter()
        .find(|candidate| candidate.service == service || candidate.container == service)
        .map(|found| found.container.clone())
        .ok_or_else(|| {
            let known: Vec<&str> = services.iter().map(|s| s.service.as_str()).collect();
            eprintln!(
                "adev: no service called {service:?}; known: {}",
                known.join(", ")
            );
            ExitCode::from(2)
        })
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
    let container = match container_for(&client, service) {
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
    let plan = match dump_plan(engine, database, &detail.env) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("adev: {error}");
            return ExitCode::from(2);
        }
    };

    let file = match std::fs::File::create(out) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("adev: cannot write {}: {error}", out.display());
            return ExitCode::from(2);
        }
    };
    let mut sink = BufWriter::new(file);

    let started = Instant::now();
    let outcome = if gzip {
        let mut encoder = GzEncoder::new(&mut sink, Compression::default());
        let outcome = client.exec(&container, &plan.command, &plan.env, &mut encoder);
        outcome.and_then(|outcome| {
            encoder
                .finish()
                .map(|_| outcome)
                .map_err(|error| DockerError::Malformed(error.to_string()))
        })
    } else {
        client.exec(&container, &plan.command, &plan.env, &mut sink)
    };

    let outcome = match outcome.and_then(|outcome| {
        sink.flush()
            .map(|()| outcome)
            .map_err(|error| DockerError::Malformed(error.to_string()))
    }) {
        Ok(outcome) => outcome,
        Err(error) => return abandon(out, &format!("{error}")),
    };

    if outcome.exit_code != 0 {
        // A half written dump that looks like a backup is worse than no dump,
        // so the file goes rather than being left for someone to trust later.
        let reason = String::from_utf8_lossy(&outcome.stderr).trim().to_string();
        return abandon(
            out,
            &format!("{} exited {}: {reason}", plan.command[0], outcome.exit_code),
        );
    }

    let bytes = std::fs::metadata(out).map(|meta| meta.len()).unwrap_or(0);
    if !outcome.stderr.is_empty() {
        // Warnings are not failures, but they are not nothing either.
        eprintln!("adev: {}", String::from_utf8_lossy(&outcome.stderr).trim());
    }
    outln!(
        "wrote {} ({bytes} bytes) from {container} in {:.2}s",
        out.display(),
        started.elapsed().as_secs_f64()
    );
    ExitCode::SUCCESS
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
    let container = match container_for(&client, service) {
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

    let plan = match restore_plan(engine, database, &remote, &detail.env) {
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
    let container = match container_for(&client, service) {
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

fn lifecycle(config: &Config, names: &[String], action: Action) -> ExitCode {
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

    let mut refused = false;
    for name in names {
        let found = services
            .iter()
            .find(|candidate| candidate.service == *name || candidate.container == *name);
        let Some(service) = found else {
            // One name nobody recognises must not cancel the others: a typo in
            // the third argument should not leave the first two untouched.
            eprintln!(
                "adev: no container for {name:?}; a service the compose file defines but that \
                 has never been created has nothing to act on"
            );
            refused = true;
            continue;
        };

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
        let plan = match dump_all_plan(engine, &detail.env) {
            Ok(plan) => plan,
            Err(error) => {
                eprintln!("adev: {}: {error}", service.service);
                failed = true;
                continue;
            }
        };

        let path = directory.join(backup_filename(&service.service, engine, gzip));
        match dump_to_file(&client, &service.container, &plan, &path, gzip) {
            Ok(bytes) => {
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
) -> Result<u64, String> {
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
    std::fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|error| error.to_string())
}
