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


## Settings

Everything is configurable and nothing is guessed at runtime, so the question
"where do I change this" needs an answer that is not "read the source".

```
adev config           what is in force, and which file it came from
adev config --init    write a starter file describing this machine
adev config --edit    open it in $EDITOR
```

`--init` writes only what it actually found — the PHP and Node directories that
exist and hold installations, and `DOCKER_HOST` as it is set. A generated file
that names a directory nobody has is worse than an empty one: it reads as a
fact about the machine and sends the reader looking for a bug that is not
there.

`adev config` reports how many versions each configured toolchain path actually
holds. A path that turns out to hold nothing looks identical to one that was
never set until you see a zero beside it.

Inside the dashboard, `g` shows the same thing. It is read-only there on
purpose: the file carries comments explaining each choice, and rewriting it
from a form would throw those away every time somebody changed a number.

Where a service's password comes from is shown; the password itself never is.
Both of these outputs get pasted into bug reports.

## Commands

```
adev scan       [--json]              projects, framework and version, branch, changes
adev services   [--json] [--memory]   containers and whether they are actually usable
adev ports      [--json]              every listening port, and what holds it
adev kill <port> [--dry-run]          end whatever is holding one
adev start|stop|restart <service...>|--all
adev open <service|project>           open it in a browser
adev dotenv <project> [--use <file>]  which .env it runs with, or switch it
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
adev config [--init|--edit]           settings, and where they live
adev tui                              the dashboard
```


## The dashboard

`adev tui` shows all three lists at once rather than one tab at a time — the
question "is the database up" should not need a keystroke to answer.

```
┌ 1 Projects ─────────────────────────────┬ 2 Services ──────────────┐
│ PROJECT          FRAMEWORK      BRANCH  │ SERVICE      PORT  STATE  │
│ billing-api      Laravel 9.52   fix/…   │ mysql        3306  ready  │
│ orders-queue     Laravel 11.54  featu…  │ redis        6379  stopped│
│ …                                       ├ 3 Ports ─────────────────┤
│                                         │ 3306  mysql   answering   │
└ 8 of 34 examined ───────────────────────┴ 3 of 5 answering ─────────┘
 8 projects of 34 directories · 3 ready of 5 services
```

`enter` on any row lists what you can do with it, in words, with the key that
does each one beside it — so nothing has to be memorised, and the shortcuts are
learned by using the menu rather than by reading a list. Choosing an entry is
the same as pressing that key: one path through the code, not two.

`tab` moves the focus, `1`–`3` jump straight to a pane, `j`/`k` move within
whichever pane has it, `q` quits, and `?` lists every key. Each pane keeps its
own row, so coming back to a list finds it where you left it.

`r` refreshes the pane you are looking at and `R` refreshes everything.
Rescanning every repository to find out whether a container came back up is a
wait nobody asked for, so the quick answer does not queue behind the slow one.

The dashboard is a peer of the command line, not a window onto part of it:
everything `adev` can do, it can do.

`s`, `x` and `S` start, stop and restart the selected service, `o` opens
whatever the focused row serves, `b` dumps its databases, `e` opens the
selected project's folder, `v` shows which toolchain versions it resolves to,
and `d` lists the routed hostnames. Those finish without taking the screen,
running on their own threads so the drawing never waits on them.

`E`, `I`, `.`, `A` and `X` need something typed — a database, a dump file, a
hostname — and ask for it on the status line. A prompt takes every key while it
is open, so `j` lands in what is being typed rather than moving a list behind
it, and an arriving notice cannot take the line out from under a half-typed
answer. `.` shows the project's `.env` variants while you type which one to
switch to, because nobody remembers the exact spelling of all of them.

`p` runs the selected project, `t` opens a shell in it, and `:` runs one
command in it. Those close the dashboard first, because a dev server and a
redraw loop fighting over the terminal would garble both.

Two things ask before they act, and only those two: `K`, which ends whatever
holds the selected port, and `I`, which replaces a database with a dump. Only
an explicit `y` goes ahead — any other key, including a stray arrow, is a no.
Everything else either reverses or only creates, and asking about those would
teach the habit of pressing `y` without reading.

The footer carries two numbers about the container host: how much memory is in
use inside the machine docker runs in, and what that machine costs this one.
The second is usually what explains a struggling laptop and cannot be seen from
inside it.

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
$ adev run shop-web
adev: php artisan serve · http://localhost:8000
Laravel development server started: <http://127.0.0.1:8000>
[...] PHP 7.4.33 Development Server (http://127.0.0.1:8000) started
```

`--print` says what would happen without doing it:

```
$ adev run shop-web --print
shop-web  C:/Projects/clients/shop-web
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

[open]                         # defaults to the platform's own openers
browser = ["firefox", "{}"]    # {} is where the target goes
file_manager = ["code", "{}"]

[backup]
directory = "backups"          # where the dashboard's b key writes
gzip = false

[memory]                       # the two numbers in the footer
interval_secs = 5              # 0 turns them off
guest = ["wsl", "free", "-m"]  # read the way free -m prints
host_process = ["vmmemWSL", "vmmem"]
```

An unknown key is an error rather than a typo that passes silently. A
configuration that parses but cannot work — no project roots, zero workers —
is refused at startup with the field named.

### Declaring services

Docker can only report containers that exist. A service defined in a compose
file but never started is exactly the one you open a dashboard to start, so
declaring it makes it visible — shown as `absent` until something creates it.

It is also where the details stop being this tool's guesses.

```toml
[service.mysql]
container = "mysql-db"             # default: the service's own name
port = 3306                        # kept on the row even while it is stopped
domain = "db.test"                 # routed by Caddy, to the container
panel = "http://localhost:8080"    # what `o` opens
user = "root"                      # who a dump connects as
password_env = "MYSQL_ROOT_PASSWORD"   # or `password` for a literal

[service.redis]                    # a name alone is enough
```

A declared port also fixes something visible on any machine: a stopped
container publishes no ports, so its row lost its port number at exactly the
moment you needed it to start the thing again. What a running container
actually publishes still wins over what was written down.

Credentials are still read from the container's own environment. These settings
only cover the containers built by hand, the ones that keep their password
somewhere else, and the dumps that should run as somebody other than the
superuser.

A service's `domain` is merged into the generated Caddyfile and never written
back into `domains.toml`, so the hostname stays a fact about the service
instead of being copied into an artefact that could then disagree with it. A
host claimed by both is refused by name.

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
- `start` acts on containers that exist. A declared service that has never been
  created is listed as `absent` and says what to run to create it, rather than
  failing with a daemon error.
- `--memory` costs about a second and a half per container, which is why the
  listing does not include it by default.
- Below 100 columns the three panes become one at a time, with a strip naming
  the other two. A terminal that narrow cannot show three readable columns.
- The dashboard's prompt is one line of plain text: no history, no completion,
  and no file picker. A dump file is typed as a path.
- Not carried over from the predecessor, deliberately: ejecting a drive, and
  the buttons for one particular editor. Those were buttons because the old
  tool was a window and knew one machine's layout.
- `env` and `exec` only consider a tool when the project was pinned to it or
  holds one of the files that tool's `when` lists. A project that never
  mentions Node does not get one put in front of it.
- Nothing yet reads a wanted version out of a `go.mod` or a `pyproject.toml`,
  so for those a pin or the newest installed decides.
- The memory footer reads `free` inside the guest and `tasklist` on Windows.
  On macOS the backing process is named differently between Docker Desktop
  versions, so `host_process` is empty there until somebody sets it.
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
