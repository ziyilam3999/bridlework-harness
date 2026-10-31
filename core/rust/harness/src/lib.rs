//! Privacy gate engine (std only).

#[cfg(unix)]
pub mod both_ends;
#[cfg(unix)]
pub mod caps;
pub mod classes;
pub mod cli;
pub mod config;
pub mod core_layering;
pub mod decode;
pub mod detect;
pub mod events;
pub mod explain;
pub mod fixtures;
pub mod gate_result;
pub mod gates;
pub mod gitio;
pub mod intake;
pub mod overlay;
pub mod path_map;
pub mod repo_id;
pub mod runner;
#[cfg(unix)]
pub mod tree_prep;
pub mod scan;
pub mod status;
pub mod ticket;
#[cfg(unix)]
pub mod ticket_gate;

/// Crate version string printed by `harness --version`.
pub fn version_line() -> String {
    format!("harness {}", env!("CARGO_PKG_VERSION"))
}
