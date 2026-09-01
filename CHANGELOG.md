# Changelog

## 0.7.0

### No external program is assumed

- `[tools].git` and `[tools].kill` name the commands to run. A bare `git` is
  found on PATH, which is right almost always and wrong on a machine with more
  than one.
- `[ports].probe` is a chain, tried in order until one answers. On Linux the
  default is `ss` and then `netstat`, so a machine without iproute2 still gets
  a list rather than a dead end. macOS gets an empty chain and an error naming
  the setting, rather than a guess.
- A probe names its output format as well as its command, because the two are
  not interchangeable: netstat output read by the `ss` reader still yields the
  right ports, with every process name lost and connections counted as
  listeners — a list that looks complete and is not.
- When every probe fails, the error names each attempt and why it failed.

## 0.6.0

- The TLS stack nothing used is gone. Every request goes to a Docker daemon on
  this machine over plain HTTP, and `ring` — a C library reached through
  rustls — was the only thing making the other platforms unbuildable from a
  Windows host. The binary went from 5,208,064 to 3,679,744 bytes with it.
- Linux and macOS compile, and CI builds and runs the tests on both. That is
  the first time the `#[cfg(unix)]` branches had ever been compiled.
- A scan that finds nothing says where it looked and what named that place,
  and offers `adev config --init` when no configuration was found at all.

## 0.5.0

- `enter` on any row lists what can be done with it, in words, with the key
  that does each one beside it. Choosing an entry becomes that keystroke, so
  the menu is a way of finding the actions rather than a second copy of them.
- A refresh landing while a menu is open no longer replaces the list
  underneath it, which would have moved the row the action was meant for.
- Below 100 columns a strip along the top names all three panes, so a terminal
  showing one of them no longer looks like the whole tool.

## 0.4.0

- The ports pane lists every listening port rather than only the handful
  Docker publishes. The question it exists to answer was the one it could not.
- `K` ends what holds a port, `v` shows a project's toolchain versions, `d`
  lists the routed hostnames, and `E`, `I`, `.`, `A`, `X` and `:` cover the
  database dumps, the `.env` variants, the domains, and running one command.
- Two actions ask before they act, and only two: ending a process, and
  replacing a database with a dump.

## 0.3.0

- `[service.<name>]` declares the services this machine is meant to have.
  Declaring one makes it visible before it exists, and it is where the
  container name, port, hostname, panel URL and database credentials stop
  being this tool's guesses.
- A declared port stays on the row while the container is stopped. Docker
  publishes no ports for a stopped container, so the row lost its number at
  exactly the moment it was needed to start the thing again.
- `[open]`, `[memory]` and `[backup]` — the browser, the file manager, the two
  numbers in the footer, and where the dashboard writes a dump.
- A tool's `when` decides which projects it applies to, which is how a Go or
  Python toolchain becomes relevant without this code knowing the language
  exists. It had been written, tested, and never called.

## 0.2.0

### Toolchain versions

- `env` / `exec` / `shell` — per-project toolchain versions, for the projects
  that are not in containers. A machine holding PHP 7.4, 8.1, 8.2 and 8.3 side
  by side runs each project on the one it asks for. A bare version means that
  version: 7.4 is 7.4.x, not "7.4 or newer", which the semver crate would
  otherwise read as a caret and answer with 8.3.

Where the versions live is configuration, not convention. A pin beats what a
manifest declares, and when nothing installed satisfies the constraint nothing
is chosen: running a legacy project on an interpreter it said it cannot use,
silently, is the failure this exists to prevent.


### Closing the gap with the predecessor

- The dashboard acts as well as shows: start, stop and restart a service, open
  its port, run a project, open a shell in it. Quick things stay inside on
  their own thread; anything that wants the terminal gets it once the
  dashboard has handed it back. `?` lists every key.
- `ports` reports every listening port on the machine with the process holding
  it, and `kill <port>` ends that process. Docker only supplies names now — if
  the daemon is down the ports are still there, and still the question.
- `start`, `stop` and `restart` take `--all`.
- `open <service|project>` opens an address in a browser.
- `dotenv <project>` shows which .env a project runs with and switches between
  its variants, keeping the replaced one as .env.bak.
- Not carried across, on purpose: ejecting a drive, and the tray's WSL cache
  drop. Both are one machine's housekeeping rather than anything this tool can
  offer somebody else.

### Running projects

- `adev run <project>` starts a project the way its kind of project is
  started, with its own toolchain in front on PATH. Verified end to end: a
  Laravel 5.8 project boots its dev server on PHP 7.4.33 and answers, while
  the shell around it has PHP 8.2.
- Recipes cover Laravel, CodeIgniter 3 and 4, Django, FastAPI, Next.js, Vite,
  plain Node, Go, Rust and plain Python, and each can be replaced for a whole
  kind of project or for one project alone. `--print` says what would run
  without running it.

### Dashboard

- All three lists are on screen at once rather than one tab at a time. The
  question "is the database up" should not need a keystroke to answer. `tab`
  and `1`-`3` move the focus; each pane keeps its own row, so coming back to a
  list finds it where you left it.
- Below 100 columns the focused pane takes the whole screen instead: three
  panes on a narrow terminal are three unreadable columns.
- Drawn with real widgets — bordered frames, a header row, a lit border on the
  focused pane, colour carrying the same distinction the words do, per-pane
  counts, and a scrollbar only when a list is longer than fits.
- Its tests render into an in-memory terminal and assert on the cells, so they
  check what a user would see: that an unreachable daemon does not report
  "0 of 0 ready", that the focused pane looks focused, and that a ready
  service is not coloured like a stopped one.

### Also

- The configuration file is found rather than named: `aether.toml` in the
  current directory or any parent, then the machine-wide one. Passing
  `--config` on every command is the kind of friction that stops a tool being
  used at all.

## 0.1.0 — unreleased

First release. Replaces a 4,300-line PowerShell script that drove a WinForms
window on one machine.

### Commands

- `scan` — projects with framework and version, branch and working-tree state,
  gathered concurrently. Measured against the predecessor on the same 24
  repositories: 5,983 ms sequentially, 0.39s here. The framework version is
  the one installed rather than the constraint a manifest asks for.
- `services` — containers, with `ready` distinguished from `starting` so a
  database that is up but not yet accepting connections is not reported as
  usable. `--memory` adds usage matching what `docker stats` reports.
- `ports` — published ports and whether they answer.
- `start` / `stop` / `restart` — for containers that exist.
- `db backup` — every database on every running database service, into a
  timestamped directory. Verified at 55 databases in one file.
- `logs` — with `--follow` and `--tail`.
- `db export` / `db import` — MySQL, Postgres and Mongo. Credentials are read
  from the container's own environment; nothing is stored or asked for.
- `domains` — local hostnames, with the Caddyfile generated from a single
  source of truth rather than kept in step with it by hand.
- `tui` — a dashboard over projects, services and ports.

### Notes

- Every capability is a command before it is a screen, so anything the
  dashboard does can also go in a script or a scheduler.
- Configuration replaces the hard-coded paths of the predecessor. Unknown keys
  are refused rather than ignored.
- The name `localhost` is pinned to IPv4 when reaching Docker. On Windows it
  otherwise resolves to `::1` first and every request pays for a connection
  that cannot succeed: 2.45s against 0.72s for a listing.

