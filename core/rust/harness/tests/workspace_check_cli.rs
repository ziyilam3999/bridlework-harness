//! Process-level tests for `harness workspace check` (P1B-11), one per plan case. Every check runs the real
//! binary as a child process against a scratch workspace made by the real `harness workspace create`, so the
//! exit-code contract (0 clean, 2 stray, 3 cannot check), the manifest-driven worktree lookup, the
//! expected-vs-stray rules and the read-only promise are all exercised end to end.
#![cfg(unix)]
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
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
        let raw = std::env::temp_dir().join(format!("wchk-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

/// The same call, allowed to fail (a conflicted merge is the point, not a bug).
fn git_try(dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    c.stdin(Stdio::null()).output().unwrap()
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

fn head(d: &Path) -> String {
    git(d, &["rev-parse", "HEAD"])
}

/// A valid ticket file for `id` naming repo `repo_id` (the strict reader's eleven fields, all legal values).
fn ticket(t: &Tmp, id: &str, repo_id: &str) -> PathBuf {
    let (f, _) = t.both(&format!("{id}.json"));
    fs::write(&f, format!("{{\"id\":\"{id}\",\"repo_id\":\"{repo_id}\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"M\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}")).unwrap();
    f
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

/// One scratch workspace: a repo `R`, folders `W`/`M`, a ticket `t1`, and the real `workspace create` run on
/// it. Returns (repo, tree raw path, manifest file raw path). `with_node_modules` plants a real primary
/// `node_modules` in the repo BEFORE create (the P1B-04 link case).
fn mk_ws(t: &Tmp, with_node_modules: bool) -> (PathBuf, PathBuf, PathBuf) {
    let (r_raw, _) = t.both("R");
    let r = repo(t, "R", "one");
    if with_node_modules {
        fs::create_dir_all(r.join("node_modules/pkg")).unwrap();
        fs::write(r.join("node_modules/pkg/x.js"), "x").unwrap();
    }
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
    let (m, _) = t.both("M/t1.jsonl");
    (r, tree, m)
}

/// The full `workspace check` line for ticket `t1`, run from an unrelated folder (the check must never use the
/// current folder).
fn check(t: &Tmp) -> Output {
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let args: Vec<String> = [
        "workspace".to_string(),
        "check".to_string(),
        "t1".to_string(),
        "--manifest-dir".to_string(),
        t.raw.join("M").to_str().unwrap().to_string(),
    ]
    .to_vec();
    run(&args, Some(&unrelated))
}

fn stray_lines(o: &Output) -> Vec<String> {
    out(o).lines().filter(|l| l.starts_with("stray ")).map(|l| l.to_string()).collect()
}

/// Neither stdout nor stderr may carry the raw or canonical spelling of the temp folder or of the worktree.
fn assert_no_folder(label: &str, o: &Output, t: &Tmp) {
    let text = format!("{}{}", out(o), err(o));
    let probes = [
        t.raw.to_string_lossy().into_owned(),
        t.canon.to_string_lossy().into_owned(),
        t.raw.join("W/t1").to_string_lossy().into_owned(),
        t.canon.join("W/t1").to_string_lossy().into_owned(),
        t.raw.join("W/t1/tree").to_string_lossy().into_owned(),
        t.canon.join("W/t1/tree").to_string_lossy().into_owned(),
    ];
    for probe in probes {
        assert!(!text.contains(&probe), "{label}: output carries a folder text");
    }
}

/// The full `workspace check` line for `id`, run from an unrelated folder (never the current folder).
fn check_id(t: &Tmp, id: &str, md_rel: &str) -> Output {
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let args: Vec<String> = [
        "workspace".to_string(),
        "check".to_string(),
        id.to_string(),
        "--manifest-dir".to_string(),
        t.raw.join(md_rel).to_str().unwrap().to_string(),
    ]
    .to_vec();
    run(&args, Some(&unrelated))
}

/// Run the real `workspace create` for `id` under roots `ws_rel` / `md_rel` (both under the scratch tmp).
fn create_id(t: &Tmp, id: &str, ws_rel: &str, md_rel: &str) {
    let (r_raw, _) = t.both("R");
    let s = head(&r_raw);
    ticket(t, id, "r");
    for d in [ws_rel, md_rel] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    let (tf, _) = t.both(&format!("{id}.json"));
    let args: Vec<String> = [
        "workspace",
        "create",
        tf.to_str().unwrap(),
        "--repo-map",
        &format!("r={}", r_raw.display()),
        "--base",
        &s,
        "--workspace-root",
        t.raw.join(ws_rel).to_str().unwrap(),
        "--manifest-dir",
        t.raw.join(md_rel).to_str().unwrap(),
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    let o = run(&args, None);
    assert_eq!(code(&o), 0, "create {id}: {}", err(&o));
}

// ---------------------------------------------------------------- case 1: tracked-only naive

#[test]
fn case01_tracked_only_naive_reports_untracked() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::write(tree.join("planted.txt"), "stray").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    let binding = out(&o);
    let lines: Vec<&str> = binding.lines().collect();
    assert_eq!(lines.len(), 2, "lines: {:?}", lines);
    assert_eq!(lines[0], "stray untracked planted.txt");
    assert_eq!(lines[1], "workspace=stray ticket=t1 count=1");
    assert_no_folder("case01", &o, &t);
}

// ---------------------------------------------------------------- case 2: untracked-folder naive

#[test]
fn case02_untracked_folder_lists_every_file() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::create_dir_all(tree.join("newdir/a")).unwrap();
    fs::write(tree.join("newdir/a/b.txt"), "stray").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray untracked newdir/a/b.txt"), "lines: {:?}", out(&o));
    assert!(!out(&o).contains("newdir/a/\n"), "the folder must not be named instead of the file: {}", out(&o));
}

// ---------------------------------------------------------------- case 3: ignored trick

#[test]
fn case03_ignored_file_is_stray() {
    let t = Tmp::new();
    let (r, _, _) = mk_ws(&t, false);
    // The base itself commits the ignore rule, so the planted file is ignored by a committed pattern.
    fs::write(r.join(".gitignore"), "secret.txt\n").unwrap();
    git(&r, &["add", ".gitignore"]);
    git(&r, &["commit", "-q", "-m", "ignore"]);
    let s = head(&r);
    ticket(&t, "t1b", "r");
    let (tf, _) = t.both("t1b.json");
    let args: Vec<String> = [
        "workspace",
        "create",
        tf.to_str().unwrap(),
        "--repo-map",
        &format!("r={}", t.raw.join("R").display()),
        "--base",
        &s,
        "--workspace-root",
        t.raw.join("W2").to_str().unwrap(),
        "--manifest-dir",
        t.raw.join("M2").to_str().unwrap(),
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    fs::create_dir_all(t.raw.join("W2")).unwrap();
    fs::create_dir_all(t.raw.join("M2")).unwrap();
    let o = run(&args, None);
    assert_eq!(code(&o), 0, "create: {}", err(&o));
    let (tree, _) = t.both("W2/t1b/tree");
    fs::write(tree.join("secret.txt"), "hidden").unwrap();
    let check_args: Vec<String> = [
        "workspace".to_string(),
        "check".to_string(),
        "t1b".to_string(),
        "--manifest-dir".to_string(),
        t.raw.join("M2").to_str().unwrap().to_string(),
    ]
    .to_vec();
    let o = run(&check_args, None);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray ignored secret.txt"), "lines: {:?}", out(&o));
}

// ---------------------------------------------------------------- case 4: assume-unchanged trick

#[test]
fn case04_assume_unchanged_is_hidden_stray() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["update-index", "--assume-unchanged", "f.txt"]);
    fs::write(tree.join("f.txt"), "edited but hidden from status").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray hidden f.txt"), "lines: {:?}", out(&o));
    assert!(!out(&o).contains("stray modified f.txt"), "the hidden read must name the bit, not the edit: {}", out(&o));
}

// ---------------------------------------------------------------- case 5: repo-config trick

#[test]
fn case05_repo_config_cannot_hide_untracked() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["config", "status.showUntrackedFiles", "no"]);
    fs::write(tree.join("planted.txt"), "stray").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray untracked planted.txt"), "lines: {:?}", out(&o));
}

// ---------------------------------------------------------------- case 6: node_modules false positive

#[test]
fn case06_node_modules_link_exempt_but_not_blind() {
    let t = Tmp::new();
    {
        let (_, tree, _) = mk_ws(&t, true);
        assert!(fs::symlink_metadata(tree.join("node_modules")).unwrap().file_type().is_symlink(), "fixture: no link");
        let o = check(&t);
        assert_eq!(code(&o), 0, "clean ws with a live link must pass: {}\n{}", out(&o), err(&o));
        assert_eq!(out(&o), "workspace=clean ticket=t1\n");
        // A file NEXT TO the link (not under it) is still reported.
        fs::write(tree.join("extra.txt"), "stray").unwrap();
        let o = check(&t);
        assert_eq!(code(&o), 2, "stdout: {}", out(&o));
        assert!(stray_lines(&o).iter().any(|l| l == "stray untracked extra.txt"), "lines: {:?}", out(&o));
        assert!(!out(&o).contains("node_modules"), "the live link stays exempt: {}", out(&o));
    }
    // A second workspace: the link replaced by a REAL directory is not exempt.
    let (t2_tree_raw, t2_tree_canon) = t.both("W/t1/tree");
    let _ = t2_tree_canon;
    fs::remove_file(t2_tree_raw.join("node_modules")).unwrap();
    fs::create_dir_all(t2_tree_raw.join("node_modules")).unwrap();
    fs::write(t2_tree_raw.join("node_modules/pkg2.js"), "now real").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray missing node_modules"), "a replaced link is kind missing: {:?}", out(&o));
    assert!(out(&o).contains("node_modules/pkg2.js"), "its files are not exempt either: {}", out(&o));
}

// ---------------------------------------------------------------- case 7: missing listed path

#[test]
fn case07_removed_link_is_missing_stray() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, true);
    fs::remove_file(tree.join("node_modules")).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    let binding = out(&o);
    let lines: Vec<&str> = binding.lines().collect();
    assert_eq!(lines.len(), 2, "lines: {:?}", lines);
    assert_eq!(lines[0], "stray missing node_modules");
    assert_eq!(lines[1], "workspace=stray ticket=t1 count=1");
}

