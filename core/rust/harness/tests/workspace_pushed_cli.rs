//! Process-level tests for `harness workspace check --pushed` (P1B-12), one per plan case. Every check runs
//! the real binary as a child process against a scratch workspace made by the real `harness workspace create`,
//! with a local bare repo as the remote (pushed over a file path, no network), so the exit-code contract
//! (0 pushed, 4 unpushed naming each commit, 3 cannot check), the fail-closed refusals (no upstream, a
//! symbolic tracking ref, grafts/shallow, another repo, odd `.git` shapes) and the read-only promise are all
//! exercised end to end. No test asserts on elapsed time or sleeps.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// One scratch layout: the RAW temp path (what a caller would type) and its canonical spelling are both kept,
/// because on this platform they differ and neither may ever appear on stdout.
struct Tmp {
    raw: PathBuf,
    canon: PathBuf,
}

impl Tmp {
    fn new() -> Tmp {
        let raw = std::env::temp_dir().join(format!("wpsh-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&raw);
        fs::create_dir_all(&raw).unwrap();
        let canon = raw.canonicalize().unwrap();
        Tmp { raw, canon }
    }

    /// Both spellings of a child path (the raw one is passed on the command line; the canonical one is what git prints).
    fn both(&self, rel: &str) -> (PathBuf, PathBuf) {
        (self.raw.join(rel), self.canon.join(rel))
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.raw);
        let _ = fs::remove_dir_all(&self.canon);
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

/// A valid ticket file for `id` naming repo `repo_id` (the strict reader's eleven fields, all legal values).
fn ticket(t: &Tmp, id: &str, repo_id: &str) -> PathBuf {
    let (f, _) = t.both(&format!("{id}.json"));
    fs::write(&f, format!("{{\"id\":\"{id}\",\"repo_id\":\"{repo_id}\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"M\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}")).unwrap();
    f
}

/// A scratch repo with one tracked file and one commit, upkeep off (a background gc would flake snapshots).
fn repo(t: &Tmp, name: &str, msg: &str) -> PathBuf {
    let (d, _) = t.both(name);
    fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        git(&d, &["config", k, v]);
    }
    fs::write(d.join("f.txt"), msg).unwrap();
    git(&d, &["add", "f.txt"]);
    git(&d, &["commit", "-q", "-m", msg]);
    d.canonicalize().unwrap()
}

/// A local bare repo to push to over a file path (the remote; no network anywhere in this file).
fn bare(t: &Tmp, name: &str) -> PathBuf {
    let (d, _) = t.both(name);
    fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q", "--bare"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0")] {
        git(&d, &["config", k, v]);
    }
    d
}

fn run(args: &[String], cwd: Option<&Path>) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
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

/// One scratch workspace: a repo `R`, folders `W`/`M`, a ticket `t1`, and the real `workspace create` run on
/// it. Returns (repo raw path, tree raw path). The tree sits on branch `ws/t1` as a linked worktree of `R`.
fn mk_ws(t: &Tmp) -> (PathBuf, PathBuf) {
    let (r_raw, _) = t.both("R");
    let r = repo(t, "R", "one");
    let s = head(&r);
    ticket(t, "t1", "r");
    for d in ["W", "M"] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    let (tf, _) = t.both("t1.json");
    let args: Vec<String> = [
        "workspace",
        "create",
        tf.to_str().unwrap(),
        "--repo-map",
        &format!("r={}", r_raw.display()),
        "--base",
        &s,
        "--workspace-root",
        t.raw.join("W").to_str().unwrap(),
        "--manifest-dir",
        t.raw.join("M").to_str().unwrap(),
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    let o = run(&args, None);
    assert_eq!(code(&o), 0, "create: {}", err(&o));
    let (tree, _) = t.both("W/t1/tree");
    (r_raw, tree)
}

/// One more commit on `ws/t1` in the ticket worktree; returns the new commit's full sha.
fn commit(tree: &Path, name: &str, msg: &str) -> String {
    fs::write(tree.join(name), msg).unwrap();
    git(tree, &["add", name]);
    git(tree, &["commit", "-q", "-m", msg]);
    head(tree)
}

/// A workspace whose branch `ws/t1` is pushed to a local bare remote with `-u` (upstream configured, tracking
/// ref present). Returns (repo raw path, tree, remote raw path).
fn ws_pushed(t: &Tmp) -> (PathBuf, PathBuf, PathBuf) {
    let (r_raw, tree) = mk_ws(t);
    let remote = bare(t, "origin.git");
    git(&tree, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&tree, &["push", "-q", "-u", "origin", "ws/t1"]);
    (r_raw, tree, remote)
}

/// The full `workspace check --pushed` line for ticket `t1` against `repo`, run from an unrelated folder (the
/// check must never use the current folder).
fn pushed_run(t: &Tmp, repo: &Path) -> Output {
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let args: Vec<String> = [
        "workspace".to_string(),
        "check".to_string(),
        "t1".to_string(),
        "--manifest-dir".to_string(),
        t.raw.join("M").to_str().unwrap().to_string(),
        "--pushed".to_string(),
        "--repo".to_string(),
        repo.to_str().unwrap().to_string(),
    ]
    .to_vec();
    run(&args, Some(&unrelated))
}

/// The same line with arbitrary extra/absent option pairs, for the usage cases.
fn pushed_args(t: &Tmp, extra: &[&str]) -> Output {
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let mut args: Vec<String> = vec![
        "workspace".into(),
        "check".into(),
        "t1".into(),
        "--manifest-dir".into(),
        t.raw.join("M").to_str().unwrap().into(),
    ];
    for x in extra {
        args.push(x.to_string());
    }
    run(&args, Some(&unrelated))
}

/// Exit 3 with empty stdout and a fixed folder-free stderr line.
fn assert_refused(label: &str, o: &Output, needle: &str) {
    assert_eq!(code(o), 3, "{label}: exit {}\nstdout: {}\nstderr: {}", code(o), out(o), err(o));
    assert_eq!(out(o), "", "{label}: stdout must be empty on exit 3");
    assert!(err(o).starts_with("harness: error: "), "{label}: stderr must be the fixed error line: {}", err(o));
    assert!(err(o).contains(needle), "{label}: stderr must say `{needle}`: {}", err(o));
}

/// Neither stdout nor stderr may carry the raw or canonical spelling of the temp folder, the worktree, the
/// repo or the remote.
fn assert_no_folder(label: &str, o: &Output, t: &Tmp) {
    let text = format!("{}{}", out(o), err(o));
    let probes = [
        t.raw.to_string_lossy().into_owned(),
        t.canon.to_string_lossy().into_owned(),
        t.raw.join("W/t1/tree").to_string_lossy().into_owned(),
        t.canon.join("W/t1/tree").to_string_lossy().into_owned(),
        t.raw.join("R").to_string_lossy().into_owned(),
        t.canon.join("R").to_string_lossy().into_owned(),
        t.raw.join("origin.git").to_string_lossy().into_owned(),
        t.canon.join("origin.git").to_string_lossy().into_owned(),
    ];
    for probe in probes {
        assert!(!text.contains(&probe), "{label}: output carries a folder text");
    }
}

// ---------------------------------------------------------------- test 1: pushed passes

#[test]
fn pushed_clean_passes() {
    let t = Tmp::new();
    let (r_raw, _, _) = ws_pushed(&t);
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 0, "stdout: {}\nstderr: {}", out(&o), err(&o));
    let ls = lines(&o);
    assert_eq!(ls.len(), 1, "exactly one line: {:?}", ls);
    assert_eq!(ls[0], "workspace=pushed ticket=t1 branch=ws/t1 upstream=refs/remotes/origin/ws/t1");
    assert_no_folder("pushed_clean_passes", &o, &t);
}

// ---------------------------------------------------------------- test 2: unpushed commit named

#[test]
fn unpushed_commit_fails_naming_it() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    let c = commit(&tree, "c.txt", "C");
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 4, "stdout: {}\nstderr: {}", out(&o), err(&o));
    let ls = lines(&o);
    assert_eq!(ls.len(), 2, "lines: {:?}", ls);
    assert_eq!(ls[0], format!("unpushed {} ws/t1", c));
    assert_eq!(ls[1], "workspace=unpushed ticket=t1 branch=ws/t1 upstream=refs/remotes/origin/ws/t1 count=1");
    assert_no_folder("unpushed_commit_fails_naming_it", &o, &t);
    git(&tree, &["push", "-q", "origin", "ws/t1"]);
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 0, "after push: stdout: {}\nstderr: {}", out(&o), err(&o));
    assert_eq!(out(&o), "workspace=pushed ticket=t1 branch=ws/t1 upstream=refs/remotes/origin/ws/t1\n");
    assert_no_folder("unpushed_commit_fails_naming_it-after", &o, &t);
}

