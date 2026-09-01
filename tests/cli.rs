//! What the binary actually does when somebody runs it.
//!
//! Every other test in this project calls a function. These run `adev` and
//! read what comes back, which is the only way to reach `main.rs` - three
//! thousand lines that had no test of any kind, and the part a stranger meets
//! first.
//!
//! Two rules keep them honest. Nothing here may depend on the machine it runs
//! on: every test builds its own directory and passes `--config`, so a
//! developer's own projects and their machine-wide configuration cannot make a
//! broken test pass. And nothing here needs Docker, because CI has none - the
//! commands that do need it are covered by what they say when it is missing,
//! which is behaviour worth pinning anyway.

use std::path::Path;
use std::process::{Command, Output};

/// The binary cargo just built, not whatever is on PATH.
const ADEV: &str = env!("CARGO_BIN_EXE_adev");

fn run(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(ADEV)
        .args(arguments)
        .current_dir(directory)
        // Pointed at nothing that exists, so the machine-wide configuration of
        // whoever runs this cannot reach in and change the answer.
        .env("APPDATA", directory.join("no-config-here"))
        .env("XDG_CONFIG_HOME", directory.join("no-config-here"))
        .env("HOME", directory.join("no-config-here"))
        .output()
        .expect("the binary cargo built should be runnable")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A directory holding two projects: one Laravel, one plain Node.
fn workspace() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a temporary directory");
    let shop = root.path().join("projects").join("shop-web");
    std::fs::create_dir_all(&shop).unwrap();
    std::fs::write(shop.join("composer.json"), r#"{"require":{}}"#).unwrap();
    std::fs::write(shop.join("artisan"), "").unwrap();

    let tools = root.path().join("projects").join("tools-ui");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(tools.join("package.json"), r#"{"name":"tools-ui"}"#).unwrap();

    let roots = root.path().join("projects").display().to_string();
    std::fs::write(
        root.path().join("aether.toml"),
        format!("[project]\nroots = [{roots:?}]\n"),
    )
    .unwrap();
    root
}

#[test]
fn the_version_it_reports_is_the_one_it_was_built_from() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &["--version"]);
    assert!(output.status.success());
    assert!(
        stdout(&output).contains(env!("CARGO_PKG_VERSION")),
        "got {:?}",
        stdout(&output)
    );
}

#[test]
fn scanning_finds_the_projects_and_says_what_each_one_is_built_with() {
    let workspace = workspace();
    let output = run(workspace.path(), &["--config", "aether.toml", "scan"]);
    let text = stdout(&output);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("shop-web"), "got {text}");
    assert!(text.contains("tools-ui"), "got {text}");
    assert!(
        text.contains("Laravel"),
        "artisan beside composer.json is Laravel, not plain PHP; got {text}"
    );
    assert!(text.contains("2 projects"), "got {text}");
}

