//! Both-ends judgement (std only), keyed by head sha. The declared check must FAIL at base and PASS at head; the result names the head sha it ran on.
//! This module is pure logic over two trees the caller has already prepared: it makes no git call and writes nothing. Preparing the trees (worktrees,
//! temp folders, carrying the check file to base, environment hygiene for the check, `.git` rules) is a separate ticket and is NOT done here.
//! A check that already passes at base catches nothing and is FAIL. A head that does not match the recorded sha, equal shas, or the same folder for both ends are refused.
//! Known limits: the check runs with the caller's environment; the two ends run one after the other, so a stateful check can differ between them;
//! a runner that reports a missing target with an exit code other than 126 or 127 reads as a red result.

use crate::gate_result::{GateResult, Status};
use crate::runner::{run_code, Check};
use std::path::{Component, Path};
use std::time::Duration;

/// The declared check: a path inside each tree plus its limits.
pub struct Spec<'a> {
    pub id: &'a str,
    pub program: &'a str,
    pub args: &'a [&'a str],
    pub budget: Duration,
    pub max_output: usize,
}

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn verdict(spec: &Spec, head_sha: &str, status: Status, class: Option<&str>, message: &str) -> GateResult {
    GateResult { status, gate_id: spec.id.to_string(), head_sha: head_sha.to_string(), failure_class: class.map(|c| c.to_string()), message: message.to_string(), file: None, line: None }
}

/// Every part of the check path must be a plain name, and none may be `.git` in any letter case.
fn plain_program(program: &str) -> bool {
    !program.is_empty()
        && Path::new(program).components().all(|c| match c {
            Component::Normal(n) => !n.to_string_lossy().eq_ignore_ascii_case(".git"),
            _ => false,
        })
}

/// Judge the declared check over a prepared pair of trees. Err means the request is refused; Ok carries a normalized result naming `head_sha`.
/// `base_tree` and `head_tree` must be different folders that both hold the check file at `spec.program`.
pub fn judge(base_tree: &Path, head_tree: &Path, base_sha: &str, head_sha: &str, recorded_head: &str, spec: &Spec) -> Result<GateResult, String> {
    if !sha_ok(base_sha) || !sha_ok(head_sha) {
        return Err("base and head must be full lowercase hex shas".to_string());
    }
    if head_sha != recorded_head {
        return Err("head does not match the recorded sha".to_string());
    }
    if base_sha == head_sha {
        return Err("base and head are the same commit".to_string());
    }
    if !plain_program(spec.program) {
        return Err("check path must be plain names relative to the tree".to_string());
    }
    let (b, h) = (base_tree.canonicalize().map_err(|_| "base tree unreadable".to_string())?, head_tree.canonicalize().map_err(|_| "head tree unreadable".to_string())?);
    if b == h {
        return Err("base and head trees are the same folder".to_string());
    }
    // Check::load resolves the path with realpath inside each tree and refuses anything that leaves it.
    let base_check = Check::load(&b, spec.id, spec.program, spec.args, spec.budget, spec.max_output)?;
    let head_check = Check::load(&h, spec.id, spec.program, spec.args, spec.budget, spec.max_output)?;

    let (at_base, base_code) = run_code(&b, &base_check, head_sha);
    if at_base.status == Status::Pass {
        return Ok(verdict(spec, head_sha, Status::Fail, Some("passes-at-base"), "the check passes at base, so it catches nothing"));
    }
    // Only a normal non-zero exit is a red result. 126 and 127 mean something the check needs is missing; a timeout, spawn error or signal is not a red either.
    let red = at_base.failure_class.as_deref() == Some("check-failed") && matches!(base_code, Some(c) if c != 126 && c != 127);
    if !red {
        return Ok(verdict(spec, head_sha, Status::Fail, Some("base-error"), "the check did not run cleanly at base, so it is not a red result"));
    }
    let (at_head, _) = run_code(&h, &head_check, head_sha);
    if at_head.status == Status::Pass {
        return Ok(verdict(spec, head_sha, Status::Pass, None, "fails at base and passes at head"));
    }
    Ok(at_head)
}