// ---------------------------------------------------------------- test 3: two commits, newest first

#[test]
fn two_unpushed_listed_newest_first() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    let a = commit(&tree, "a.txt", "A");
    let b = commit(&tree, "b.txt", "B");
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 4, "stdout: {}", out(&o));
    let ls = lines(&o);
    assert_eq!(ls.len(), 3, "lines: {:?}", ls);
    assert_eq!(ls[0], format!("unpushed {} ws/t1", b), "newest first: {:?}", ls);
    assert_eq!(ls[1], format!("unpushed {} ws/t1", a), "oldest second: {:?}", ls);
    assert_eq!(ls[2], "workspace=unpushed ticket=t1 branch=ws/t1 upstream=refs/remotes/origin/ws/t1 count=2");
    assert_no_folder("two_unpushed_listed_newest_first", &o, &t);
}

// ---------------------------------------------------------------- test 4: no upstream fails closed

#[test]
fn no_upstream_fails_closed() {
    let t = Tmp::new();
    let (r_raw, _) = mk_ws(&t);
    let o = pushed_run(&t, &r_raw);
    assert_refused("no_upstream_fails_closed", &o, "the ticket branch has no upstream");
    assert_no_folder("no_upstream_fails_closed", &o, &t);
}

// ---------------------------------------------------------------- test 5: tracking ref deleted

