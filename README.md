# aether-dev

A terminal dashboard for a local development environment: Docker services, a
project inventory with git status, ports, reverse-proxy domains, container
logs, database dump and restore, and per-project toolchain versions for the
projects that are not in containers at all.

That last one is the reason this exists rather than a list of Docker commands.
A machine can hold PHP 7.4, 8.1, 8.2 and 8.3 at once and a directory full of
legacy projects that each need a different one. Docker answers that by
isolating; without it, something has to read what each project asks for and put
the right binary in front of the wrong one.

Every capability is a command first. The dashboard is a layer on top of the
same functions, so anything you can do on screen you can also put in a script
or a scheduler.

**Status: early. Every command works and is used daily on the author's
machine, but it has run on exactly one setup so far — Windows with Docker
inside WSL. Expect the rough edges of a tool that has met one environment.**

## Commands

```
adev scan       [--json]              projects, framework and version, branch, changes
adev services   [--json] [--memory]   containers and whether they are actually usable
adev ports      [--json]              published ports and whether they answer
adev start|stop|restart <service...>  for containers that already exist
adev logs <service> [-f] [-n N]       what a service is writing
adev db export <service> --database <db> --out <file> [--gzip] [--force]
adev db import <service> --database <db> --file <file>
adev db backup --out <dir> [--gzip]   every database on every running service
adev domains    list
adev domains    add <host> <container:port> [--no-reload]
adev domains    remove <host> [--no-reload]
adev env <project>                    which toolchain versions it resolves to
adev exec <project> -- <cmd...>       run a command with those in front on PATH
adev shell <project> [--shell <s>]    a shell with those in front on PATH
adev tui                              the dashboard
```

## Toolchain versions

Nothing is discovered by convention: you say where your versions live, and a
directory counts when it both names a version and holds the binary.

```toml
[toolchain.php]
search = ["C:/ProgramData/php"]
binary = "php.exe"

[toolchain.node]
search = ["C:/Users/you/AppData/Local/nvm"]
binary = "node.exe"

[toolchain.go]
search = ["C:/Go"]
binary = "go.exe"
bin_subdir = "bin"        # where the binary sits inside a version directory

# For the legacy ones whose manifest says nothing, or says the wrong thing.
[pin.old-billing]
php = "7.4"
```

A project's version comes from a pin first, then from what it declares —
`require.php` in `composer.json`, `engines.node` in `package.json` — and
failing both, the newest installed.

Two rules worth knowing. **A bare version means that version**: `7.4` is
7.4.x, not "7.4 or newer", because the reason you pinned it was that the newer
one does not work. Operators still mean what they say, so `^8.1` may move up
within the major. And **when nothing installed satisfies the constraint,
nothing is chosen** and the command refuses — running a legacy project on an
interpreter it said it cannot use is the failure this feature exists to
prevent, and doing it silently would be worse.

```
$ adev exec old-billing -- php -v
PHP 7.4.33 (cli) ...
$ adev exec new-api -- php -v
PHP 8.3.33 (cli) ...
```

The toolchain goes in front of the existing PATH rather than replacing it: a
project needs its own PHP, and it still needs git.

`--config <file>` selects a configuration file; without one the defaults
apply. Paths inside the configuration resolve against the current directory.

## Why it exists

The predecessor was a 4,300-line PowerShell script driving a WinForms window.
It worked, and two things still made it a dead end.

**It froze.** Every render of the project list spawned `git status` once per
repository, sequentially, on the UI thread. Measured with the exact spawn
mechanism the script used: **5,983 ms for 24 repositories**. The same
repositories scanned concurrently here take **under a second**.

**It could not be shared.** A single-file PowerShell GUI wired to one
machine's paths is not something another developer can install.

## Configuration

Everything has a working default; a configuration file only overrides what it
mentions. See `aether.example.toml`.

```toml
[project]
roots = ["C:/Projects"]        # default: the current directory
max_depth = 3
ignore = ["node_modules", "vendor", "target", ".git"]

[scan]
workers = 12                   # git runs this many repositories at a time
git_timeout_ms = 2000
cache_ttl_secs = 30

[docker]
endpoint = "auto"              # auto follows DOCKER_HOST

[caddy]
container = "caddy-proxy"
caddyfile = "Caddyfile"        # generated; do not edit by hand
domains = "domains.toml"       # the source of truth for hostnames
```

An unknown key is an error rather than a typo that passes silently. A
configuration that parses but cannot work — no project roots, zero workers —
is refused at startup with the field named.

## Reaching Docker

`endpoint = "auto"` follows `DOCKER_HOST` the way other Docker clients do.
Only TCP and HTTP endpoints work in this build; a unix socket or a Windows
named pipe is reported as an unsupported transport rather than failing
obscurely.

On Windows with Docker inside WSL there is no socket the host can reach, so
the daemon has to be exposed over TCP for this tool to talk to it. **Be aware
of what that means: the Docker API is root-equivalent and unauthenticated.
Anything that can reach the port can start a container that mounts your whole
filesystem.** Bind it to `127.0.0.1` at most, and understand that on a
developer machine the realistic threat is not a network attacker but a
`postinstall` script in a package you installed.

Database credentials are never stored, read from a file, or asked for: they
are read from the container's own environment, which the daemon already
reports.

## Known limits

- One environment tested: Windows 11, Docker inside WSL2, reached over TCP.
- `db import` holds the dump in memory, because the archive handed to the
  daemon has to be one body. Fine for a local stack, not for a large dump.
- MySQL and Postgres keep the password out of the command line;
  `mongodump` offers no equivalent, so for Mongo it is visible in that
  container's process list while the dump runs.
- `start` acts on containers that exist. A service the compose file describes
  but that has never been created has nothing to start, and says so.
- `--memory` costs about a second and a half per container, which is why the
  listing does not include it by default.
- The dashboard is monochrome and shows projects, services and ports. Logs
  and memory are commands only.
- Not carried over from the predecessor, deliberately: opening a folder, an
  editor or Postman. Those were buttons because the old tool was a window; in
  a terminal you are already where `cd` and your editor live.
- `env` and `exec` only consider a tool when the project was pinned to it or
  carries the manifest that tool reads. A project that never mentions Node
  does not get one put in front of it.
- The Caddyfile is rewritten from the domains file. Anything added to it by
  hand is lost on the next change.

## Building

```
cargo build --release
```

The binary is `adev`. Developed against Rust 1.97 on the 2021 edition; the
oldest version that still compiles it has not been tested.

## License

MIT. See [LICENSE](LICENSE).