// ---------------------------------------------------------------- case 8: symlink escape

#[test]
fn case08_symlink_is_judged_alone() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let (outside, _) = t.both("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("x.txt"), "never to be listed").unwrap();
    std::os::unix::fs::symlink(&outside, tree.join("out")).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    let binding = out(&o);
    let lines: Vec<&str> = binding.lines().collect();
    assert_eq!(lines.len(), 2, "exactly one stray line: {:?}", lines);
    assert_eq!(lines[0], "stray untracked out");
    assert!(!out(&o).contains("x.txt"), "nothing through the link: {}", out(&o));
    assert_no_folder("case08", &o, &t);
}

// ---------------------------------------------------------------- case 9: tracked edits, staged, deletion, committed-then-edited

#[test]
fn case09_tracked_changes_named_and_sorted() {
    let t = Tmp::new();
    let (r, tree, _) = mk_ws(&t, false);
    for name in ["g.txt", "h.txt", "i.txt"] {
        fs::write(r.join(name), name).unwrap();
    }
    git(&r, &["add", "g.txt", "h.txt", "i.txt"]);
    git(&r, &["commit", "-q", "-m", "more files"]);
    // A new workspace from this base so the extra files are tracked.
    let (tree2, _) = t.both("W2/t2/tree");
    let (tf, _) = t.both("t2.json");
    ticket(&t, "t2", "r");
    fs::create_dir_all(t.raw.join("W2")).unwrap();
    fs::create_dir_all(t.raw.join("M2")).unwrap();
    let s = head(&r);
    let args: Vec<String> = [
        "workspace",
        "create",
        tf.to_str().unwrap(),
        "--repo-map",
        &format!("r={}", t.raw.join("R").display()),
        "--base",
        &s,
        "--workspace-root",
        t.raw.join("W2").to_str().unwrap(),
        "--manifest-dir",
        t.raw.join("M2").to_str().unwrap(),
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    let o = run(&args, None);
    assert_eq!(code(&o), 0, "create: {}", err(&o));
    let _ = tree; // the first workspace only supplies the repo's tracked files
    fs::write(tree2.join("f.txt"), "unstaged edit").unwrap(); // modified
    fs::remove_file(tree2.join("h.txt")).unwrap(); // deleted
    fs::write(tree2.join("c.txt"), "committed").unwrap();
    git(&tree2, &["add", "c.txt"]);
    git(&tree2, &["commit", "-q", "-m", "ticket work"]); // committed on ws/t2...
    fs::write(tree2.join("c.txt"), "then edited").unwrap(); // ...then edited
    // Staged LAST: a stage before the commit would ride into it and vanish from status.
    fs::write(tree2.join("g.txt"), "staged edit").unwrap();
    git(&tree2, &["add", "g.txt"]); // staged
    let check_args: Vec<String> = [
        "workspace".to_string(),
        "check".to_string(),
        "t2".to_string(),
        "--manifest-dir".to_string(),
        t.raw.join("M2").to_str().unwrap().to_string(),
    ]
    .to_vec();
    let o = run(&check_args, None);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    let lines = stray_lines(&o);
    assert_eq!(lines.join("\n"), "stray modified c.txt\nstray modified f.txt\nstray modified g.txt\nstray modified h.txt", "lines: {:?}", lines);
    assert!(out(&o).ends_with("workspace=stray ticket=t2 count=4\n"), "summary: {}", out(&o));
}

// ---------------------------------------------------------------- case 10: clean, and commits are expected

#[test]
fn case10_clean_workspace_and_commits_pass() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let o = check(&t);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "workspace=clean ticket=t1\n");
    // New commits on ws/t1 (the ticket's own work) with nothing else dirty: still clean.
    fs::write(tree.join("work.txt"), "ticket work").unwrap();
    git(&tree, &["add", "work.txt"]);
    git(&tree, &["commit", "-q", "-m", "work"]);
    let o = check(&t);
    assert_eq!(code(&o), 0, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert_eq!(out(&o), "workspace=clean ticket=t1\n");
}

// ---------------------------------------------------------------- case 11: cannot-check cases

