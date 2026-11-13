//! Privacy gate engine (std only).

#[cfg(unix)]
pub mod both_ends;
#[cfg(unix)]
pub mod caps;
pub mod classes;
pub mod cli;
pub mod config;
pub mod control;
pub mod core_layering;
pub mod death;
pub mod decode;
pub mod detect;
#[cfg(unix)]
pub mod evidence;
pub mod events;
pub mod explain;
pub mod fix_tier;
pub mod fixtures;
pub mod friction;
pub mod friction_event;
pub mod friction_loop;
pub mod gate_result;
pub mod gates;
pub mod gitio;
pub mod intake;
#[cfg(unix)]
pub mod launcher;
pub mod manifest;
pub mod overlay;
pub mod path_map;
pub mod plugin;
pub mod question_class;
#[cfg(unix)]
pub mod quarantine;
pub mod repo_id;
pub mod runner;
#[cfg(unix)]
pub mod tree_prep;
pub mod scan;
pub mod session_name;
pub mod status;
#[cfg(unix)]
pub mod supervisor;
pub mod ticket;
#[cfg(unix)]
pub mod ticket_gate;
#[cfg(unix)]
pub mod workspace;
#[cfg(unix)]
pub mod workspace_check;

/// Crate version string printed by `harness --version`.
pub fn version_line() -> String {
    format!("harness {}", env!("CARGO_PKG_VERSION"))
}
