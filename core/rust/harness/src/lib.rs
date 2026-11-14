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
pub mod events;
#[cfg(unix)]
pub mod evidence;
pub mod explain;
#[cfg(unix)]
pub mod finalize;
pub mod fix_tier;
#[cfg(debug_assertions)]
pub mod fixtures;
pub mod friction;
pub mod friction_event;
pub mod friction_loop;
pub mod gate_result;
pub mod gates;
pub mod gitio;
#[cfg(unix)]
pub mod handback;
#[cfg(unix)]
pub mod human_origin;
pub mod intake;
#[cfg(unix)]
pub mod launcher;
pub mod manifest;
pub mod overlay;
pub mod permission;
pub mod path_map;
pub mod plugin;
#[cfg(unix)]
pub mod quarantine;
pub mod question_class;
pub mod repo_id;
pub mod runner;
pub mod scan;
#[cfg(unix)]
pub mod session_close;
pub mod session_name;
#[cfg(unix)]
pub mod signoff;
pub mod status;
#[cfg(unix)]
pub mod supervisor;
#[cfg(unix)]
pub mod teardown;
pub mod ticket;
#[cfg(unix)]
pub mod ticket_gate;
#[cfg(unix)]
pub mod tree_prep;
#[cfg(unix)]
pub mod workspace;
#[cfg(unix)]
pub mod workspace_check;

/// Crate version string printed by `harness --version`.
pub fn version_line() -> String {
    format!("harness {}", env!("CARGO_PKG_VERSION"))
}
