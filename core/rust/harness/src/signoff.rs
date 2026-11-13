//! Sign-off id check over the launch records (P1B-20, S-1.5 part 5). std only, unix only.
//!
//! `harness session sign-off-check --ticket <t> --role <gov|e-rvw> --session <uuid-or-short-id>
//! --claim <verdict|done> [--records <file>] [--manifest-dir <dir>] [--log <path>] [--root <dir>]`.
//! Exit 0 accepted (one stdout line); 1 refused (exactly one stdout line, nothing on stderr); 2 usage,
//! unreadable input or a failed `unmanaged_session` append (stdout carries no `sign-off=` line). Read-only
//! except for the one `unmanaged_session` append. See `docs/signoff.md`.

use crate::events::{self, Field, Kind, Record};
use crate::launcher::{self, Launch, SIGN_OFF_ROLES};
use crate::manifest::{self, Line};
use crate::session_name;
use crate::supervisor::{ensure_default_log_folder, resolve_log_root, LogRoot};
use std::path::{Path, PathBuf};

/// Roles whose session ids count as build or orchestration ids of a ticket (Orchestrator input O-1).
pub(crate) const BUILD_ROLES: [&str; 4] = ["plan", "exec", "ship", "push"];
const DEFAULT_MANIFEST_DIR: &str = ".harness/manifests";
const USAGE: &str = "usage: harness session sign-off-check --ticket <t> --role <gov|e-rvw> --session <uuid-or-short-id> --claim <verdict|done> [--records <file>] [--manifest-dir <dir>] [--log <path>] [--root <dir>]";
const MAX_ARGS: usize = 32;

#[derive(Debug)]
struct Args {
    ticket: String,
    role: String,
    session: String,
    claim: String,
    records: Option<String>,
    manifest_dir: Option<String>,
    log: Option<String>,
    root: Option<String>,
}

fn parse_args(a: &[String]) -> Result<Args, String> {
    if a.len() > MAX_ARGS {
        return Err("too many arguments".into());
    }
    let mut slots: [Option<String>; 8] = Default::default();
    const NAMES: [&str; 8] = ["--ticket", "--role", "--session", "--claim", "--records", "--manifest-dir", "--log", "--root"];
    let mut it = a.iter();
    while let Some(f) = it.next() {
        let Some(i) = NAMES.iter().position(|n| n == f) else { return Err(format!("unknown argument `{f}`")) };
        let v = it.next().ok_or_else(|| format!("{f} is missing its value"))?;
        if slots[i].replace(v.clone()).is_some() {
            return Err(format!("{f} was given twice"));
        }
    }
    let [ticket, role, session, claim, records, manifest_dir, log, root] = slots;
    let need = |o: Option<String>, n: &str| o.ok_or_else(|| format!("{n} is required"));
    let ticket = need(ticket, "--ticket")?;
    if !events::token_ok(&ticket) {
        return Err("--ticket is not a plain token".into());
    }
    let role = need(role, "--role")?;
    if !SIGN_OFF_ROLES.contains(&role.as_str()) {
        return Err("--role must be gov or e-rvw".into());
    }
    let claim = need(claim, "--claim")?;
    if claim != "verdict" && claim != "done" {
        return Err("--claim must be verdict or done".into());
    }
    let session = need(session, "--session")?;
    if !launcher::uuid_ok(&session) && !(events::token_ok(&session) && session.len() <= 64) {
        return Err("--session must be a full uuid or a short id token of at most 64 bytes".into());
    }
    Ok(Args { ticket, role, session, claim, records, manifest_dir, log, root })
}

