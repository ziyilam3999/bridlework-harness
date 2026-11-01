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

    // (e) worktree path replaced by a symlink
    let t = Tmp::new();
    let (_, tree, _) = mk_ws(&t, false);
    fs::remove_dir_all(&tree).unwrap();
    let (outside, _) = t.both("outside");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, &tree).unwrap();
    let o = check(&t);
    assert_eq!(code(&o), 3, "worktree is a symlink: {}\n{}", out(&o), err(&o));
    assert!(out(&o).is_empty());

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
