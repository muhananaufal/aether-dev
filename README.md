# aether-dev

A terminal dashboard for a local development environment: Docker services,
a project inventory with git status, ports, reverse-proxy domains, and
database dump/restore.

**Status: pre-alpha. Nothing runs yet.** This repository currently contains a
license, a `.gitignore`, and this file. Code lands once the terminal rendering
spike is confirmed on Windows.

## Why this exists

The predecessor was a single 4,300-line PowerShell script driving a WinForms
window. It worked, and two things still made it a dead end:

- **It froze.** Every render of the project list spawned `git status` once per
  repository, sequentially, on the UI thread. Measured on the author's machine
  with the exact spawn mechanism the script used: **5,983 ms for 24
  repositories** (~249 ms each). The window was unresponsive for six seconds.
  The same 24 repositories scanned concurrently from a Rust prototype — doing
  *more* work, two git processes per repository instead of one — finished in
  **850 ms**.
- **It could not be shared.** A single-file PowerShell GUI wired to one
  machine's paths is not something another developer can install.

## Design decisions

| Decision | Reason |
| --- | --- |
| Rust + Ratatui 0.30.2, crossterm backend | Single binary, no runtime for users to install. Crossterm supports UNIX and Windows terminals. |
| CLI first, TUI as a layer on top | Every capability is also a non-interactive command. The TUI calls the same functions. Scriptable, testable, usable from a scheduler. |
| The draw loop never waits on a process | Collectors run concurrently and send results as messages; the UI draws the last known state and marks what is still loading. This is the rule the predecessor broke. |
| Windows first, cross-platform by construction | Docker is reached through its API rather than `wsl -d Ubuntu bash -c`, project roots are configuration rather than constants, and toolchains are found on `PATH` rather than in guessed directories. |

## Non-goals

- Replacing `lazydocker`, `ctop`, or Portainer for general container
  management. The value here is the combination with project inventory,
  framework and runtime-version detection, git status, and local domains.
- Graphical rendering. If a chart is ever required, a terminal is the wrong
  surface and this is the wrong tool.

## License

MIT. See [LICENSE](LICENSE).