#[test]
fn case11_cannot_check_exit_3() {
    // (a) missing manifest
    let t = Tmp::new();
    let (_, _, m) = mk_ws(&t, false);
    fs::remove_file(&m).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "missing manifest: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty(), "stdout must stay empty on exit 3");

    // (b) torn manifest (truncated mid-line)
    let t = Tmp::new();
    let (_, _, m) = mk_ws(&t, false);
    let bytes = fs::read(&m).unwrap();
    assert!(bytes.len() > 10);
    fs::write(&m, &bytes[..bytes.len() - 4]).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "torn manifest: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty());

    // (c) manifest without a live worktree entry
    let t = Tmp::new();
    let (_, _, m) = mk_ws(&t, false);
    let text = fs::read_to_string(&m).unwrap();
    let kept: Vec<&str> = text.lines().filter(|l| !l.contains("\"id\":\"worktree\"")).collect();
    assert!(!kept.is_empty());
    fs::write(&m, format!("{}\n", kept.join("\n"))).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "no worktree entry: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty());

    // (d) worktree folder removed
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::remove_dir_all(&tree).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "worktree removed: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty());

    // (e) worktree path replaced by a symlink POINTING AT A REAL CLEAN WORKTREE: the exit 3 must come
    //     from the symlink rule itself (the tree behind the link is clean and checkable), never from a
    //     failing git call or the not-a-directory arm, so the stderr names the symlink rule.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    create_id(&t, "t9", "W9", "M9");
    let (real_tree, _) = t.both("W9/t9/tree");
    fs::remove_dir_all(&tree).unwrap();
    std::os::unix::fs::symlink(&real_tree, &tree).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "worktree is a symlink: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("is a symlink"), "the symlink rule itself must speak: {}", err(&o));

    // (f) detached HEAD
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["checkout", "-q", "--detach"]);
    let o = check(&t);
    assert_eq!(code(&o), 3, "detached HEAD: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty());

    // (g) HEAD on another branch
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["switch", "-q", "-c", "other"]);
    let o = check(&t);
    assert_eq!(code(&o), 3, "other branch: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty());

    // (h) mid-merge (a conflicted merge leaves MERGE_HEAD behind)
    let t = Tmp::new();
    let (r, tree, _) = mk_ws(&t, false);
    fs::write(tree.join("f.txt"), "ticket side").unwrap();
    git(&tree, &["add", "f.txt"]);
    git(&tree, &["commit", "-q", "-m", "ticket side"]);
    fs::write(r.join("f.txt"), "master side").unwrap();
    git(&r, &["add", "f.txt"]);
    git(&r, &["commit", "-q", "-m", "master side"]);
    let master = head(&r);
    let o = git_try(&tree, &["merge", &master]);
    assert!(!o.status.success(), "fixture: the merge was meant to conflict");
    let o = check(&t);
    assert_eq!(code(&o), 3, "mid-merge: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty());
}

// ---------------------------------------------------------------- case 12: the size bound

#[test]
fn case12_over_ten_thousand_paths_is_exit_3() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    for i in 0..=10_000 {
        fs::write(tree.join(format!("f{i}.txt")), "x").unwrap();
    }
    let o = check(&t);
    assert_eq!(code(&o), 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(!out(&o).contains("workspace=clean"), "no truncated PASS: {}", out(&o));
    assert!(err(&o).contains("too many paths"), "stderr: {}", err(&o));
}

// ---------------------------------------------------------------- case 13: non-UTF-8 path bytes

#[test]
fn case13_non_utf8_path_is_escaped_not_panicked() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    // Byte 0xff is the plan's spelling, but this platform's filesystem refuses names that are not valid
    // UTF-8 (EILSEQ), so the fixture falls back to a control byte — the same escaping path in the check.
    let (name, escaped) = match fs::write(tree.join(OsStr::from_bytes(b"bad\xff.txt")), "stray") {
        Ok(()) => (b"bad\xff.txt".to_vec(), "stray untracked bad\\xff.txt".to_string()),
        Err(e) if e.raw_os_error() == Some(92) => { // EILSEQ: the filesystem refuses non-UTF-8 names
            let n = b"ctrl\x01.txt".to_vec();
            fs::write(tree.join(OsStr::from_bytes(&n)), "stray").unwrap();
            (n, "stray untracked ctrl\\x01.txt".to_string())
        }
        Err(e) => panic!("fixture: {e}"),
    };
    let _ = name;
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(out(&o).contains(&escaped), "escaped line: {:?}", out(&o).lines().collect::<Vec<_>>());
    assert_eq!(out(&o).lines().count(), 2, "one stray line plus the summary: {}", out(&o));
}

// ---------------------------------------------------------------- case 14: changed submodule

