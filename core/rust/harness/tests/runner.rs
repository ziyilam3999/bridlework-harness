//! Runner smoke. Checks are tiny shell scripts written into a scratch repo outside the project tree.
#![cfg(unix)]
use harness::gate_result::{GateResult, Status};
use harness::runner::{run, Check};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hr-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn script(dir: &Path, name: &str, body: &str) {
    let f = dir.join(name);
    std::fs::write(&f, format!("#!/bin/sh\n{}\n", body)).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn load(dir: &Path, prog: &str, budget_ms: u64, cap: usize) -> Result<Check, String> {
    Check::load(dir, "smoke", prog, &[], Duration::from_millis(budget_ms), cap)
}

#[test]
fn a_clean_exit_after_the_budget_is_still_fail() {
    let d = scratch("late");
    script(&d, "ok.sh", "exit 0");
    // One nanosecond: the budget is gone before any exit can be seen, so a clean exit must not pass.
    let c = Check::load(&d, "smoke", "ok.sh", &[], Duration::from_nanos(1), 1000).unwrap();
    for _ in 0..5 {
        let r = run(&d, &c, SHA);
        assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("timeout")));
    }
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_malformed_check_id_is_refused_at_load() {
    let d = scratch("id");
    script(&d, "ok.sh", "exit 0");
    for bad in ["", "bad id!", "UPPER", "a/b"] {
        assert!(Check::load(&d, bad, "ok.sh", &[], Duration::from_secs(1), 100).is_err(), "should refuse {:?}", bad);
    }
    assert!(Check::load(&d, "unit-tests.v1_a", "ok.sh", &[], Duration::from_secs(1), 100).is_ok());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn past_budget_is_fail_and_inside_budget_is_pass() {
    let d = scratch("budget");
    script(&d, "slow.sh", "exec sleep 5");
    let t = Instant::now();
    let r = run(&d, &load(&d, "slow.sh", 300, 1000).unwrap(), SHA);
    assert_eq!(r.status, Status::Fail);
    assert_eq!(r.failure_class.as_deref(), Some("timeout"));
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(r.validate().is_ok());

    script(&d, "quick.sh", "echo fine");
    let r = run(&d, &load(&d, "quick.sh", 5000, 1000).unwrap(), SHA);
    assert_eq!(r.status, Status::Pass);
    assert_eq!(r.failure_class, None);
    assert_eq!(r.head_sha, SHA);
    assert!(r.message.contains("fine"));
    assert_eq!(GateResult::from_json(&r.to_json().unwrap()).unwrap(), r);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn nonzero_exit_and_missing_binary_are_fail() {
    let d = scratch("exit");
    script(&d, "bad.sh", "echo oops >&2; exit 3");
    let r = run(&d, &load(&d, "bad.sh", 5000, 1000).unwrap(), SHA);
    assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("check-failed")));
    assert!(r.message.contains("oops"));
    std::fs::write(d.join("noexec.sh"), "echo x\n").unwrap();
    let r = run(&d, &load(&d, "noexec.sh", 5000, 1000).unwrap(), SHA);
    assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("spawn-error")));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn output_over_the_cap_is_cut_and_marked() {
    let d = scratch("cap");
    script(&d, "loud.sh", "head -c 200000 /dev/zero | tr '\\0' a");
    let r = run(&d, &load(&d, "loud.sh", 10000, 1000).unwrap(), SHA);
    assert_eq!(r.status, Status::Pass);
    assert!(r.message.contains("[output cut at 1000 bytes]"));
    assert!(r.message.len() < 1100);
    script(&d, "small.sh", "echo tiny");
    let r = run(&d, &load(&d, "small.sh", 5000, 1000).unwrap(), SHA);
    assert!(!r.message.contains("output cut"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn paths_that_leave_the_repo_are_refused() {
    let base = scratch("escape");
    let repo = base.join("repo");
    std::fs::create_dir_all(repo.join("ci")).unwrap();
    script(&repo, "ci/ok.sh", "exit 0");
    script(&base, "outside.sh", "exit 0");
    symlink(base.join("outside.sh"), repo.join("ci/link.sh")).unwrap();
    symlink(&base, repo.join("ci/dirlink")).unwrap();
    symlink(repo.join("ci/ok.sh"), repo.join("ci/inner.sh")).unwrap();

    assert!(load(&repo, "ci/ok.sh", 1000, 100).is_ok());
    assert!(load(&repo, "ci/inner.sh", 1000, 100).is_ok());
    for bad in ["ci/link.sh", "ci/dirlink/outside.sh", "../outside.sh", "ci/../../outside.sh", "ci/missing.sh", "ci", ""] {
        assert!(load(&repo, bad, 1000, 100).is_err(), "should refuse {}", bad);
    }
    let abs = base.join("outside.sh");
    assert!(load(&repo, abs.to_str().unwrap(), 1000, 100).is_err());
    assert!(load(&repo, "ci/ok.sh", 0, 100).is_err());
    assert!(load(&repo, "ci/ok.sh", 1000, 0).is_err());
    let _ = std::fs::remove_dir_all(base);
}
