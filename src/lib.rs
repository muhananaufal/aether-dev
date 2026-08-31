//! Contracts shared by every layer: the CLI, the collectors, and the TUI.
//!
//! Nothing here performs I/O. `domain` describes what the collectors produce,
//! `config` describes what the user is allowed to configure, and `ports`
//! describes how a collector is allowed to report.

pub mod config;
pub mod domain;
pub mod ports;
