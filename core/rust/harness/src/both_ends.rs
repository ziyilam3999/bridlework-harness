//! Both-ends judgement (std only, Unix). The declared check must FAIL at base and PASS at head; the result names the head sha it ran on.
//! `judge_reports` is a pure mapping over the two end reports tree preparation returns: it spawns nothing, calls no git, reads no folder and writes nothing.
//! `judge` is the one call a gate makes: it hands everything to tree preparation, passes any refusal through verbatim, and maps the reports.
//! The end reports are the only source of the base and head shas, and `Ended` is read as given (never re-inferred from output text).
//! A check that already passes at base catches nothing and is FAIL. `Ended::Unknown` is never a red at base and never a pass at head.
//! Known limits: the two ends run one after the other, so a stateful check can differ between them; a check that exits 126 or 127 at base is read as
//! "something it needs is missing", not as red.

use crate::gate_result::{GateResult, Status};
use crate::runner::Ended;
use crate::tree_prep::{prepare_and_run, EndReport, Reports, Spec};
use std::path::Path;

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The same id rule `GateResult::validate` and the runner's limits check use: lowercase letters, digits, `-`, `_`, `.`.
fn id_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

fn fail(id: &str, head_sha: &str, class: &str, message: String) -> GateResult {
    GateResult { status: Status::Fail, gate_id: id.to_string(), head_sha: head_sha.to_string(), failure_class: Some(class.to_string()), message, file: None, line: None }
}

fn pass(id: &str, head_sha: &str) -> GateResult {
    GateResult { status: Status::Pass, gate_id: id.to_string(), head_sha: head_sha.to_string(), failure_class: None, message: "fails at base and passes at head".to_string(), file: None, line: None }
}

/// Every value is checked once, here, against its one root (the end reports); after this only the checked values are used.
fn check_entry(base: &EndReport, head: &EndReport, recorded_head: &str, gate_id: &str) -> Result<(), String> {
    if !id_ok(gate_id) {
        return Err("gate id must be lowercase letters, digits, '-', '_' or '.'".to_string());
    }
    if !sha_ok(&base.sha) || !sha_ok(&head.sha) {
        return Err("base and head must be full lowercase hex shas".to_string());
    }
    if head.sha != recorded_head {
        return Err("head does not match the recorded sha".to_string());
    }
    if base.sha == head.sha {
        return Err("base and head are the same commit".to_string());
    }
    Ok(())
}

/// Judge the declared check from the two end reports. Err means the request is refused (no result is produced); Ok carries a normalized result naming
/// the head sha taken from the head report.
pub fn judge_reports(reports: &Reports, recorded_head: &str, gate_id: &str) -> Result<GateResult, String> {
    let (base, head) = (&reports.base, &reports.head);
    check_entry(base, head, recorded_head, gate_id)?;

    // At base, before the head is looked at. Only a normal non-zero exit is a red result. 126 and 127 mean something the check needs is missing;
    // a signal, a timeout, a failed start or an unknown ending is a harness problem, never the test catching something.
    match base.ended {
        Ended::Exit(0) => return Ok(fail(gate_id, &head.sha, "passes-at-base", "the check passes at base, so it catches nothing".to_string())),
        Ended::Exit(c) if c != 126 && c != 127 => {}
        _ => return Ok(fail(gate_id, &head.sha, "base-error", "the check did not run cleanly at base, so it is not a red result".to_string())),
    }

    // At head, reached only when base was red. Only a clean exit 0 is a pass.
    Ok(match head.ended {
        Ended::Exit(0) => pass(gate_id, &head.sha),
        Ended::Exit(c) => fail(gate_id, &head.sha, "check-failed", format!("the check still fails at head (exit {}): {}", c, head.output)),
        Ended::Signal(s) => fail(gate_id, &head.sha, "head-signal", format!("the check was killed by signal {} at head", s)),
        Ended::TimedOut => fail(gate_id, &head.sha, "head-timeout", "the check ran past its budget at head".to_string()),
        Ended::NotStarted => fail(gate_id, &head.sha, "head-not-started", "the check could not be started at head".to_string()),
        Ended::Unknown => fail(gate_id, &head.sha, "head-unknown", "how the check ended at head could not be told, so it cannot certify the fix".to_string()),
    })
}

/// The one call a gate makes for both-ends mode: prepare both trees, run the check once in each, map the two reports.
/// Every refusal comes from tree preparation and passes through verbatim; this adds none of its own.
pub fn judge(repo_path: &Path, base: &str, head: &str, recorded_head: &str, spec: &Spec) -> Result<GateResult, String> {
    let reports = prepare_and_run(repo_path, base, head, recorded_head, spec)?;
    judge_reports(&reports, recorded_head, spec.id)
}
