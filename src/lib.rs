//! cave: a Wine bottle manager for macOS.
//!
//! All the domain logic lives here; the CLI (src/main.rs) and the graphical
//! interface (ui/) only call it.

pub mod apps;
pub mod bottle;
pub mod components;
pub mod config;
pub mod games;
pub mod i18n;
pub mod icon;
pub mod lnk;
pub mod processes;
pub mod runner;
pub mod runtime;
pub mod runtimes;
pub mod steam;
