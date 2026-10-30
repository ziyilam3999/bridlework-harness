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
    assert_eq!(r.status, Status::Pass, "{:?} {}", r.failure_class, r.message);
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
    assert_eq!(r.status, Status::Pass, "{:?} {}", r.failure_class, r.message);
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

#[test]
fn base_equal_to_head_is_refused() {
    let (d, _base, head) = repo("same", "bad", "good", true);
    assert!(both_ends(&d, &head, &head, &head, &spec()).is_err());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_check_cannot_tell_base_from_head_by_its_path() {
    let (d, base, head) = repo("path", "bad", "good", true);
    let f = d.join("ci/check.sh");
    std::fs::write(&f, "#!/bin/sh\ncase \"$PWD\" in *base*) exit 1;; esac\nexit 0\n").unwrap();
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "keyed on path"]);
    let head2 = git(&d, &["rev-parse", "HEAD"]);
    let r = both_ends(&d, &base, &head2, &head2, &spec()).unwrap();
    assert_ne!(head, head2);
    assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("passes-at-base")));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_helper_missing_at_base_is_not_a_red_result() {
    let d = scratch("helper");
    git(&d, &["init", "-q"]);
    std::fs::write(d.join("value.txt"), "same").unwrap();
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "base"]);
    let base = git(&d, &["rev-parse", "HEAD"]);
    // head adds a helper that exits 0 and a check that runs it: nothing is actually fixed.
    std::fs::create_dir_all(d.join("ci")).unwrap();
    std::fs::write(d.join("ci/check.sh"), "#!/bin/sh\nexec ./ci/helper.sh\n").unwrap();
    std::fs::write(d.join("ci/helper.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    for f in ["ci/check.sh", "ci/helper.sh"] {
        std::fs::set_permissions(d.join(f), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "head"]);
    let head = git(&d, &["rev-parse", "HEAD"]);
    let r = both_ends(&d, &base, &head, &head, &spec()).unwrap();
    assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("base-error")));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_symlinked_check_folder_at_base_never_writes_outside_the_tree() {
    let d = scratch("link");
    let outside = scratch("link-outside");
    std::fs::write(outside.join("check.sh"), "victim\n").unwrap();
    git(&d, &["init", "-q"]);
    std::os::unix::fs::symlink(&outside, d.join("ci")).unwrap();
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "base"]);
    let base = git(&d, &["rev-parse", "HEAD"]);
    std::fs::remove_file(d.join("ci")).unwrap();
    write_check(&d);
    std::fs::write(d.join("value.txt"), "good").unwrap();
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "head"]);
    let head = git(&d, &["rev-parse", "HEAD"]);
    assert!(both_ends(&d, &base, &head, &head, &spec()).is_err());
    assert_eq!(std::fs::read_to_string(outside.join("check.sh")).unwrap(), "victim\n");
    assert_eq!(git(&d, &["worktree", "list"]).lines().count(), 1);
    let _ = std::fs::remove_dir_all(d);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn a_dot_dot_check_path_never_writes_outside_the_trees() {
    let d = scratch("dots");
    let outside = scratch("dots-outside");
    let decoy = outside.join("decoy.txt");
    std::fs::write(&decoy, "victim\n").unwrap();
    // The decoy's absolute path without its leading slash, so it also exists as a relative file inside head.
    let abs = decoy.to_str().unwrap().to_string();
    let rel = abs.trim_start_matches('/').to_string();

    git(&d, &["init", "-q"]);
    std::fs::write(d.join("value.txt"), "bad").unwrap();
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "base"]);
    let base = git(&d, &["rev-parse", "HEAD"]);

    // Head: `a` is a symlink to a 30-deep folder inside head, so thirty dot-dots from `a` land back at the head root, and the relative decoy path exists there.
    let deep = (0..30).map(|_| "d").collect::<Vec<_>>().join("/");
    std::fs::create_dir_all(d.join(&deep)).unwrap();
    std::fs::write(d.join(&deep).join("keep"), "x").unwrap();
    std::os::unix::fs::symlink(&deep, d.join("a")).unwrap();
    let target = d.join(&rel);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "head"]);
    let head = git(&d, &["rev-parse", "HEAD"]);

    let program = format!("a/{}{}", "../".repeat(30), rel);
    let mut s = spec();
    s.program = &program;
    assert!(both_ends(&d, &base, &head, &head, &s).is_err());
    assert_eq!(std::fs::read_to_string(&decoy).unwrap(), "victim\n");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(git(&d, &["worktree", "list"]).lines().count(), 1);
    let _ = std::fs::remove_dir_all(d);
    let _ = std::fs::remove_dir_all(outside);
}
