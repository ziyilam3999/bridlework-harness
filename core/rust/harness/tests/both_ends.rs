//! Both-ends judgement smoke (Unix). The mapping tests fabricate end reports (no folder, no process, no git); the last test runs the real path on scratch repos.
#![cfg(unix)]
use harness::both_ends::{judge, judge_reports};
use harness::gate_result::Status;
use harness::runner::Ended;
use harness::tree_prep::{EndReport, Reports, Spec};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const HEAD: &str = "89abcdef0123456789abcdef0123456789abcdef";
const ID: &str = "ticket-test";

fn reports(base: Ended, head: Ended, head_out: &str) -> Reports {
    Reports {
        base: EndReport { sha: BASE.to_string(), ended: base, output: "base output".to_string() },
        head: EndReport { sha: HEAD.to_string(), ended: head, output: head_out.to_string() },
    }
}

fn judged(base: Ended, head: Ended) -> harness::gate_result::GateResult {
    judge_reports(&reports(base, head, ""), HEAD, ID).expect("well-formed reports are judged")
}

#[test]
fn red_at_base_green_at_head_passes_and_names_the_head_sha() {
    let r = judge_reports(&reports(Ended::Exit(1), Ended::Exit(0), ""), HEAD, "chosen-id.7").unwrap();
    assert_eq!(r.status, Status::Pass, "{:?} {}", r.failure_class, r.message);
    assert_eq!(r.failure_class, None);
    assert_eq!(r.head_sha, HEAD);
    assert_eq!(r.gate_id, "chosen-id.7");
    assert!(r.validate().is_ok());
}

#[test]
fn a_check_that_passes_at_base_is_fail() {
    let r = judged(Ended::Exit(0), Ended::Exit(0));
    assert_eq!(r.status, Status::Fail);
    assert_eq!(r.failure_class.as_deref(), Some("passes-at-base"));
    assert_eq!(r.head_sha, HEAD);
    assert!(r.validate().is_ok());
}

#[test]
fn a_base_killed_by_a_signal_is_not_a_red_result() {
    for base in [Ended::Signal(9), Ended::TimedOut, Ended::NotStarted, Ended::Unknown, Ended::Exit(126), Ended::Exit(127)] {
        let r = judged(base, Ended::Exit(0));
        assert_eq!(r.status, Status::Fail, "base {:?}", base);
        assert_eq!(r.failure_class.as_deref(), Some("base-error"), "base {:?}", base);
        assert_eq!(r.head_sha, HEAD);
        assert!(r.validate().is_ok());
    }
}

#[test]
fn a_check_still_red_at_head_is_fail() {
    let r = judge_reports(&reports(Ended::Exit(1), Ended::Exit(3), "line one\nMARKER-LINE\n[output cut]"), HEAD, ID).unwrap();
    assert_eq!(r.status, Status::Fail);
    assert_eq!(r.failure_class.as_deref(), Some("check-failed"));
    assert!(r.message.contains("MARKER-LINE"), "{}", r.message);
    assert_eq!(r.head_sha, HEAD);
    assert!(r.validate().is_ok());
}

#[test]
fn an_unknown_ending_at_head_is_never_a_pass() {
    for head in [Ended::Unknown, Ended::Signal(9), Ended::TimedOut, Ended::NotStarted] {
        let r = judged(Ended::Exit(1), head);
        assert_eq!(r.status, Status::Fail, "head {:?}", head);
        assert!(r.failure_class.is_some(), "head {:?}", head);
        assert_eq!(r.head_sha, HEAD, "head {:?}", head);
        assert!(r.validate().is_ok(), "head {:?}", head);
    }
}

#[test]
fn a_head_that_does_not_match_the_recorded_sha_is_refused() {
    let r = judge_reports(&reports(Ended::Exit(1), Ended::Exit(0), ""), BASE, ID);
    assert!(r.is_err(), "a head that is not the recorded sha produced a result: {:?}", r);
}

