//! Process-level tests for `harness plan fresh --index <file>`, one per plan case. Every case runs the
//! real binary as a child process against a scratch layout with a local bare remote and two clones
//! (pushed over a file path, no network), so the exit-code contract (0 current, 1 behind or diverged,
//! 2 cannot judge), the local-refs-only promise (an unreachable remote URL changes nothing), the
//! symlink and linked-worktree rules, and the fixed engine stderr lines are exercised end to end.
//! The first clone fetches its tracking ref only where a case needs it, in that case's setup.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// One row of a plan index; the verb reads the file's place, not its content.
const ROW: &str = "{\"id\":\"r1\",\"title\":\"Row one\",\"plan_line\":1,\"state\":\"built\",\"tickets\":[],\"deviation\":null,\"trigger\":null,\"code\":[]}\n";

struct Tmp {
    raw: PathBuf,
}

impl Tmp {
    fn new() -> Tmp {
        let raw = std::env::temp_dir().join(format!("pfresh-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&raw);
        fs::create_dir_all(&raw).unwrap();
        Tmp { raw }
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.raw);
    }
}

/// One git call in a scratch repo: no hooks, no inherited repo-redirecting names, no stdin, fixed identity.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn head(d: &Path) -> String {
    git(d, &["rev-parse", "HEAD"])
}

/// The first 12 hex characters of a full sha, as the verb prints them.
fn short(sha: &str) -> String {
    sha[..12].to_string()
}

/// One scratch layout: a bare remote `remote.git` on branch `main` with one commit, a seed repo that
/// pushed it, and two clones `c1` and `c2` (each with an untracked `PLAN_INDEX` at its root). A clone
/// sets `refs/remotes/origin/HEAD` to `refs/remotes/origin/main`, so no case needs the network.
struct Fix {
    root: PathBuf,
    c1: PathBuf,
    c2: PathBuf,
    index1: PathBuf,
    _t: Tmp,
}

fn fix() -> Fix {
    let t = Tmp::new();
    let bare = t.raw.join("remote.git");
    fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        git(&bare, &["config", k, v]);
    }
    let seed = t.raw.join("seed");
    fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        git(&seed, &["config", k, v]);
    }
    fs::write(seed.join("f.txt"), "one\n").unwrap();
    git(&seed, &["add", "f.txt"]);
    git(&seed, &["commit", "-q", "-m", "one"]);
    git(&seed, &["push", "-q", bare.to_str().unwrap(), "main"]);
    let c1 = t.raw.join("c1");
    let c2 = t.raw.join("c2");
    git(&t.raw, &["clone", "-q", bare.to_str().unwrap(), c1.to_str().unwrap()]);
    git(&t.raw, &["clone", "-q", bare.to_str().unwrap(), c2.to_str().unwrap()]);
    for c in [&c1, &c2] {
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(c, &["config", k, v]);
        }
    }
    let index1 = c1.join("PLAN_INDEX");
    fs::write(&index1, ROW).unwrap();
    fs::write(c2.join("PLAN_INDEX"), ROW).unwrap();
    Fix { root: t.raw.clone(), c1, c2, index1, _t: t }
}

/// One more commit on `main` in a clone; returns the new commit's full sha.
fn commit(d: &Path, name: &str, msg: &str) -> String {
    fs::write(d.join(name), msg).unwrap();
    git(d, &["add", name]);
    git(d, &["commit", "-q", "-m", msg]);
    head(d)
}

/// The remote default branch as the clone records it, full sha.
fn base(d: &Path) -> String {
    git(d, &["rev-parse", "refs/remotes/origin/main"])
}

