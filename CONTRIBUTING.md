# Contributing

## Running the checks

The same three gates CI runs, in the order that fails fastest:

```
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Clippy runs with warnings as errors. That is deliberate: a warning nobody has
to act on becomes a warning nobody reads.

## What the tests are for

Tests here assert what a user would see, not what a function happens to
return. A test named `a_repository_git_could_not_read_is_a_failure_not_a_clean_repository`
exists because reporting "no changes" for a repository nobody could read is a
lie somebody acts on — and the name says so, so a later reader knows what
breaking it would mean.

Write the test first and watch it fail before writing the code. A test you
have never seen fail has not been shown to test anything.

## Two things that are easy to get wrong

**A container being up is not the same as it being usable.** A database
reports itself running seconds before it accepts a connection. `ServiceStatus`
keeps those apart, and `condition()` gives them different words. Merging them
is how the predecessor told its user a database was ready when it was not.

**On Windows, crossterm sends a press and a release for one keystroke.** Key
handling filters on `KeyEventKind::Press`; without it every movement in the
dashboard counts twice. This is documented Ratatui behaviour, not a bug here.

## Commits

Conventional Commits. The subject says what changed; the body says why, in
terms of what would go wrong otherwise. A measurement beats an adjective — if
something got faster, say by how much and on what.

## Adding a database engine

`src/db.rs` decides what to run and with what, as a pure function over the
container's environment. Add the engine there with its dump and restore plans
and their tests; nothing else needs to change. Keep the password out of the
command line if the tool allows it — `mysqldump` and `pg_dump` do,
`mongodump` does not, and that difference is called out in the code.