#[test]
fn malformed_shas_and_gate_ids_are_refused() {
    let bad = ["89ABCDEF0123456789ABCDEF0123456789ABCDEF", "89abcdef", "89abcdef0123456789abcdef0123456789abcdeg", ""];
    for sha in bad {
        // a malformed head, with the recorded sha equal to it so only the format check can refuse it
        let mut r = reports(Ended::Exit(1), Ended::Exit(0), "");
        r.head.sha = sha.to_string();
        assert!(judge_reports(&r, sha, ID).is_err(), "head sha {:?}", sha);
        // a malformed base
        let mut r = reports(Ended::Exit(1), Ended::Exit(0), "");
        r.base.sha = sha.to_string();
        assert!(judge_reports(&r, HEAD, ID).is_err(), "base sha {:?}", sha);
    }
    let mut same = reports(Ended::Exit(1), Ended::Exit(0), "");
    same.base.sha = HEAD.to_string();
    assert!(judge_reports(&same, HEAD, ID).is_err(), "base equal to head");
    for id in ["", "bad id!", "Upper"] {
        assert!(judge_reports(&reports(Ended::Exit(1), Ended::Exit(0), ""), HEAD, id).is_err(), "gate id {:?}", id);
    }
}

// ---------- end to end through the real tree preparation ----------

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hbe-e2e-{}-{}", name, std::process::id()));
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
    assert!(o.status.success(), "git failed: {:?} {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn put(dir: &Path, rel: &str, body: &str, mode: u32) {
    let f = dir.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, body).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A repo with a base commit (value.txt = "bad", no check file) and a head commit (value.txt = "good", the check file added). Returns (repo, base sha, head sha).
fn repo(name: &str, check: &str) -> (PathBuf, String, String) {
    let repo = scratch(name);
    git(&repo, &["init", "-q"]);
    put(&repo, "value.txt", "bad", 0o644);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    put(&repo, "value.txt", "good", 0o644);
    put(&repo, "ci/check.sh", check, 0o755);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    (repo, base, head)
}

fn only_the_main_tree(repo: &Path) {
    let list = git(repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(list.lines().filter(|l| l.starts_with("worktree ")).count(), 1, "stale worktrees:\n{}", list);
}

fn spec() -> Spec<'static> {
    Spec { id: ID, program: "ci/check.sh", args: &[], budget: Duration::from_secs(20), max_output: 4000 }
}

#[test]
fn end_to_end_through_real_tree_preparation_on_a_scratch_repo() {
    // red at base, green at head: the fix passes and the result names the real head sha
    let (repo_a, base_a, head_a) = repo("fix", "#!/bin/sh\ntest \"$(cat value.txt)\" = good\n");
    let r = judge(&repo_a, &base_a, &head_a, &head_a, &spec()).unwrap();
    assert_eq!(r.status, Status::Pass, "{:?} {}", r.failure_class, r.message);
    assert_eq!(r.head_sha, git(&repo_a, &["rev-parse", "HEAD"]));
    assert!(r.validate().is_ok());
    only_the_main_tree(&repo_a);

    // a check that already passes at base catches nothing
    let (repo_b, base_b, head_b) = repo("green", "#!/bin/sh\nexit 0\n");
    let r = judge(&repo_b, &base_b, &head_b, &head_b, &spec()).unwrap();
    assert_eq!(r.status, Status::Fail);
    assert_eq!(r.failure_class.as_deref(), Some("passes-at-base"));
    only_the_main_tree(&repo_b);

    // refusals from tree preparation pass through, no result is produced, and nothing is left behind
    let e = judge(&repo_a, &base_a, &head_a, &base_a, &spec()).unwrap_err();
    assert!(e.contains("recorded"), "{}", e);
    only_the_main_tree(&repo_a);
    let e = judge(&repo_a, "no-such-ref", &head_a, &head_a, &spec()).unwrap_err();
    assert!(e.contains("does not resolve"), "{}", e);
    only_the_main_tree(&repo_a);

    let _ = std::fs::remove_dir_all(repo_a);
    let _ = std::fs::remove_dir_all(repo_b);
}