#[test]
fn case14_changed_submodule_is_stray() {
    let t = Tmp::new();
    // A submodule source repo, then the primary repo commits it as a real submodule.
    let (s_raw, _) = t.both("S");
    fs::create_dir_all(&s_raw).unwrap();
    git(&s_raw, &["init", "-q"]);
    fs::write(s_raw.join("s.txt"), "s").unwrap();
    git(&s_raw, &["add", "s.txt"]);
    git(&s_raw, &["commit", "-q", "-m", "s1"]);
    let (r, _tree, _) = mk_ws(&t, false);
    git(&r, &["-c", "protocol.file.allow=always", "submodule", "add", "--name", "sub", &s_raw.to_str().unwrap(), "sub"]);
    git(&r, &["add", ".gitmodules"]);
    git(&r, &["commit", "-q", "-m", "add sub"]);
    // A workspace from that base, the submodule materialised, then its HEAD advanced.
    let s = head(&r);
    let (tf, _) = t.both("t3.json");
    ticket(&t, "t3", "r");
    fs::create_dir_all(t.raw.join("W3")).unwrap();
    fs::create_dir_all(t.raw.join("M3")).unwrap();
    let args: Vec<String> = [
        "workspace",
        "create",
        tf.to_str().unwrap(),
        "--repo-map",
        &format!("r={}", t.raw.join("R").display()),
        "--base",
        &s,
        "--workspace-root",
        t.raw.join("W3").to_str().unwrap(),
        "--manifest-dir",
        t.raw.join("M3").to_str().unwrap(),
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    let o = run(&args, None);
    assert_eq!(code(&o), 0, "create: {}", err(&o));
    let (tree3, _) = t.both("W3/t3/tree");
    git(&tree3, &["-c", "protocol.file.allow=always", "submodule", "update", "--init"]);
    git(&tree3.join("sub"), &["commit", "-q", "--allow-empty", "-m", "s2"]);
    let check_args: Vec<String> = [
        "workspace".to_string(),
        "check".to_string(),
        "t3".to_string(),
        "--manifest-dir".to_string(),
        t.raw.join("M3").to_str().unwrap().to_string(),
    ]
    .to_vec();
    let o = run(&check_args, None);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray submodule sub"), "lines: {:?}", out(&o));
}

// ---------------------------------------------------------------- case 15: read-only

#[test]
fn case15_check_writes_nothing() {
    let t = Tmp::new();
    let (r, _, _) = mk_ws(&t, false); // fixes the repo id in the ticket map; the fixture tree below is t5's
    let _ = &r;
    // A second tracked file in the base, so the checked tree holds an UNMODIFIED tracked path whose stat
    // cache can be made stale: an optional-lock status would then want to rewrite the index.
    let (r_raw, _) = t.both("R");
    fs::write(r_raw.join("keep.txt"), "unchanged").unwrap();
    git(&r_raw, &["add", "keep.txt"]);
    git(&r_raw, &["commit", "-q", "-m", "keep"]);
    create_id(&t, "t5", "W5", "M5");
    let (tree5, _) = t.both("W5/t5/tree");
    let (m5, _) = t.both("M5/t5.jsonl");
    // A dirty tree: one untracked file, one tracked edit, and a stale stat cache on the untouched file.
    fs::write(tree5.join("planted.txt"), "stray").unwrap();
    fs::write(tree5.join("f.txt"), "edited").unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(20);
    std::fs::File::options().write(true).open(tree5.join("keep.txt")).unwrap().set_modified(later).unwrap();
    let gitdir = PathBuf::from(git(&tree5, &["rev-parse", "--absolute-git-dir"]));
    let index = gitdir.join("index");
    let idx_before = fs::read(&index).unwrap();
    let man_before = fs::read(&m5).unwrap();
    let list_before = git(&r_raw, &["worktree", "list", "--porcelain"]);
    let o = check_id(&t, "t5", "M5");
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert_eq!(fs::read(&index).unwrap(), idx_before, "the worktree index changed");
    assert_eq!(fs::read(&m5).unwrap(), man_before, "the manifest file changed");
    assert_eq!(git(&r_raw, &["worktree", "list", "--porcelain"]), list_before, "the worktree registration changed");
}
// ---------------------------------------------------------------- case 16: usage validated before anything

#[test]
fn case16_usage_and_name_refusals_are_exit_3() {
    let t = Tmp::new();
    let (_, _, _) = mk_ws(&t, false);
    let md = t.raw.join("M").to_str().unwrap().to_string();
    for args in [
        vec!["workspace", "check", "T-1", "--manifest-dir", &md],          // not a plain token: refused, never rewritten
        vec!["workspace", "check", "t1", "--manifest-dir", "relative/dir"], // not absolute
        vec!["workspace", "check", "t1", "--manifest-dir", &md, "--extra"], // unknown option
        vec!["workspace", "check", "--manifest-dir", &md],                  // no ticket
        vec!["workspace", "check", "t1"],                                   // no --manifest-dir
    ] {
        let a: Vec<String> = args.iter().map(|x| x.to_string()).collect();
        let o = run(&a, None);
        assert_eq!(code(&o), 3, "args {:?}: stdout {} stderr {}", &args[2..], out(&o), err(&o));
        assert!(out(&o).is_empty(), "usage refusals print nothing on stdout: {}", out(&o));
    }
    // The good spelling still works after all those refusals (nothing was consumed or written).
    let o = check(&t);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
}

// ---------------------------------------------------------------- case 17 (fix F1): both hidden bits, tag `s`

#[test]
fn case17_both_hidden_bits_tag_s_is_hidden_stray() {
    // `git ls-files -v` prints one case-folded tag per path: `S` is skip-worktree, a LOWERCASE tag is
    // assume-unchanged, and one file carrying BOTH bits prints `s`. Anything that is not a plain `H`
    // must be stray (fail closed: lowercase tags, both-bits tags, unknown and future tags), whatever
    // the state of the file itself.
    // (a) both bits, then edited
    {
        let t = Tmp::new();
        let (_, tree, _) = mk_ws(&t, false);
        git(&tree, &["update-index", "--skip-worktree", "f.txt"]);
        git(&tree, &["update-index", "--assume-unchanged", "f.txt"]);
        fs::write(tree.join("f.txt"), "edited under both bits").unwrap();
        let o = check(&t);
        assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
        assert!(stray_lines(&o).iter().any(|l| l == "stray hidden f.txt"), "lines: {:?}", out(&o));
        assert!(!out(&o).contains("stray modified f.txt"), "the hidden read names the bit: {}", out(&o));
    }
    // (b) both bits, then deleted
    {
        let t = Tmp::new();
        let (_, tree, _) = mk_ws(&t, false);
        git(&tree, &["update-index", "--skip-worktree", "f.txt"]);
        git(&tree, &["update-index", "--assume-unchanged", "f.txt"]);
        fs::remove_file(tree.join("f.txt")).unwrap();
        let o = check(&t);
        assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
        assert!(stray_lines(&o).iter().any(|l| l == "stray hidden f.txt"), "lines: {:?}", out(&o));
    }
    // (c) both bits, file untouched: the bit alone is stray
    {
        let t = Tmp::new();
        let (_, tree, _) = mk_ws(&t, false);
        git(&tree, &["update-index", "--skip-worktree", "f.txt"]);
        git(&tree, &["update-index", "--assume-unchanged", "f.txt"]);
        let _ = tree;
        let o = check(&t);
        assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
        let binding = out(&o);
        let lines: Vec<&str> = binding.lines().collect();
        assert_eq!(lines.len(), 2, "lines: {:?}", lines);
        assert_eq!(lines[0], "stray hidden f.txt");
        assert_eq!(lines[1], "workspace=stray ticket=t1 count=1");
    }
}

// ---------------------------------------------------------------- case 18 (fix F2): staged rename names both paths

#[test]
fn case18_staged_rename_names_both_paths() {
    // A staged rename (`git mv`) carries TWO paths: the new one and the removed original. Both must be
    // named, or a FAIL would hide half of what moved.
    {
        let t = Tmp::new();
        let (_, tree, _) = mk_ws(&t, false);
        git(&tree, &["mv", "f.txt", "renamed.txt"]);
        let o = check(&t);
        assert_eq!(code(&o), 2, "stdout: {}", out(&o));
        let lines = stray_lines(&o);
        assert_eq!(lines.join("\n"), "stray modified f.txt\nstray modified renamed.txt", "lines: {:?}", lines);
        assert!(out(&o).ends_with("workspace=stray ticket=t1 count=2\n"), "summary: {}", out(&o));
    }
    // A worktree-only rename (a plain mv, nothing staged) keeps its shape: the old path is a tracked
    // deletion and the new path is untracked.
    {
        let t = Tmp::new();
        let (_, tree, _) = mk_ws(&t, false);
        fs::rename(tree.join("f.txt"), tree.join("moved.txt")).unwrap();
        let o = check(&t);
        assert_eq!(code(&o), 2, "stdout: {}", out(&o));
        let lines = stray_lines(&o);
        assert_eq!(lines.join("\n"), "stray modified f.txt\nstray untracked moved.txt", "lines: {:?}", lines);
    }
}

// ---------------------------------------------------------------- case 19 (F3a): submodule DIRTY CONTENT

#[test]
fn case19_submodule_dirty_content_is_stray() {
    // Advancing a submodule HEAD is only one way a submodule changes; a dirty file INSIDE a committed
    // submodule must be named too (`--ignore-submodules=dirty` would hide it).
    let t = Tmp::new();
    let (s_raw, _) = t.both("S");
    fs::create_dir_all(&s_raw).unwrap();
    git(&s_raw, &["init", "-q"]);
    fs::write(s_raw.join("s.txt"), "s").unwrap();
    git(&s_raw, &["add", "s.txt"]);
    git(&s_raw, &["commit", "-q", "-m", "s1"]);
    let (r, _, _) = mk_ws(&t, false);
    git(&r, &["-c", "protocol.file.allow=always", "submodule", "add", "--name", "sub", s_raw.to_str().unwrap(), "sub"]);
    git(&r, &["add", ".gitmodules"]);
    git(&r, &["commit", "-q", "-m", "add sub"]);
    let s = head(&r);
    ticket(&t, "t3", "r");
    fs::create_dir_all(t.raw.join("W3")).unwrap();
    fs::create_dir_all(t.raw.join("M3")).unwrap();
    let (tf, _) = t.both("t3.json");
    let args: Vec<String> = [
        "workspace",
        "create",
        tf.to_str().unwrap(),
        "--repo-map",
        &format!("r={}", t.raw.join("R").display()),
        "--base",
        &s,
        "--workspace-root",
        t.raw.join("W3").to_str().unwrap(),
        "--manifest-dir",
        t.raw.join("M3").to_str().unwrap(),
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    let o = run(&args, None);
    assert_eq!(code(&o), 0, "create: {}", err(&o));
    let (tree3, _) = t.both("W3/t3/tree");
    git(&tree3, &["-c", "protocol.file.allow=always", "submodule", "update", "--init"]);
    fs::write(tree3.join("sub/s.txt"), "dirty content, not a new commit").unwrap();
    let o = check_id(&t, "t3", "M3");
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    let binding = out(&o);
    let lines: Vec<&str> = binding.lines().collect();
    assert_eq!(lines.len(), 2, "lines: {:?}", lines);
    assert_eq!(lines[0], "stray submodule sub");
    assert_eq!(lines[1], "workspace=stray ticket=t3 count=1");
}

// ---------------------------------------------------------------- case 20 (F3b): non-top-level folder

#[test]
fn case20_nontoplevel_folder_of_another_repo_is_exit_3() {
    // A folder that belongs to another repo but is not that repo's top level must be refused, even
    // when everything else about it looks right (the repo even sits on a branch named ws/t1): judging
    // it would mean judging the WRONG work tree. The foreign repo sits inside the ticket root so the
    // manifest fence still holds; only the worktree entry's path text is rewritten to it.
    let t = Tmp::new();
    let (_, _, m) = mk_ws(&t, false);
    let (x, _) = t.both("W/t1/xrepo");
    fs::create_dir_all(x.join("inner")).unwrap();
    git(&x, &["init", "-q", "-b", "ws/t1"]);
    fs::write(x.join("inner/keep.txt"), "x").unwrap();
    git(&x, &["add", "inner/keep.txt"]);
    git(&x, &["commit", "-q", "-m", "x"]);
    let inner = x.join("inner").canonicalize().unwrap();
    let text = fs::read_to_string(&m).unwrap();
    let (tree_raw, tree_canon) = t.both("W/t1/tree");
    let updated = text
        .replace(&*tree_canon.to_string_lossy(), &*inner.to_string_lossy())
        .replace(&*tree_raw.to_string_lossy(), &*inner.to_string_lossy());
    assert!(updated != text, "fixture: the worktree entry path was not found in the manifest");
    fs::write(&m, updated).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(out(&o).is_empty(), "stdout must stay empty on exit 3");
    assert!(err(&o).contains("top level"), "the top-level rule itself must speak: {}", err(&o));
}

// ---------------------------------------------------------------- case 21 (F3c): strongest kind wins

#[test]
fn case21_strongest_kind_wins_at_one_path() {
    // One path carrying two kinds reports once, with the STRONGER kind (doc order). Here the
    // node_modules link is replaced by a real directory that a COMMITTED ignore rule covers: status
    // calls the path `ignored`, the manifest calls it `missing`, and `missing` (the stronger) wins.
    let t = Tmp::new();
    let (r, _, _) = mk_ws(&t, true);
    fs::write(r.join(".gitignore"), "node_modules/\n").unwrap();
    git(&r, &["add", ".gitignore"]);
    git(&r, &["commit", "-q", "-m", "ignore node_modules"]);
    create_id(&t, "t6", "W6", "M6");
    let (tree6, _) = t.both("W6/t6/tree");
    fs::remove_file(tree6.join("node_modules")).unwrap();
    fs::create_dir_all(tree6.join("node_modules")).unwrap();
    fs::write(tree6.join("node_modules/pkg2.js"), "now real").unwrap();
    let o = check_id(&t, "t6", "M6");
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray missing node_modules"), "lines: {:?}", out(&o));
    assert!(!out(&o).contains("stray ignored node_modules"), "the weaker kind must not win: {}", out(&o));
    assert!(!out(&o).contains("node_modules/"), "one spelling of the path only: {}", out(&o));
}

// ---------------------------------------------------------------- case 22 (F3e): ignored dir trailing slash

#[test]
fn case22_ignored_dir_trailing_slash_is_dropped() {
    // Porcelain spells an ignored DIRECTORY with a trailing `/`; the stray line must name the path,
    // and the trailing slash must be dropped so a directory and its spelling as a path compare equal.
    let t = Tmp::new();
    let (r, _, _) = mk_ws(&t, false);
    fs::write(r.join(".gitignore"), "build/\n").unwrap();
    git(&r, &["add", ".gitignore"]);
    git(&r, &["commit", "-q", "-m", "ignore build"]);
    create_id(&t, "t7", "W7", "M7");
    let (tree7, _) = t.both("W7/t7/tree");
    fs::create_dir_all(tree7.join("build")).unwrap();
    fs::write(tree7.join("build/x.txt"), "x").unwrap();
    let o = check_id(&t, "t7", "M7");
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    let binding = out(&o);
    let lines: Vec<&str> = binding.lines().collect();
    assert_eq!(lines.len(), 2, "lines: {:?}", lines);
    assert_eq!(lines[0], "stray ignored build");
    assert_eq!(lines[1], "workspace=stray ticket=t7 count=1");
}

// ---------------------------------------------------------------- case 23 (F3f): space prints as \x20

#[test]
fn case23_space_in_path_prints_as_x20() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::write(tree.join("a b.txt"), "stray").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    let binding = out(&o);
    let lines: Vec<&str> = binding.lines().collect();
    assert_eq!(lines.len(), 2, "lines: {:?}", lines);
    assert_eq!(lines[0], "stray untracked a\\x20b.txt");
    assert_eq!(lines[1], "workspace=stray ticket=t1 count=1");
}

// ---------------------------------------------------------------- case 24 (F3d): inherited GIT_* names

#[test]
fn case24_inherited_git_env_cannot_redirect() {
    // GIT_DIR / GIT_WORK_TREE inherited from the caller point at the PRIMARY repo (which is clean):
    // the check must strip every inherited GIT_* name, or its git calls would judge the wrong tree
    // and report the planted file's workspace clean.
    let t = Tmp::new();
    let (r, tree, _) = mk_ws(&t, false);
    fs::write(tree.join("planted.txt"), "stray").unwrap();
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let args: Vec<String> = [
        "workspace".to_string(),
        "check".to_string(),
        "t1".to_string(),
        "--manifest-dir".to_string(),
        t.raw.join("M").to_str().unwrap().to_string(),
    ]
    .to_vec();
    let mut c = Command::new(BIN);
    c.args(&args)
        .env("GIT_DIR", r.join(".git"))
        .env("GIT_WORK_TREE", &r)
        .current_dir(&unrelated)
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray untracked planted.txt"), "lines: {:?}", out(&o));
}

// ------------------------------------------------- fix round 1 held-out variants (own inventions, not review rows)

#[test]
fn ho1_tag_s_and_type_change_to_symlink_is_hidden() {
    // must-fire: both hidden bits AND the tracked file replaced by a symlink (a type change git
    // status would report if the bits were not hiding it).
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["update-index", "--skip-worktree", "f.txt"]);
    git(&tree, &["update-index", "--assume-unchanged", "f.txt"]);
    fs::remove_file(tree.join("f.txt")).unwrap();
    let (outside, _) = t.both("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("x.txt"), "x").unwrap();
    std::os::unix::fs::symlink(&outside, tree.join("f.txt")).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    let binding = out(&o);
    let lines: Vec<&str> = binding.lines().collect();
    assert_eq!(lines.len(), 2, "lines: {:?}", lines);
    assert_eq!(lines[0], "stray hidden f.txt");
    assert_eq!(lines[1], "workspace=stray ticket=t1 count=1");
}

#[test]
fn ho2_assume_unchanged_in_subfolder_named_with_folder() {
    // must-fire: the hidden bit on a tracked file inside a subfolder, then edited — the line must
    // carry the full relative path.
    let t = Tmp::new();
    let (r, _, _) = mk_ws(&t, false);
    fs::create_dir_all(r.join("sub")).unwrap();
    fs::write(r.join("sub/f.txt"), "sub").unwrap();
    git(&r, &["add", "sub/f.txt"]);
    git(&r, &["commit", "-q", "-m", "sub"]);
    create_id(&t, "t8", "W8", "M8");
    let (tree8, _) = t.both("W8/t8/tree");
    git(&tree8, &["update-index", "--assume-unchanged", "sub/f.txt"]);
    fs::write(tree8.join("sub/f.txt"), "edited").unwrap();
    let o = check_id(&t, "t8", "M8");
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray hidden sub/f.txt"), "lines: {:?}", out(&o));
}

#[test]
fn ho3_staged_rename_into_subfolder_names_both_paths_sorted() {
    // must-fire: a staged rename that also moves the file into a new subfolder — both paths named,
    // sorted by raw path bytes.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::create_dir_all(tree.join("sub")).unwrap();
    git(&tree, &["mv", "f.txt", "sub/f.txt"]);
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    let lines = stray_lines(&o);
    assert_eq!(lines.join("\n"), "stray modified f.txt\nstray modified sub/f.txt", "lines: {:?}", lines);
    assert!(out(&o).ends_with("workspace=stray ticket=t1 count=2\n"), "summary: {}", out(&o));
}

#[test]
fn ho4_staged_rename_with_space_escapes_old_name() {
    // must-fire: a staged rename whose OLD name holds a space — the removed original must be named
    // too, with the space escaped so it stays one line.
    let t = Tmp::new();
    let (r, _, _) = mk_ws(&t, false);
    fs::write(r.join("old name.txt"), "o").unwrap();
    git(&r, &["add", "old name.txt"]);
    git(&r, &["commit", "-q", "-m", "space"]);
    create_id(&t, "t9", "W9", "M9");
    let (tree9, _) = t.both("W9/t9/tree");
    git(&tree9, &["mv", "old name.txt", "new.txt"]);
    let o = check_id(&t, "t9", "M9");
    assert_eq!(code(&o), 2, "stdout: {}", out(&o));
    let lines = stray_lines(&o);
    assert_eq!(lines.join("\n"), "stray modified new.txt\nstray modified old\\x20name.txt", "lines: {:?}", lines);
}

#[test]
fn ho5_clean_space_named_tracked_file_stays_clean() {
    // must-not-fire: a plain `H` tag next to a committed file whose NAME holds a space — nothing to
    // report, and the clean line is the only output.
    let t = Tmp::new();
    let (r, _, _) = mk_ws(&t, false);
    fs::write(r.join("a b.txt"), "ab").unwrap();
    git(&r, &["add", "a b.txt"]);
    git(&r, &["commit", "-q", "-m", "space name"]);
    create_id(&t, "t10", "W10", "M10");
    let o = check_id(&t, "t10", "M10");
    assert_eq!(code(&o), 0, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert_eq!(out(&o), "workspace=clean ticket=t10\n");
}

#[test]
fn ho6_committed_rename_on_ticket_branch_stays_clean() {
    // must-not-fire: a rename that is already committed on the ticket's own branch is expected work.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["mv", "f.txt", "renamed.txt"]);
    git(&tree, &["commit", "-q", "-m", "rename the file"]);
    let o = check(&t);
    assert_eq!(code(&o), 0, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert_eq!(out(&o), "workspace=clean ticket=t1\n");
}

#[test]
fn ho7_node_modules_link_and_clean_submodule_stay_clean() {
    // must-not-fire: the expected node_modules link present AND an unrelated, cleanly checked-out
    // submodule — both are expected, the check stays clean.
    let t = Tmp::new();
    let (s_raw, _) = t.both("S");
    fs::create_dir_all(&s_raw).unwrap();
    git(&s_raw, &["init", "-q"]);
    fs::write(s_raw.join("s.txt"), "s").unwrap();
    git(&s_raw, &["add", "s.txt"]);
    git(&s_raw, &["commit", "-q", "-m", "s1"]);
    let (r, _, _) = mk_ws(&t, true);
    git(&r, &["-c", "protocol.file.allow=always", "submodule", "add", "--name", "sub", s_raw.to_str().unwrap(), "sub"]);
    git(&r, &["add", ".gitmodules"]);
    git(&r, &["commit", "-q", "-m", "add sub"]);
    create_id(&t, "t11", "W11", "M11");
    let (tree11, _) = t.both("W11/t11/tree");
    git(&tree11, &["-c", "protocol.file.allow=always", "submodule", "update", "--init"]);
    let o = check_id(&t, "t11", "M11");
    assert_eq!(code(&o), 0, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert_eq!(out(&o), "workspace=clean ticket=t11\n");
}

// ------------------------------------------------- fix round 2: shared helpers

/// The absolute git dir of a scratch repo (its `info/` holds `attributes`, `exclude`, `sparse-checkout`).
fn gitdir_of(d: &Path) -> PathBuf {
    PathBuf::from(git(d, &["rev-parse", "--path-format=absolute", "--absolute-git-dir"]))
}

/// A same-size edit of `f.txt` (3 bytes) with the mtime restored to the SAME WHOLE SECOND (sub-second
/// zeroed): the exact shape the stat levers hide — `core.trustctime=false` and `core.checkStat=minimal`
/// each make git judge the entry clean (measured), while the default comparison catches it. The de-racy
/// pass first rewrites the index after the checkout and un-smudges the entry, so the restored mtime no
/// longer sits in git's racy-clean window (a racy entry re-compares CONTENT and tests nothing).
fn same_size_edit_with_old_mtime(tree: &Path) {
    fs::write(tree.join("de-racy.txt"), "index rewrite after checkout").unwrap();
    git(tree, &["add", "de-racy.txt"]);
    git(tree, &["commit", "-q", "-m", "de-racy the index"]);
    // A racily-clean index entry records size 0, so ANY later edit trips on size alone; the refresh
    // de-smudges the entry (real size and stat) while the tree is still clean.
    git(tree, &["update-index", "--refresh"]);
    let f = tree.join("f.txt");
    let before = fs::metadata(&f).unwrap();
    assert_eq!(before.len(), 3, "fixture: f.txt must be 3 bytes");
    let secs = before
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::thread::sleep(std::time::Duration::from_millis(1100)); // past the whole second, past racy
    fs::write(&f, "twp").unwrap();
    let whole = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    std::fs::File::options().write(true).open(&f).unwrap().set_modified(whole).unwrap();
    // One default-config status run settles the index into its post-status shape: after it, a
    // lever-honouring status (trustctime=false / checkStat=minimal) judges the entry CLEAN while a
    // default-comparison status still names it (measured on this platform's git).
    git(tree, &["status"]);
}

/// A workspace whose base commits one checked-out submodule `sub` (source repo `S`, one file `a.txt`).
/// Returns (worktree, submodule folder). The submodule is materialised, so its own config and index
/// are live — exactly the F1 ground.
fn mk_sub_ws(t: &Tmp, id: &str, wdir: &str, mdir: &str) -> (PathBuf, PathBuf) {
    let (s_raw, _) = t.both("S");
    fs::create_dir_all(&s_raw).unwrap();
    git(&s_raw, &["init", "-q"]);
    fs::write(s_raw.join("a.txt"), "a").unwrap();
    git(&s_raw, &["add", "a.txt"]);
    git(&s_raw, &["commit", "-q", "-m", "s1"]);
    let (r, _, _) = mk_ws(t, false);
    git(&r, &["-c", "protocol.file.allow=always", "submodule", "add", "--name", "sub", s_raw.to_str().unwrap(), "sub"]);
    git(&r, &["add", ".gitmodules"]);
    git(&r, &["commit", "-q", "-m", "add sub"]);
    create_id(t, id, wdir, mdir);
    let (tree, _) = t.both(&format!("{wdir}/{id}/tree"));
    git(&tree, &["-c", "protocol.file.allow=always", "submodule", "update", "--init"]);
    (tree.clone(), tree.join("sub"))
}

/// chmod a path and return (the check must survive every mode-shape unchanged paths can take).
fn chmod_x(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(p).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap();
}

// ------------------------------------------- F1: state hidden inside a submodule

#[test]
fn case25_submodule_assume_unchanged_edit_is_stray() {
    // git's own submodule probe runs the submodule's own status, which honours ITS assume-unchanged
    // bit: an edited file inside a committed submodule must still be `stray submodule sub`.
    let t = Tmp::new();
    let (_, sub) = mk_sub_ws(&t, "t20", "W20", "M20");
    git(&sub, &["update-index", "--assume-unchanged", "a.txt"]);
    fs::write(sub.join("a.txt"), "edited inside the submodule").unwrap();
    let o = check_id(&t, "t20", "M20");
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray submodule sub"), "lines: {:?}", out(&o));
}

#[test]
fn case26_submodule_skip_worktree_edit_is_stray() {
    let t = Tmp::new();
    let (_, sub) = mk_sub_ws(&t, "t21", "W21", "M21");
    git(&sub, &["update-index", "--skip-worktree", "a.txt"]);
    fs::write(sub.join("a.txt"), "edited inside the submodule").unwrap();
    let o = check_id(&t, "t21", "M21");
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray submodule sub"), "lines: {:?}", out(&o));
}

#[test]
fn case27_submodule_showuntrackedfiles_no_is_stray() {
    // The submodule's own `status.showUntrackedFiles=no` blinds the outer probe; the forced
    // `--untracked-files=all` read inside the submodule must see the file.
    let t = Tmp::new();
    let (_, sub) = mk_sub_ws(&t, "t22", "W22", "M22");
    git(&sub, &["config", "status.showUntrackedFiles", "no"]);
    fs::write(sub.join("new.txt"), "untracked").unwrap();
    let o = check_id(&t, "t22", "M22");
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray submodule sub"), "lines: {:?}", out(&o));
}

#[test]
fn case28_submodule_own_info_exclude_is_stray() {
    // The submodule's own info/exclude must not hide a planted file: `--ignored=matching` runs
    // inside the submodule too.
    let t = Tmp::new();
    let (_, sub) = mk_sub_ws(&t, "t23", "W23", "M23");
    let subgit = gitdir_of(&sub);
    fs::create_dir_all(subgit.join("info")).unwrap();
    fs::write(subgit.join("info/exclude"), "planted.txt\n").unwrap();
    fs::write(sub.join("planted.txt"), "excluded inside the submodule").unwrap();
    let o = check_id(&t, "t23", "M23");
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray submodule sub"), "lines: {:?}", out(&o));
}

#[test]
fn case29_submodule_own_filemode_false_is_stray() {
    // The submodule's own `core.filemode=false` hides a mode change from the outer probe; the
    // forced `core.filemode=true` read inside the submodule must see it.
    let t = Tmp::new();
    let (_, sub) = mk_sub_ws(&t, "t24", "W24", "M24");
    git(&sub, &["config", "core.filemode", "false"]);
    chmod_x(&sub.join("a.txt"));
    let o = check_id(&t, "t24", "M24");
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray submodule sub"), "lines: {:?}", out(&o));
}

// ------------------------------------------- F2: repo config cannot hide an edit

#[test]
fn case30_trustctime_false_cannot_hide_same_size_edit() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["config", "core.trustctime", "false"]);
    same_size_edit_with_old_mtime(&tree);
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray modified f.txt"), "lines: {:?}", out(&o));
}

#[test]
fn case31_checkstat_minimal_cannot_hide_same_size_edit() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["config", "core.trustctime", "false"]);
    git(&tree, &["config", "core.checkStat", "minimal"]);
    same_size_edit_with_old_mtime(&tree);
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray modified f.txt"), "lines: {:?}", out(&o));
}