#[test]
fn upstream_configured_but_tracking_ref_missing_fails_closed() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    git(&tree, &["update-ref", "-d", "refs/remotes/origin/ws/t1"]);
    let o = pushed_run(&t, &r_raw);
    assert_refused("tracking_ref_missing", &o, "no upstream");
    assert_no_folder("tracking_ref_missing", &o, &t);
}

// ---------------------------------------------------------------- test 6: local branch as upstream

#[test]
fn local_branch_as_upstream_refused() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    git(&tree, &["config", "branch.ws/t1.remote", "."]);
    let o = pushed_run(&t, &r_raw);
    assert_refused("local_branch_as_upstream", &o, "not a remote-tracking ref");
    assert_no_folder("local_branch_as_upstream", &o, &t);
}

// ---------------------------------------------------------------- test 7: symbolic tracking ref

#[test]
fn symref_upstream_refused() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    commit(&tree, "c.txt", "C");
    // The tracking ref becomes a symbolic ref pointing at the branch itself: without the symref rule the
    // upstream sha equals the branch tip and the unpushed commit C would pass.
    git(&tree, &["symbolic-ref", "refs/remotes/origin/ws/t1", "refs/heads/ws/t1"]);
    let o = pushed_run(&t, &r_raw);
    assert_refused("symref_to_branch", &o, "not a remote-tracking ref");
    assert_no_folder("symref_to_branch", &o, &t);
    // A symref to ANOTHER remote-tracking ref passes the prefix rule and must be refused by the symref rule
    // itself: without it the upstream sha would be `origin/other`'s and the unpushed commit C would pass.
    let base = git(&tree, &["rev-parse", "ws/t1~1"]);
    git(&tree, &["push", "-q", "origin", &format!("{}:refs/heads/other", base)]);
    git(&tree, &["symbolic-ref", "refs/remotes/origin/ws/t1", "refs/remotes/origin/other"]);
    let o = pushed_run(&t, &r_raw);
    assert_refused("symref_upstream_refused", &o, "symbolic");
    assert_no_folder("symref_upstream_refused", &o, &t);
}

// ---------------------------------------------------------------- test 8: detached HEAD

#[test]
fn detached_head_fails_closed() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    git(&tree, &["checkout", "-q", "--detach"]);
    let o = pushed_run(&t, &r_raw);
    assert_refused("detached_head", &o, "a git call in the worktree failed");
    assert_no_folder("detached_head", &o, &t);
}

// ---------------------------------------------------------------- test 9: force-moved upstream

#[test]
fn force_moved_upstream_fails() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    let base = head(&tree);
    let a = commit(&tree, "a.txt", "A");
    let b = commit(&tree, "b.txt", "B");
    let every: Vec<String> = vec![
        format!("unpushed {} ws/t1", b),
        format!("unpushed {} ws/t1", a),
        "workspace=unpushed ticket=t1 branch=ws/t1 upstream=refs/remotes/origin/ws/t1 count=2".to_string(),
    ];
    // Moved back to the base commit (an old shared ancestor): every ticket commit is no longer contained.
    git(&tree, &["update-ref", "refs/remotes/origin/ws/t1", &base]);
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 4, "moved to base: stdout: {}", out(&o));
    assert_eq!(lines(&o), every, "moved to base must name every ticket commit");
    assert_no_folder("force_moved_base", &o, &t);
    // Moved to an unrelated sibling commit (same base, no ticket commits): same answer.
    let tree_of_base = git(&tree, &["rev-parse", &format!("{base}^{{tree}}")]);
    let sibling = git(&tree, &["commit-tree", &tree_of_base, "-p", &base, "-m", "sibling"]);
    git(&tree, &["update-ref", "refs/remotes/origin/ws/t1", &sibling]);
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 4, "moved to sibling: stdout: {}", out(&o));
    assert_eq!(lines(&o), every, "moved to sibling must name every ticket commit");
    assert_no_folder("force_moved_sibling", &o, &t);
}