fn fresh(index: &str, cwd: Option<&Path>) -> Output {
    let mut c = Command::new(BIN);
    c.args(["plan", "fresh", "--index", index]).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn lines(o: &Output) -> Vec<String> {
    out(o).lines().map(|l| l.to_string()).collect()
}

#[test]
fn current_head_equals_base_exit_zero() {
    let f = fix();
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let line = format!("plan-index current head={} base={}", short(&head(&f.c1)), short(&base(&f.c1)));
    assert_eq!(lines(&o), vec![line]);
    assert_eq!(err(&o), "");
}

#[test]
fn behind_by_two_exit_one() {
    let f = fix();
    commit(&f.c2, "a.txt", "a");
    commit(&f.c2, "b.txt", "b");
    git(&f.c2, &["push", "-q", "origin", "main"]);
    git(&f.c1, &["fetch", "-q", "origin"]);
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    let line = format!("plan-index behind=2 head={} base={}", short(&head(&f.c1)), short(&base(&f.c1)));
    assert_eq!(lines(&o), vec![line]);
    assert_eq!(err(&o), "");
}

#[test]
fn ahead_only_exit_zero() {
    let f = fix();
    commit(&f.c1, "local.txt", "local");
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let line = format!("plan-index current head={} base={}", short(&head(&f.c1)), short(&base(&f.c1)));
    assert_eq!(lines(&o), vec![line]);
    assert_eq!(err(&o), "");
}

#[test]
fn diverged_ahead_one_behind_one_exit_one() {
    let f = fix();
    commit(&f.c1, "local.txt", "local");
    commit(&f.c2, "r.txt", "r");
    git(&f.c2, &["push", "-q", "origin", "main"]);
    git(&f.c1, &["fetch", "-q", "origin"]);
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    let line = format!("plan-index diverged ahead=1 behind=1 head={} base={}", short(&head(&f.c1)), short(&base(&f.c1)));
    assert_eq!(lines(&o), vec![line]);
    assert_eq!(err(&o), "");
}

#[test]
fn dirty_tree_and_current_exit_zero() {
    let f = fix();
    fs::write(f.c1.join("f.txt"), "changed\n").unwrap();
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let line = format!("plan-index current head={} base={}", short(&head(&f.c1)), short(&base(&f.c1)));
    assert_eq!(lines(&o), vec![line]);
    assert_eq!(err(&o), "");
}

#[test]
fn on_another_branch_exit_two() {
    let f = fix();
    git(&f.c1, &["checkout", "-q", "-b", "other"]);
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the current branch is not the branch origin/HEAD names\n");
}

#[test]
fn origin_head_unset_exit_two() {
    let f = fix();
    git(&f.c1, &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"]);
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    // Exact equality: git's own stderr of the failed symbolic-ref read is not passed through.
    assert_eq!(err(&o), "harness: error: the remote default branch ref refs/remotes/origin/HEAD is not set\n");
}

#[test]
fn detached_head_exit_two() {
    let f = fix();
    git(&f.c1, &["checkout", "-q", "--detach"]);
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    // Exact equality: git's own stderr of the failed HEAD read is not passed through.
    assert_eq!(err(&o), "harness: error: HEAD is detached\n");
}

#[test]
fn file_outside_any_repo_exit_two() {
    let f = fix();
    let plain = f.root.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let p = plain.join("PLAN_INDEX");
    fs::write(&p, ROW).unwrap();
    let o = fresh(p.to_str().unwrap(), None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the plan index is not inside a git work tree\n");
}

#[test]
fn relative_index_exit_two() {
    let f = fix();
    // The file exists relative to the working folder, so only the absolute-path rule refuses it.
    let o = fresh("PLAN_INDEX", Some(&f.c1));
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "usage: harness plan fresh --index <absolute-file>\n");
}

#[test]
fn dot_dot_in_index_exit_two() {
    let f = fix();
    let p = format!("{}/../PLAN_INDEX", f.c1.display());
    let o = fresh(&p, None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "usage: harness plan fresh --index <absolute-file>\n");
}

#[test]
fn index_through_a_symlink_matches_its_target() {
    let f = fix();
    // The link sits outside the clone; the verb answers about the work tree of the file it points at.
    let link = f.root.join("link-index");
    symlink(&f.index1, &link).unwrap();
    let o = fresh(link.to_str().unwrap(), None);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let line = format!("plan-index current head={} base={}", short(&head(&f.c1)), short(&base(&f.c1)));
    assert_eq!(lines(&o), vec![line]);
    assert_eq!(err(&o), "");
}

#[test]
fn index_in_a_linked_worktree_works() {
    let f = fix();
    // The main checkout moves to a side branch, so `main` is free to be checked out in a linked worktree.
    git(&f.c1, &["checkout", "-q", "-b", "side"]);
    let wt = f.root.join("wt");
    git(&f.c1, &["worktree", "add", "-q", wt.to_str().unwrap(), "main"]);
    let p = wt.join("PLAN_INDEX");
    fs::write(&p, ROW).unwrap();
    let o = fresh(p.to_str().unwrap(), None);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let line = format!("plan-index current head={} base={}", short(&head(&wt)), short(&base(&wt)));
    assert_eq!(lines(&o), vec![line]);
    assert_eq!(err(&o), "");
}

#[test]
fn an_unreachable_remote_url_still_answers_from_local_refs() {
    let f = fix();
    commit(&f.c2, "a.txt", "a");
    commit(&f.c2, "b.txt", "b");
    git(&f.c2, &["push", "-q", "origin", "main"]);
    git(&f.c1, &["fetch", "-q", "origin"]);
    git(&f.c1, &["remote", "set-url", "origin", f.root.join("no-such-remote.git").to_str().unwrap()]);
    let o = fresh(f.index1.to_str().unwrap(), None);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    let line = format!("plan-index behind=2 head={} base={}", short(&head(&f.c1)), short(&base(&f.c1)));
    assert_eq!(lines(&o), vec![line]);
    assert_eq!(err(&o), "");
}