#[test]
fn the_json_a_scan_prints_is_json() {
    let workspace = workspace();
    let output = run(
        workspace.path(),
        &["--config", "aether.toml", "scan", "--json"],
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("--json must produce parseable json");

    let projects = parsed["projects"]
        .as_array()
        .expect("an object with a projects array, which is what a consumer scripts against");
    assert_eq!(projects.len(), 2, "one entry per project");

    // By name, not by position: the scan runs the repositories in parallel and
    // the order they finish in is not something to assert on.
    let shop = projects
        .iter()
        .find(|project| project["name"] == "shop-web")
        .expect("shop-web in the json");
    assert_eq!(shop["stack"], "Laravel");
    assert!(
        shop["git"]["branch"].is_null(),
        "a fixture that is not a repository reports no branch rather than omitting the field"
    );
}

#[test]
fn a_first_run_with_no_configuration_says_where_it_looked_and_what_to_do() {
    // No aether.toml anywhere above it, and the machine-wide locations are
    // pointed at nothing: this is what somebody sees minutes after installing.
    let empty = tempfile::tempdir().unwrap();
    let output = run(empty.path(), &["scan"]);
    let text = stdout(&output);

    assert!(output.status.success(), "finding nothing is not an error");
    assert!(text.contains("0 projects"), "got {text}");
    assert!(text.contains("looked in"), "where it looked; got {text}");
    assert!(
        text.contains("adev config --init"),
        "and what to do about it; got {text}"
    );
}

#[test]
fn a_configuration_that_was_found_is_not_told_how_to_write_one() {
    let workspace = workspace();
    // An empty root, but a configuration that named it deliberately.
    let empty = workspace.path().join("nothing-here");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(
        workspace.path().join("explicit.toml"),
        format!("[project]\nroots = [{:?}]\n", empty.display().to_string()),
    )
    .unwrap();

    let output = run(workspace.path(), &["--config", "explicit.toml", "scan"]);
    let text = stdout(&output);
    assert!(text.contains("0 projects"));
    assert!(
        !text.contains("adev config --init"),
        "somebody who has a configuration and meant to scan an empty directory \
         does not need setting up; got {text}"
    );
}

#[test]
fn config_says_which_file_it_read_and_what_is_in_force() {
    let workspace = workspace();
    let output = run(workspace.path(), &["--config", "aether.toml", "config"]);
    let text = stdout(&output);

    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(text.contains("aether.toml"), "which file; got {text}");
    assert!(
        text.contains("[project]") && text.contains("[docker]"),
        "got {text}"
    );
}

#[test]
fn what_config_init_writes_is_a_file_the_next_run_can_read() {
    let dir = tempfile::tempdir().unwrap();
    let written = run(dir.path(), &["--config", "made.toml", "config", "--init"]);
    assert!(written.status.success(), "stderr: {}", stderr(&written));
    assert!(dir.path().join("made.toml").is_file(), "it wrote nothing");

    // The point of the test: what it generated has to load, not merely exist.
    let reread = run(dir.path(), &["--config", "made.toml", "config"]);
    assert!(
        reread.status.success(),
        "a generated config that will not load: {}",
        stderr(&reread)
    );
}

#[test]
fn an_unknown_setting_is_refused_rather_than_ignored() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("typo.toml"), "[scan]\nworkerz = 4\n").unwrap();

    let output = run(dir.path(), &["--config", "typo.toml", "scan"]);
    assert!(!output.status.success(), "a typo must not pass silently");
    assert!(
        stderr(&output).contains("workerz"),
        "and the message has to name it; got {}",
        stderr(&output)
    );
}

#[test]
fn a_configuration_file_that_is_not_there_is_an_error_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &["--config", "nowhere.toml", "scan"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("nowhere.toml"),
        "got {}",
        stderr(&output)
    );
}

#[test]
fn a_setting_that_parses_but_cannot_work_is_refused_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("zero.toml"), "[scan]\nworkers = 0\n").unwrap();

    let output = run(dir.path(), &["--config", "zero.toml", "scan"]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("workers"),
        "the field has to be named; got {}",
        stderr(&output)
    );
}

#[test]
fn a_service_command_without_docker_says_so_instead_of_reporting_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("nodocker.toml"),
        "[docker]\nendpoint = \"tcp://127.0.0.1:1\"\n",
    )
    .unwrap();

    let output = run(dir.path(), &["--config", "nodocker.toml", "services"]);
    assert!(
        !output.status.success(),
        "an unreachable daemon is not an empty list of services"
    );
    let complaint = stderr(&output);
    assert!(
        complaint.contains("127.0.0.1:1"),
        "and it has to say what it could not reach; got {complaint}"
    );
}

#[test]
fn domains_reports_where_its_answer_came_from_even_when_there_is_none() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("empty.toml"), "").unwrap();

    let output = run(dir.path(), &["--config", "empty.toml", "domains", "list"]);
    let text = stdout(&output);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        text.contains("0 routed") && text.contains("domains.toml"),
        "an unconfigured file and no routes look the same unless it says which; got {text}"
    );
}

#[test]
fn a_project_with_no_toolchain_configured_is_told_so_plainly() {
    let workspace = workspace();
    let output = run(
        workspace.path(),
        &["--config", "aether.toml", "env", "shop-web"],
    );
    let text = stdout(&output);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        text.contains("no toolchain") || text.contains("resolves no toolchain"),
        "got {text}"
    );
}

#[test]
fn naming_a_project_that_does_not_exist_fails_and_says_which_one() {
    let workspace = workspace();
    let output = run(
        workspace.path(),
        &["--config", "aether.toml", "env", "no-such-project"],
    );
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no-such-project"),
        "got {}",
        stderr(&output)
    );
}

#[test]
fn help_lists_the_commands_so_the_binary_can_explain_itself() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &["--help"]);
    let text = stdout(&output);
    assert!(output.status.success());
    for command in [
        "scan", "services", "ports", "tui", "config", "db", "domains",
    ] {
        assert!(text.contains(command), "{command} missing from --help");
    }
}