#[test]
fn case32_filter_config_refuses_and_never_runs_it() {
    // A clean/smudge/process filter is both a hide and an arbitrary command a "read-only" check
    // would execute: its presence is exit 3, and the filter command must NEVER run (no marker).
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let (marker, _) = t.both("marker");
    git(&tree, &["config", "filter.h.clean", &format!("touch {}", marker.display())]);
    let gd = gitdir_of(&tree);
    fs::create_dir_all(gd.join("info")).unwrap();
    fs::write(gd.join("info/attributes"), "f.txt filter=h\n").unwrap();
    same_size_edit_with_old_mtime(&tree);
    let o = check(&t);
    assert_eq!(code(&o), 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(out(&o).is_empty(), "stdout must stay empty on exit 3");
    assert!(err(&o).contains("attributes") || err(&o).contains("filter"), "stderr names why: {}", err(&o));
    assert!(!marker.exists(), "the check must never execute a repo-configured command");
}

#[test]
fn case33_filemode_false_cannot_hide_chmod() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["config", "core.filemode", "false"]);
    chmod_x(&tree.join("f.txt"));
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray modified f.txt"), "lines: {:?}", out(&o));
}

// ------------------------------------------- F3: replace refs are not honoured

#[test]
fn case34_replace_ref_cannot_hide_staged_file() {
    // `git replace <HEAD> <commit-with-x.txt>` plus a staged x.txt: a replace-aware status sees no
    // difference. Every call runs with --no-replace-objects, so the staged file is named.
    let t = Tmp::new();
    let (r, tree, _) = mk_ws(&t, false);
    fs::write(r.join("x.txt"), "x").unwrap();
    git(&r, &["add", "x.txt"]);
    git(&r, &["commit", "-q", "-m", "x"]);
    let with_x = head(&r);
    fs::write(tree.join("x.txt"), "x").unwrap();
    git(&tree, &["add", "x.txt"]);
    git(&tree, &["replace", &head(&tree), &with_x]);
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray modified x.txt"), "lines: {:?}", out(&o));
}

