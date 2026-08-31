# Changelog

## 0.2.0 — unreleased

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

