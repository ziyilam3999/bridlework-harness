//! Privacy gate engine (std only).

pub mod classes;
pub mod cli;
pub mod decode;
pub mod detect;
pub mod explain;
pub mod fixtures;
pub mod gitio;
pub mod overlay;
pub mod scan;

/// Crate version string printed by `harness --version`.
pub fn version_line() -> String {
    format!("harness {}", env!("CARGO_PKG_VERSION"))
}