// ------------------------------------------- F4: entries git never lists

#[test]
fn case35_fifo_is_stray_untracked() {
    // git never lists a FIFO, so status alone would pass; a no-follow walk names it.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let mk = Command::new("mkfifo").arg(tree.join("pipe")).stdin(Stdio::null()).output().unwrap();
    assert!(mk.status.success(), "fixture mkfifo: {}", String::from_utf8_lossy(&mk.stderr));
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray untracked pipe"), "lines: {:?}", out(&o));
}

#[test]
fn case36_unix_socket_is_stray_untracked() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let _l = std::os::unix::net::UnixListener::bind(tree.join("sock")).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray untracked sock"), "lines: {:?}", out(&o));
}

// ------------------------------------------- F5: pinned decisions (each dies under its scratch mutation)

#[test]
fn case37_plain_skip_worktree_tag_s_is_hidden() {
    // The plain `S` tag (skip-worktree alone): a lowercase-only tag rule would hide every
    // skip-worktree edit, so this is pinned in its own right (mutation F1b).
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["update-index", "--skip-worktree", "f.txt"]);
    fs::write(tree.join("f.txt"), "edited under skip-worktree alone").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray hidden f.txt"), "lines: {:?}", out(&o));
}

#[test]
fn case38_backslash_in_path_is_doubled() {
    // A literal backslash in a name must print doubled, so it cannot impersonate an escape
    // (mutation M25: `a\b.txt` printed raw).
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::write(tree.join("a\\b.txt"), "stray").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    let binding = out(&o);
    let lines: Vec<&str> = binding.lines().collect();
    assert_eq!(lines.len(), 2, "lines: {:?}", lines);
    assert_eq!(lines[0], "stray untracked a\\\\b.txt");
    assert_eq!(lines[1], "workspace=stray ticket=t1 count=1");
}

