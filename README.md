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

## Installing

```
cargo install --path .
```

That puts `adev` in `~/.cargo/bin`, which is already on PATH if you have Rust.
To run it without installing, build with `cargo build --release` and call
`target/release/adev` directly.

## Configuring

`adev` looks for `aether.toml` in the current directory and then each parent,
so running it inside a project still finds the configuration above the whole
workspace — and one project can override it by keeping its own. Failing all of
those it reads the machine-wide file:

- Windows: `%APPDATA%\aether-dev\aether.toml`
- Linux and macOS: `$XDG_CONFIG_HOME/aether-dev/aether.toml`, or
  `~/.config/aether-dev/aether.toml`

`--config <file>` overrides all of it. Everything has a working default, so a
configuration file only needs the parts you want to change. Start from
[aether.example.toml](aether.example.toml).

Paths inside the file resolve against the directory you run from, not against
the file. That matters for `caddyfile` and `domains`, which are written to.

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
adev run <project> [--print]          start it, on its own toolchain
adev env <project>                    which toolchain versions it resolves to
adev exec <project> -- <cmd...>       run a command with those in front on PATH
adev shell <project> [--shell <s>]    a shell with those in front on PATH
adev tui                              the dashboard
```


## The dashboard

`adev tui` shows all three lists at once rather than one tab at a time — the
question "is the database up" should not need a keystroke to answer.

```
┌ 1 Projects ─────────────────────────────┬ 2 Services ──────────────┐
│ PROJECT          FRAMEWORK      BRANCH  │ SERVICE      PORT  STATE  │
│ altrms           Laravel 9.52   fix/…   │ mysql        3306  ready  │
│ manunggal-queue  Laravel 11.54  featu…  │ redis        6379  stopped│
│ …                                       ├ 3 Ports ─────────────────┤
│                                         │ 3306  mysql   answering   │
└ 8 of 34 examined ───────────────────────┴ 3 of 5 answering ─────────┘
 8 projects of 34 directories · 3 ready of 5 services
```

`tab` moves the focus, `1`–`3` jump straight to a pane, `j`/`k` move within
whichever pane has it, `r` refreshes, `q` quits. Each pane keeps its own row,
so coming back to a list finds it where you left it.

The focused pane is the one with a lit border and a reversed row; the others
keep a quieter marker so you can still see where you were. Colour carries the
same distinction the words do — green for ready, yellow for starting, grey for
stopped, and modified files told apart from untracked ones. Each frame shows
its own count, and a scrollbar appears only when a list is longer than fits.

## Running a project

`adev run <project>` starts a project the way its kind of project is started,
with its own toolchain in front on PATH — so a Laravel 5.8 project boots on
PHP 7.4 while the shell around it still has 8.2.

```
$ adev run sapta-web
adev: php artisan serve · http://localhost:8000
Laravel development server started: <http://127.0.0.1:8000>
[...] PHP 7.4.33 Development Server (http://127.0.0.1:8000) started
```

`--print` says what would happen without doing it:

```
$ adev run sapta-web --print
sapta-web  C:/Projects/devivace/sapta-web
  recipe   laravel
  command  php artisan serve
  address  http://localhost:8000
  php      7.4.0
```

The built-in recipes cover Laravel, CodeIgniter 3 and 4, Django, FastAPI,
Next.js, Vite, plain Node, Go, Rust and plain Python. Marker order is
deliberate: nearly every Laravel project carries a `package.json` for its
assets, and starting npm there runs a bundler rather than the application.

Replace a recipe for every project that uses it, or one project on its own:

```toml
[recipe.laravel]                  # everything Laravel
command = "php artisan serve --host=0.0.0.0"
port = 8001

[run.old-billing]                 # just this one; beats the recipe
command = "php -S localhost:9000 -t public"
port = 9000
```

A project name beats a recipe name because it is more specific, and because a
project can be called `laravel`. A directory that says nothing about how it
starts gets an error naming the config key to add, not a guess.
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
- The dashboard shows projects, services and ports. Logs and memory are
  commands only.
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
