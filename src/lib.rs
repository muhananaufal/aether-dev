//! Contracts shared by every layer: the CLI, the collectors, and the TUI.
//!
//! Nothing here performs I/O. `domain` describes what the collectors produce,
//! `config` describes what the user is allowed to configure, `ports` describes
//! how a collector is allowed to report, and `cli` is the non-interactive
//! surface that the terminal UI is later layered on top of.

pub mod cli;
pub mod config;
pub mod db;
pub mod docker;
pub mod domain;
pub mod framework;
pub mod git;
pub mod listen;
pub mod ports;
pub mod proxy;
pub mod recipe;
pub mod scan;
pub mod toolchain;
pub mod tui;