#[test]
fn case39_cherry_pick_and_revert_in_progress_exit_3() {
    // CHERRY_PICK_HEAD / REVERT_HEAD are pinned (mutation M28): a conflicted cherry-pick or revert
    // means the tree cannot be judged at all.
    {
        let t = Tmp::new();
        let (_, tree, _) = mk_ws(&t, false);
        fs::write(tree.join("f.txt"), "ticket side").unwrap();
        git(&tree, &["add", "f.txt"]);
        git(&tree, &["commit", "-q", "-m", "c1"]);
        fs::write(tree.join("f.txt"), "ticket side 2").unwrap();
        git(&tree, &["add", "f.txt"]);
        git(&tree, &["commit", "-q", "-m", "c2"]);
        let c1 = git(&tree, &["rev-parse", "HEAD~1"]);
        let o = git_try(&tree, &["cherry-pick", &c1]);
        assert!(!o.status.success(), "fixture: the cherry-pick was meant to conflict");
        let o = check(&t);
        assert_eq!(code(&o), 3, "cherry-pick: {}\n{}", out(&o), err(&o));
        assert!(out(&o).is_empty(), "stdout must stay empty on exit 3");
    }
    {
        let t = Tmp::new();
        let (_, tree, _) = mk_ws(&t, false);
        fs::write(tree.join("f.txt"), "ticket side").unwrap();
        git(&tree, &["add", "f.txt"]);
        git(&tree, &["commit", "-q", "-m", "c1"]);
        fs::write(tree.join("f.txt"), "ticket side 2").unwrap();
        git(&tree, &["add", "f.txt"]);
        git(&tree, &["commit", "-q", "-m", "c2"]);
        let c1 = git(&tree, &["rev-parse", "HEAD~1"]);
        let o = git_try(&tree, &["revert", "--no-edit", &c1]);
        assert!(!o.status.success(), "fixture: the revert was meant to conflict");
        let o = check(&t);
        assert_eq!(code(&o), 3, "revert: {}\n{}", out(&o), err(&o));
        assert!(out(&o).is_empty(), "stdout must stay empty on exit 3");
    }
}

#[test]
fn case40_worktree_path_is_a_file_exit_3() {
    // mutation M29: the not-a-directory rule itself must speak.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::remove_dir_all(&tree).unwrap();
    fs::write(&tree, "now a regular file").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(out(&o).is_empty(), "stdout must stay empty on exit 3");
    assert!(err(&o).contains("not a directory"), "the not-a-directory rule itself must speak: {}", err(&o));
}