// ---------------------------------------------------------------- test 10: upstream ahead passes

#[test]
fn upstream_ahead_passes() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    let tip = head(&tree);
    let tree_of_tip = git(&tree, &["rev-parse", &format!("{tip}^{{tree}}")]);
    let ahead = git(&tree, &["commit-tree", &tree_of_tip, "-p", &tip, "-m", "ahead"]);
    git(&tree, &["update-ref", "refs/remotes/origin/ws/t1", &ahead]);
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 0, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert_eq!(out(&o), "workspace=pushed ticket=t1 branch=ws/t1 upstream=refs/remotes/origin/ws/t1\n");
    assert_no_folder("upstream_ahead_passes", &o, &t);
}

// ---------------------------------------------------------------- test 11: another repo

#[test]
fn worktree_of_other_repo_refused() {
    let t = Tmp::new();
    let (_, _, _) = ws_pushed(&t);
    repo(&t, "R2", "other");
    let (r2_raw, _) = t.both("R2");
    // R2 is clean but the worktree under check belongs to R, not R2.
    let o = pushed_run(&t, &r2_raw);
    assert_refused("other_repo", &o, "the worktree does not belong to --repo");
    assert_no_folder("other_repo", &o, &t);
}

// ---------------------------------------------------------------- test 12: .git shapes

#[test]
fn dot_git_shapes_refused() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);

    // (a) the worktree pointer file replaced by a symlink to the very same pointer file.
    let dg = tree.join(".git");
    let backup = tree.join("_pointer_backup");
    let pointer = fs::read_to_string(&dg).unwrap();
    fs::rename(&dg, &backup).unwrap();
    symlink(&backup, &dg).unwrap();
    let o = pushed_run(&t, &r_raw);
    assert_refused("dot_git_symlink", &o, "not a linked worktree");
    assert_no_folder("dot_git_symlink", &o, &t);
    fs::remove_file(&dg).unwrap();
    fs::rename(&backup, &dg).unwrap();

    // (b) the worktree pointer file replaced by a directory.
    fs::remove_file(&dg).unwrap();
    fs::create_dir(&dg).unwrap();
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert_eq!(out(&o), "", "stdout must be empty on exit 3");
    assert_no_folder("dot_git_dir", &o, &t);
    fs::remove_dir(&dg).unwrap();
    fs::write(&dg, pointer).unwrap();

    // (c) --repo passed through a copy folder whose .git is a symlink to the real git dir.
    let (rc_raw, rc_canon) = t.both("Rcopy");
    fs::create_dir_all(&rc_raw).unwrap();
    symlink(rc_canon.join("R/.git"), rc_raw.join(".git")).unwrap();
    let o = pushed_run(&t, &rc_raw);
    assert_refused("repo_git_symlink", &o, "--repo is not a main work tree");
    assert_no_folder("repo_git_symlink", &o, &t);
}

// ---------------------------------------------------------------- test 13: grafts and shallow

#[test]
fn grafts_refused() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    let c = commit(&tree, "c.txt", "C");
    let common = PathBuf::from(git(&tree, &["rev-parse", "--path-format=absolute", "--git-common-dir"]));
    fs::create_dir_all(common.join("info")).unwrap();
    let tracking = git(&tree, &["rev-parse", "refs/remotes/origin/ws/t1"]);
    // The graft makes the tracking commit a CHILD of the unpushed commit, so a plain reachability walk
    // would exclude C and pass; the file's presence alone must be refused.
    fs::write(common.join("info").join("grafts"), format!("{} {}\n", tracking, c)).unwrap();
    let o = pushed_run(&t, &r_raw);
    assert_refused("grafts", &o, "info/grafts");
    assert_no_folder("grafts", &o, &t);
    fs::remove_file(common.join("info").join("grafts")).unwrap();
    let base = git(&tree, &["rev-parse", "ws/t1~1"]);
    fs::write(common.join("shallow"), format!("{}\n", base)).unwrap();
    let o = pushed_run(&t, &r_raw);
    assert_refused("shallow", &o, "shallow");
    assert_no_folder("shallow", &o, &t);
}