/// The log, resolved in the SessionStart hook's order: `--log`, then an absolute `HARNESS_EVENT_LOG`, then the cwd default.
fn resolve_log(log: Option<&str>, root: Option<&str>) -> Result<LogRoot, String> {
    if log.is_some() {
        return Ok(resolve_log_root(log, root));
    }
    match std::env::var_os("HARNESS_EVENT_LOG") {
        None => Ok(resolve_log_root(None, root)),
        Some(v) => {
            let ok = v.to_str().filter(|s| !s.is_empty() && !s.chars().any(|c| c.is_control()) && s.starts_with('/'));
            match ok {
                Some(s) => Ok(resolve_log_root(Some(s), root)),
                None => Err("HARNESS_EVENT_LOG must be an absolute path".into()),
            }
        }
    }
}

/// The ticket a launch name reads back as (lane bound unconstrained: the name passed its bound at launch).
fn name_ticket(name: &str) -> Option<String> {
    session_name::check(name, u64::MAX, false).ok().and_then(|v| v.ticket)
}

fn refuse(ticket: &str, reason: &str, why: &str) -> i32 {
    println!("sign-off=refused ticket={ticket} reason={reason}: {why}");
    1
}

fn cannot(m: &str) -> i32 {
    eprintln!("harness: error: {m}");
    2
}

fn missing(p: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(p) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(format!("cannot stat {}: {}", p.display(), e.kind())),
    }
}

fn write_unmanaged(lr: &LogRoot, ticket: &str, session_id: &str) -> Result<(), String> {
    ensure_default_log_folder(lr)?;
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let rec = Record::build(
        Kind::UnmanagedSession,
        ts,
        &[
            ("session_id", Field::Str(session_id.to_string())),
            ("ticket", Field::Str(ticket.to_string())),
            ("reason", Field::Str("no_launch_record".to_string())),
        ],
    )?;
    events::append_record(&lr.root, &lr.log, &rec).map_err(|e| e.to_string())
}

