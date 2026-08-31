//! The `adev` command.

use aether_dev::cli::{Cli, Command};
use aether_dev::config::Config;
use aether_dev::domain::Project;
use aether_dev::git::GitCli;
use aether_dev::ports::{collect, ProjectScanner};
use aether_dev::scan::FsProjectScanner;
use clap::Parser;
use serde::Serialize;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

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
        Command::Services { .. } => not_built_yet("services"),
        Command::Ports { .. } => not_built_yet("ports"),
        Command::Db(_) => not_built_yet("db"),
        Command::Domains(_) => not_built_yet("domains"),
        Command::Tui => not_built_yet("tui"),
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
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("adev: could not render JSON: {error}");
                return ExitCode::from(2);
            }
        }
        return ExitCode::SUCCESS;
    }

    for project in &outcome.projects {
        println!(
            "{:<30} {:<14} {:<9} {:<26} {}",
            clip(&project.name, 30),
            clip(project.category.as_deref().unwrap_or("-"), 14),
            format!("{:?}", project.stack),
            clip(project.git.branch.as_deref().unwrap_or("-"), 26),
            project.git.badge()
        );
    }
    for (path, reason) in &outcome.failures {
        println!("{:<30} {} ({})", "! unreadable", path.display(), reason);
    }

    // The denominator is printed on purpose: "no projects" and "nothing was
    // examined" are different answers and must not read the same.
    let examined = outcome
        .scanned
        .map_or_else(|| "?".to_string(), |count| count.to_string());
    println!(
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
