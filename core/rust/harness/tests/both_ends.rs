//! Both-ends smoke. Scratch git repos live in the temp folder, outside the project tree.
#![cfg(unix)]
use harness::both_ends::{both_ends, Spec};
use harness::gate_result::Status;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CHECK: &str = "#!/bin/sh\ntest \"$(cat value.txt)\" = good\n";

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hb-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Tester")
        .env("GIT_AUTHOR_EMAIL", ["1+t", "@", "users.", "noreply.example"].concat())
        .env("GIT_COMMITTER_NAME", "Tester")
        .env("GIT_COMMITTER_EMAIL", ["1+t", "@", "users.", "noreply.example"].concat())
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git failed: {:?}", args);
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write_check(dir: &Path) {
    std::fs::create_dir_all(dir.join("ci")).unwrap();
    let f = dir.join("ci/check.sh");
    std::fs::write(&f, CHECK).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A repo with two commits; `base_val` and `head_val` are the value.txt contents; `check_at_base` says whether the check file exists at base.
fn repo(name: &str, base_val: &str, head_val: &str, check_at_base: bool) -> (PathBuf, String, String) {
    let d = scratch(name);
    git(&d, &["init", "-q"]);
    std::fs::write(d.join("value.txt"), base_val).unwrap();
    if check_at_base {
        write_check(&d);
    }
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "base"]);
    let base = git(&d, &["rev-parse", "HEAD"]);
    std::fs::write(d.join("value.txt"), head_val).unwrap();
    write_check(&d);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "--allow-empty", "-m", "head"]);
    let head = git(&d, &["rev-parse", "HEAD"]);
    (d, base, head)
}

fn spec() -> Spec<'static> {
    Spec { id: "ticket-test", program: "ci/check.sh", args: &[], budget: Duration::from_secs(10), max_output: 1000 }
}

#[test]
fn red_at_base_and_green_at_head_passes_and_names_the_head_sha() {
    let (d, base, head) = repo("ok", "bad", "good", true);
    let r = both_ends(&d, &base, &head, &head, &spec()).unwrap();
    assert_eq!(r.status, Status::Pass);
    assert_eq!(r.failure_class, None);
    assert_eq!(r.head_sha, head);
    assert!(r.validate().is_ok());
    // no worktree is left behind
    assert_eq!(git(&d, &["worktree", "list"]).lines().count(), 1);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_check_added_by_the_fix_still_runs_against_base() {
    let (d, base, head) = repo("added", "bad", "good", false);
    let r = both_ends(&d, &base, &head, &head, &spec()).unwrap();
    assert_eq!(r.status, Status::Pass);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_check_that_passes_at_base_is_fail() {
    let (d, base, head) = repo("green", "good", "good", true);
    let r = both_ends(&d, &base, &head, &head, &spec()).unwrap();
    assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("passes-at-base")));
    assert_eq!(r.head_sha, head);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_check_still_red_at_head_is_fail() {
    let (d, base, head) = repo("red", "bad", "worse", true);
    let r = both_ends(&d, &base, &head, &head, &spec()).unwrap();
    assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("check-failed")));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_check_that_cannot_run_at_base_is_not_a_red_result() {
    let (d, base, head) = repo("broken", "bad", "good", true);
    let mut s = spec();
    s.budget = Duration::from_nanos(1);
    let r = both_ends(&d, &base, &head, &head, &s).unwrap();
    assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("base-error")));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_head_that_is_not_the_recorded_sha_is_refused() {
    let (d, base, head) = repo("mismatch", "bad", "good", true);
    assert!(both_ends(&d, &base, &head, &base, &spec()).is_err());
    assert!(both_ends(&d, &base, &head, "0123456789abcdef0123456789abcdef01234567", &spec()).is_err());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn bad_refs_and_bad_check_paths_are_refused() {
    let (d, base, head) = repo("refs", "bad", "good", true);
    assert!(both_ends(&d, "no-such-ref", &head, &head, &spec()).is_err());
    assert!(both_ends(&d, "--help", &head, &head, &spec()).is_err());
    assert!(both_ends(&d, &base, "", &head, &spec()).is_err());
    let mut s = spec();
    s.program = "../outside.sh";
    assert!(both_ends(&d, &base, &head, &head, &s).is_err());
    s.program = "ci/missing.sh";
    assert!(both_ends(&d, &base, &head, &head, &s).is_err());
    assert_eq!(git(&d, &["worktree", "list"]).lines().count(), 1);
    let _ = std::fs::remove_dir_all(d);
}