#[test]
fn case41_dotdot_in_manifest_dir_exit_3() {
    // mutation M31: the `..` refusal in --manifest-dir is pinned.
    let t = Tmp::new();
    let (_, _, _) = mk_ws(&t, false);
    let md = format!("{}/../M", t.raw.join("M").display());
    let args: Vec<String> = ["workspace", "check", "t1", "--manifest-dir", &md]
        .iter().map(|x| x.to_string()).collect();
    let o = run(&args, None);
    assert_eq!(code(&o), 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(out(&o).is_empty(), "stdout must stay empty on exit 3");
}

// ------------------------------------------------- fix round 2 held-out variants (own inventions)

#[test]
fn ho8_fsmonitor_script_never_runs_and_edit_still_caught() {
    // must-fire: repo `core.fsmonitor` pointing at a script: forced off, never executed (no marker),
    // and the edit is still named.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let (script, _) = t.both("fm.sh");
    let (marker, _) = t.both("fm-marker");
    fs::write(&script, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    chmod_x(&script);
    same_size_edit_with_old_mtime(&tree); // the fixture's own git calls run BEFORE the config exists
    git(&tree, &["config", "core.fsmonitor", script.to_str().unwrap()]);
    let o = check(&t);
    assert!(code(&o) == 2 || code(&o) == 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(!marker.exists(), "the check must never execute a repo-configured command");
    if code(&o) == 2 {
        assert!(stray_lines(&o).iter().any(|l| l == "stray modified f.txt"), "lines: {:?}", out(&o));
    }
}

#[test]
fn ho9_untracked_cache_cannot_hide_planted_file() {
    // must-fire: `core.untrackedCache=true` after a first check, then a planted file.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["config", "core.untrackedCache", "true"]);
    let o = check(&t);
    assert_eq!(code(&o), 0, "clean first pass: {}\n{}", out(&o), err(&o));
    fs::write(tree.join("planted.txt"), "stray").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray untracked planted.txt"), "lines: {:?}", out(&o));
}

#[test]
fn ho10_sparse_checkout_cannot_hide_tracked_path() {
    // must-fire: `core.sparseCheckout=true` with a sparse-checkout file hiding a tracked path: the
    // skip-worktree bit it sets is still read (`stray hidden`).
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::create_dir_all(tree.join("d")).unwrap();
    fs::write(tree.join("d/h.txt"), "h").unwrap();
    git(&tree, &["add", "d/h.txt"]);
    git(&tree, &["commit", "-q", "-m", "d"]);
    let gd = gitdir_of(&tree);
    fs::create_dir_all(gd.join("info")).unwrap();
    fs::write(gd.join("info/sparse-checkout"), "/*\n!/d/\n").unwrap();
    git(&tree, &["config", "core.sparseCheckout", "true"]);
    git(&tree, &["read-tree", "-mu", "HEAD"]);
    assert!(!tree.join("d/h.txt").exists(), "fixture: the path must be sparse now");
    let o = check(&t);
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray hidden d/h.txt"), "lines: {:?}", out(&o));
}

#[test]
fn ho11_text_conversion_attributes_refuse() {
    // must-fire: an `info/attributes` text-conversion rule (no filter needed): attributes can change
    // what status compares, so the file's presence is exit 3.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let gd = gitdir_of(&tree);
    fs::create_dir_all(gd.join("info")).unwrap();
    fs::write(gd.join("info/attributes"), "f.txt text eol=crlf\n").unwrap();
    fs::write(tree.join("f.txt"), "edited").unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(out(&o).is_empty(), "stdout must stay empty on exit 3");
    assert!(err(&o).contains("attributes"), "stderr names the attributes file: {}", err(&o));
}

#[test]
fn ho12_worktree_config_cannot_hide_same_size_edit() {
    // must-fire: `extensions.worktreeConfig=true` plus `config.worktree` with `core.trustctime=false`:
    // the command-line forcing outranks the worktree scope, never exit 0.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    git(&tree, &["config", "extensions.worktreeConfig", "true"]);
    git(&tree, &["config", "--worktree", "core.trustctime", "false"]);
    same_size_edit_with_old_mtime(&tree);
    let o = check(&t);
    assert!(code(&o) == 2 || code(&o) == 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    if code(&o) == 2 {
        assert!(stray_lines(&o).iter().any(|l| l == "stray modified f.txt"), "lines: {:?}", out(&o));
    }
}

// must-fire: inherited GIT_INDEX_FILE / GIT_CONFIG_COUNT / GIT_CONFIG_GLOBAL (a filter file):
// stripped, so the real tree is judged and the filter file is never read (no marker).

#[test]
fn ho13a_caller_git_index_file_is_stripped() {
    let t = Tmp::new();
    let (r, tree, _) = mk_ws(&t, false);
    same_size_edit_with_old_mtime(&tree);
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let mut c = Command::new(BIN);
    c.args(["workspace", "check", "t1", "--manifest-dir", t.raw.join("M").to_str().unwrap()])
        .env("GIT_INDEX_FILE", gitdir_of(&r).join("index"))
        .current_dir(&unrelated)
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert!(code(&o) == 2 || code(&o) == 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    if code(&o) == 2 {
        assert!(stray_lines(&o).iter().any(|l| l == "stray modified f.txt"), "lines: {:?}", out(&o));
    }
}

#[test]
fn ho13b_caller_git_config_env_is_stripped() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    same_size_edit_with_old_mtime(&tree);
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let mut c = Command::new(BIN);
    c.args(["workspace", "check", "t1", "--manifest-dir", t.raw.join("M").to_str().unwrap()])
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.trustctime")
        .env("GIT_CONFIG_VALUE_0", "false")
        .current_dir(&unrelated)
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert!(code(&o) == 2 || code(&o) == 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    if code(&o) == 2 {
        assert!(stray_lines(&o).iter().any(|l| l == "stray modified f.txt"), "lines: {:?}", out(&o));
    }
}

#[test]
fn ho13c_caller_git_config_global_filter_is_stripped() {
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let (marker, _) = t.both("ho13c-marker");
    let (gcfg, _) = t.both("ho13c.gitconfig");
    fs::write(&gcfg, format!("[filter \"h\"]\n\tclean = touch {}\n", marker.display())).unwrap();
    same_size_edit_with_old_mtime(&tree);
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let mut c = Command::new(BIN);
    c.args(["workspace", "check", "t1", "--manifest-dir", t.raw.join("M").to_str().unwrap()])
        .env("GIT_CONFIG_GLOBAL", &gcfg)
        .current_dir(&unrelated)
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert!(code(&o) == 2 || code(&o) == 3, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(!marker.exists(), "a caller-pointed filter file is never read, let alone run");
    if code(&o) == 2 {
        assert!(stray_lines(&o).iter().any(|l| l == "stray modified f.txt"), "lines: {:?}", out(&o));
    }
}

#[test]
fn ho14_nested_submodule_hidden_bit_is_stray() {
    // must-fire: a submodule inside a submodule, assume-unchanged edit in the INNER one: the
    // recursion descends and names the inner submodule itself.
    let t = Tmp::new();
    let (s2_raw, _) = t.both("S2");
    fs::create_dir_all(&s2_raw).unwrap();
    git(&s2_raw, &["init", "-q"]);
    fs::write(s2_raw.join("inner.txt"), "i").unwrap();
    git(&s2_raw, &["add", "inner.txt"]);
    git(&s2_raw, &["commit", "-q", "-m", "i1"]);
    let (s1_raw, _) = t.both("S1");
    fs::create_dir_all(&s1_raw).unwrap();
    git(&s1_raw, &["init", "-q"]);
    fs::write(s1_raw.join("a.txt"), "a").unwrap();
    git(&s1_raw, &["add", "a.txt"]);
    git(&s1_raw, &["commit", "-q", "-m", "s1"]);
    git(&s1_raw, &["-c", "protocol.file.allow=always", "submodule", "add", "--name", "inner", s2_raw.to_str().unwrap(), "inner"]);
    git(&s1_raw, &["add", ".gitmodules"]);
    git(&s1_raw, &["commit", "-q", "-m", "add inner"]);
    let (r, _, _) = mk_ws(&t, false);
    git(&r, &["-c", "protocol.file.allow=always", "submodule", "add", "--name", "sub", s1_raw.to_str().unwrap(), "sub"]);
    git(&r, &["add", ".gitmodules"]);
    git(&r, &["commit", "-q", "-m", "add sub"]);
    create_id(&t, "t25", "W25", "M25");
    let (tree, _) = t.both("W25/t25/tree");
    git(&tree, &["-c", "protocol.file.allow=always", "submodule", "update", "--init", "--recursive"]);
    let inner = tree.join("sub/inner");
    git(&inner, &["update-index", "--assume-unchanged", "inner.txt"]);
    fs::write(inner.join("inner.txt"), "edited in the inner submodule").unwrap();
    let o = check_id(&t, "t25", "M25");
    assert_eq!(code(&o), 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(stray_lines(&o).iter().any(|l| l == "stray submodule sub/inner"), "lines: {:?}", out(&o));
}

#[test]
fn ho15_caller_global_excludes_file_cannot_hide() {
    // must-fire: GIT_CONFIG_GLOBAL pointing at a config whose core.excludesFile hides a planted
    // file: the env name is stripped, so the file is judged in the real tree.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    let (excl, _) = t.both("excl");
    fs::write(&excl, "planted.txt\n").unwrap();
    let (gcfg, _) = t.both("gitconfig-excl");
    fs::write(&gcfg, format!("[core]\n\texcludesFile = {}\n", excl.display())).unwrap();
    fs::write(tree.join("planted.txt"), "stray").unwrap();
    let (unrelated, _) = t.both("unrelated");
    fs::create_dir_all(&unrelated).unwrap();
    let mut c = Command::new(BIN);
    c.args(["workspace", "check", "t1", "--manifest-dir", t.raw.join("M").to_str().unwrap()])
        .env("GIT_CONFIG_GLOBAL", &gcfg)
        .current_dir(&unrelated)
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert!(code(&o) == 2, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert!(
        stray_lines(&o).iter().any(|l| l == "stray untracked planted.txt" || l == "stray ignored planted.txt"),
        "lines: {:?}", out(&o)
    );
}

#[test]
fn ho16_committed_symlink_and_node_modules_link_stay_clean() {
    // must-not-fire: the expected node_modules link plus a clean tracked tree containing a committed
    // symlink: both are expected, exit 0.
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, true);
    std::os::unix::fs::symlink("f.txt", tree.join("alias")).unwrap();
    git(&tree, &["add", "alias"]);
    git(&tree, &["commit", "-q", "-m", "committed symlink"]);
    let o = check(&t);
    assert_eq!(code(&o), 0, "stdout: {}\nstderr: {}", out(&o), err(&o));
    assert_eq!(out(&o), "workspace=clean ticket=t1\n");
}