// ---------------------------------------------------------------- test 14: usage pairs

#[test]
fn usage_pairs() {
    let t = Tmp::new();
    let (r_raw, _) = mk_ws(&t);
    for (label, extra, needle) in [
        ("pushed alone", vec!["--pushed"], "--pushed requires --repo"),
        ("repo alone", vec!["--repo", r_raw.to_str().unwrap()], "--repo requires --pushed"),
        ("relative repo", vec!["--pushed", "--repo", "R"], "must be an absolute path"),
        ("repo twice", vec!["--pushed", "--repo", r_raw.to_str().unwrap(), "--repo", r_raw.to_str().unwrap()], "--repo was given twice"),
        ("pushed twice", vec!["--pushed", "--repo", r_raw.to_str().unwrap(), "--pushed"], "--pushed was given twice"),
    ] {
        let o = pushed_args(&t, &extra);
        assert_eq!(code(&o), 3, "{label}: exit {}\nstdout: {}\nstderr: {}", code(&o), out(&o), err(&o));
        assert_eq!(out(&o), "", "{label}: stdout must be empty on a usage error");
        assert!(err(&o).starts_with("harness: error: "), "{label}: fixed error line: {}", err(&o));
        assert!(err(&o).contains(needle), "{label}: stderr must say `{needle}`: {}", err(&o));
        assert!(err(&o).contains("usage:"), "{label}: stderr must carry the usage line: {}", err(&o));
        assert_no_folder(label, &o, &t);
    }
}

// ---------------------------------------------------------------- test 15: no folder printed anywhere

#[test]
fn no_folder_printed() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    // A spread of outcomes: pushed (0), unpushed (4) and each refusal class (3) — none may print a folder.
    let pushed_case = pushed_run(&t, &r_raw);
    assert_eq!(code(&pushed_case), 0, "the pushed case is the outcome checked: {}", out(&pushed_case));
    assert_no_folder("no_folder_printed pushed", &pushed_case, &t);
    commit(&tree, "x.txt", "X");
    let unpushed_case = pushed_run(&t, &r_raw);
    assert_eq!(code(&unpushed_case), 4, "the unpushed case is the outcome checked: {}", out(&unpushed_case));
    assert_no_folder("no_folder_printed unpushed", &unpushed_case, &t);
    git(&tree, &["symbolic-ref", "refs/remotes/origin/ws/t1", "refs/heads/ws/t1"]);
    let symref_case = pushed_run(&t, &r_raw);
    assert_eq!(code(&symref_case), 3, "the symref case is the outcome checked: {}", err(&symref_case));
    assert_no_folder("no_folder_printed symref", &symref_case, &t);
    let (r2_raw, _) = t.both("R2");
    repo(&t, "R2", "other");
    let other_repo_case = pushed_run(&t, &r2_raw);
    assert_eq!(code(&other_repo_case), 3, "the other-repo case is the outcome checked: {}", err(&other_repo_case));
    assert_no_folder("no_folder_printed other repo", &other_repo_case, &t);
    let usage_case = pushed_args(&t, &["--pushed"]);
    assert_eq!(code(&usage_case), 3, "the usage case is the outcome checked: {}", err(&usage_case));
    assert_no_folder("no_folder_printed usage", &usage_case, &t);
}

// ---------------------------------------------------------------- test 16: read-only

#[test]
fn read_only() {
    let t = Tmp::new();
    let (r_raw, tree, _) = ws_pushed(&t);
    commit(&tree, "c.txt", "C");
    let common = PathBuf::from(git(&tree, &["rev-parse", "--path-format=absolute", "--git-common-dir"]));
    let gitdir = PathBuf::from(git(&tree, &["rev-parse", "--path-format=absolute", "--absolute-git-dir"]));
    let refs_before = git(&tree, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]);
    let config_before = fs::read(common.join("config")).unwrap();
    let index_before = fs::read(gitdir.join("index")).unwrap();
    let o = pushed_run(&t, &r_raw);
    assert_eq!(code(&o), 4, "the unpushed case is the one observed: stdout: {}", out(&o));
    let refs_after = git(&tree, &["for-each-ref", "--format=%(refname) %(objectname) %(symref)"]);
    assert_eq!(refs_before, refs_after, "the ref dump changed across the run");
    assert_eq!(config_before, fs::read(common.join("config")).unwrap(), "the repo config changed across the run");
    assert_eq!(index_before, fs::read(gitdir.join("index")).unwrap(), "the index changed across the run");
    assert_no_folder("read_only", &o, &t);
}
