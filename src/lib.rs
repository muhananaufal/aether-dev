//! Contracts shared by every layer: the CLI, the collectors, and the TUI.
//!
//! Nothing here performs I/O. Types in `domain` describe what the collectors
//! produce; `config` describes what the user is allowed to configure, so that
//! no path, port, or root directory is ever a constant in the code.

pub mod config;
pub mod domain;
