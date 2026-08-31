# Changelog

## 0.1.0 — unreleased

First release. Replaces a 4,300-line PowerShell script that drove a WinForms
window on one machine.

### Commands

- `scan` — projects with stack, branch and working-tree state, gathered
  concurrently. Measured against the predecessor on the same 24 repositories:
  5,983 ms sequentially, under a second here.
- `services` — containers, with `ready` distinguished from `starting` so a
  database that is up but not yet accepting connections is not reported as
  usable. `--memory` adds usage matching what `docker stats` reports.
- `ports` — published ports and whether they answer.
- `start` / `stop` / `restart` — for containers that exist. A service the
  compose file describes but that has never been created is reported as
  having nothing to act on rather than silently skipped.
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