/// `harness session sign-off-check ...`.
pub fn sign_off_check_cmd(a: &[String]) -> i32 {
    let args = match parse_args(a) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("harness: error: {e}");
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let lr = match resolve_log(args.log.as_deref(), args.root.as_deref()) {
        Ok(l) => l,
        Err(e) => return cannot(&e),
    };
    // (b) read the inputs; any failure is exit 2.
    let records = PathBuf::from(args.records.as_deref().unwrap_or(launcher::DEFAULT_RECORDS));
    let launches = match launcher::read_launches(&records) {
        Ok(l) => l,
        Err(e) => return cannot(&format!("cannot check: launch records: {e}")),
    };
    let mdir = PathBuf::from(args.manifest_dir.as_deref().unwrap_or(DEFAULT_MANIFEST_DIR));
    let mfile = manifest::manifest_file(&mdir, &args.ticket);
    let workers: Vec<String> = match missing(&mfile) {
        Err(e) => return cannot(&format!("cannot check: manifest: {e}")),
        Ok(true) => Vec::new(),
        Ok(false) => match manifest::read_manifest(&mdir, &args.ticket) {
            Err(e) => return cannot(&format!("cannot check: manifest: {e}")),
            Ok(m) if !m.is_clean() => return cannot("cannot check: the ticket's manifest is damaged"),
            Ok(m) => m
                .lines
                .iter()
                .filter_map(|l| match l {
                    Line::Session { session_id, .. } => Some(session_id.clone()),
                    _ => None,
                })
                .collect(),
        },
    };
    let hook_ids: Vec<String> = match missing(&lr.log) {
        Err(e) => return cannot(&format!("cannot check: event log: {e}")),
        Ok(true) => Vec::new(),
        Ok(false) => match events::read_log(&lr.log) {
            Err(e) => return cannot(&format!("cannot check: event log: {e}")),
            Ok(r) if !r.is_clean() => return cannot("cannot check: the event log has a corrupt line or a torn tail"),
            Ok(r) => r
                .events
                .iter()
                .filter(|e| e.kind == Kind::SessionStarted && e.field("source") == Some(&Field::Str("hook".into())))
                .filter_map(|e| match e.field("session_id") {
                    Some(Field::Str(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
        },
    };
    let t = args.ticket.as_str();
    // (c) resolve a short id to one uuid.
    let uuid: String = if launcher::uuid_ok(&args.session) {
        args.session.clone()
    } else {
        let mut ids: Vec<&str> = launches
            .iter()
            .filter(|l| l.state == "bound" && l.short_id.as_deref() == Some(args.session.as_str()))
            .filter_map(|l| l.session_id.as_deref())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        match ids.len() {
            0 => return refuse(t, "no-launch-record", "the short id matches no launch record"),
            1 => ids[0].to_string(),
            _ => return cannot("short id is ambiguous in the launch records"),
        }
    };
    // (d) build and orchestration ids, BEFORE the signer's own row lookup.
    let bound = |l: &&Launch| l.state == "bound" && l.session_id.as_deref() == Some(uuid.as_str());
    let build_hit = launches
        .iter()
        .filter(bound)
        .any(|l| BUILD_ROLES.contains(&l.role.as_str()) && name_ticket(&l.name).as_deref() == Some(t))
        || workers.contains(&uuid);
    if build_hit {
        return refuse(t, "build-id", "the signer id equals a build-role session id of this ticket");
    }
    if hook_ids.contains(&uuid) {
        return refuse(t, "orchestration-id", "the signer id equals an interactive (orchestration) session id");
    }
    // (e) the signer's own launch row.
    let own: Vec<&Launch> = launches
        .iter()
        .filter(|l| l.session_id.as_deref() == Some(uuid.as_str()) && name_ticket(&l.name).as_deref() == Some(t))
        .collect();
    let own_bound: Vec<&&Launch> = own.iter().filter(|l| l.state == "bound").collect();
    if own_bound.is_empty() {
        return match write_unmanaged(&lr, t, &uuid) {
            Ok(()) => refuse(t, "no-launch-record", "the signer id has no bound launch record for this ticket"),
            Err(e) => cannot(&format!("sign-off refused: no launch record; unmanaged_session not recorded: {e}")),
        };
    }
    let matching: Vec<&&&Launch> = own_bound.iter().filter(|l| l.role == args.role).collect();
    if matching.is_empty() {
        return refuse(t, "role-mismatch", "the launch record's role differs from the claimed signer role");
    }
    if args.claim == "done" && args.role == "e-rvw" && !matching.iter().any(|l| l.sign_off) {
        return refuse(t, "not-marked-sign-off", "an e-rvw Done claim needs a session launched with --sign-off");
    }
    // (f) accept.
    let run = matching.iter().find(|l| l.sign_off).unwrap_or(&matching[0]).run_id.clone();
    println!("sign-off=accepted ticket={t} role={} claim={} run_id={run} session_id={uuid}", args.role, args.claim);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }
    const OK: [&str; 8] = ["--ticket", "1", "--role", "gov", "--session", "abc", "--claim", "done"];

    #[test]
    fn build_roles_subset_of_launcher_roles() {
        for r in BUILD_ROLES {
            assert!(session_name::LAUNCHER_ROLES.contains(&r), "{r}");
        }
    }

    #[test]
    fn build_roles_exclude_sign_off_roles() {
        for r in BUILD_ROLES {
            assert!(!SIGN_OFF_ROLES.contains(&r), "{r}");
        }
    }

    #[test]
    fn repeated_flag_is_refused() {
        let mut a = v(&OK);
        a.extend(v(&["--ticket", "2"]));
        assert!(parse_args(&a).unwrap_err().contains("twice"));
    }

    #[test]
    fn unknown_flag_is_refused() {
        let mut a = v(&OK);
        a.extend(v(&["--bogus", "x"]));
        assert!(parse_args(&a).unwrap_err().contains("unknown"));
        assert!(parse_args(&v(&OK)).is_ok());
    }

    #[test]
    fn bad_role_and_claim_are_refused() {
        let mut a = v(&OK);
        a[3] = "exec".into();
        assert!(parse_args(&a).is_err());
        let mut b = v(&OK);
        b[7] = "maybe".into();
        assert!(parse_args(&b).is_err());
    }
}
