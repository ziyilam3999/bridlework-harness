//! Privacy gate engine (std only). The scan modules land in the next step.

/// Crate version string printed by `harness --version`.
pub fn version_line() -> String {
    format!("harness {}", env!("CARGO_PKG_VERSION"))
}
