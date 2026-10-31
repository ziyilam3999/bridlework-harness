//! Privacy gate engine (std only).

#[cfg(unix)]
pub mod both_ends;
pub mod classes;
pub mod cli;
pub mod decode;
pub mod detect;
pub mod explain;
pub mod fixtures;
pub mod gate_result;
pub mod gitio;
pub mod overlay;
pub mod path_map;
pub mod runner;
#[cfg(unix)]
pub mod tree_prep;
pub mod scan;

/// Crate version string printed by `harness --version`.
pub fn version_line() -> String {
    format!("harness {}", env!("CARGO_PKG_VERSION"))
}
