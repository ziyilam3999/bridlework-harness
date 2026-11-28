//! Process-level tests for `harness ticket risk`. The real binary runs as a child against a temp git repo
//! built with local git only (no network). Each refusal test checks one exact stderr line and an empty
//! stdout. No test waits on a clock.
#![cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const USAGE: &str = "usage: harness ticket risk <ticket-file> --repo-dir <absolute-dir> --from <full-sha> --to <full-sha> [--override <low|high> --by <jev|model|human> --reason <text> --log <absolute-file>]";
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("risk-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn gitcmd(dir: &Path, args: &[&str]) -> Command {
    let contact = format!("user.email=t{}scratch", '@');
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str(), "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        c.env_remove(k);
    }
    c.stdin(Stdio::null());
    c
}

fn g(dir: &Path, args: &[&str]) -> String {
    let out = gitcmd(dir, args).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A scratch git call that reads `input` from stdin.
fn g_stdin(dir: &Path, args: &[&str], input: &[u8]) -> String {
    let mut c = gitcmd(dir, args);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path) -> String {
    g(dir, &["commit", "-q", "-m", "m"]);
    g(dir, &["rev-parse", "HEAD"])
}

/// Writes each file (making its folder), stages everything and commits; returns the new sha.
fn commit_all<B: AsRef<[u8]>>(dir: &Path, files: &[(&str, B)]) -> String {
    for (p, b) in files {
        let f = dir.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, b).unwrap();
    }
    g(dir, &["add", "-A"]);
    commit(dir)
}

struct Repo {
    t: Tmp,
    dir: PathBuf,
}

fn repo() -> Repo {
    let t = Tmp::new();
    let dir = t.0.join("repo");
    fs::create_dir_all(&dir).unwrap();
    g(&dir, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(&dir, &["config", k, v]);
    }
    Repo { t, dir }
}

/// A valid ticket file `id` with the given `risk`, written beside the repo.
fn ticket(r: &Repo, id: &str, risk: &str) -> PathBuf {
    let f = r.t.0.join(format!("{id}.json"));
    fs::write(&f, ticket_text(id, risk)).unwrap();
    f
}

fn ticket_text(id: &str, risk: &str) -> String {
    format!("{{\"id\":\"{id}\",\"repo_id\":\"scratch-1\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"S\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"{risk}\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}")
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn run_raw(args: &[String]) -> Out {
    let mut c = Command::new(BIN);
    c.args(["ticket", "risk"]).args(args).stdin(Stdio::null());
    let o = c.output().unwrap();
    Out { code: o.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&o.stdout).into_owned(), stderr: String::from_utf8_lossy(&o.stderr).into_owned() }
}

fn args_of(r: &Repo, ticket: &Path, from: &str, to: &str, extra: &[&str]) -> Vec<String> {
    let mut a = s(&[ticket.to_str().unwrap(), "--repo-dir", r.dir.to_str().unwrap(), "--from", from, "--to", to]);
    a.extend(s(extra));
    a
}

fn run(r: &Repo, ticket: &Path, from: &str, to: &str, extra: &[&str]) -> Out {
    run_raw(&args_of(r, ticket, from, to, extra))
}

fn lines(o: &Out) -> Vec<&str> {
    o.stdout.lines().collect()
}

fn last(o: &Out) -> &str {
    o.stdout.lines().next_back().unwrap_or("")
}

fn raising(log: &Path) -> Vec<String> {
    s(&["--override", "high", "--by", "jev", "--reason", "ok", "--log", log.to_str().unwrap()])
}

// Mutant: accept a lowering override, read the map from the work tree or --to, let a map entry lower a
// gates path, or drop a check below, and the named test turns red.

/// Low end: every changed path is covered by a `low` entry, the ticket is `normal`, and the same inputs give
/// the same bytes twice.
#[test]
fn risk_mapped_low_diff_is_low_and_same_twice() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "# m\n\ndocs low\nsrc low\n"), ("docs/a.md", "one\n"), ("src/b.rs", "fn a() {}\n")]);
    let head = commit_all(&r.dir, &[("docs/a.md", "two\n"), ("src/b.rs", "fn b() {}\n")]);
    let t = ticket(&r, "T-1", "normal");
    let want = format!("path=docs/a.md class=low why=mapped-low\npath=src/b.rs class=low why=mapped-low\nrisk=low ticket=T-1 from={base} to={head} files=2 why=mapped-low\n");
    let a = run(&r, &t, &base, &head, &[]);
    assert_eq!(a.code, 0, "stderr: {}", a.stderr);
    assert_eq!(a.stderr, "");
    assert_eq!(a.stdout, want);
    let b = run(&r, &t, &base, &head, &[]);
    assert_eq!(b.stdout, a.stdout, "a second run gives byte-equal stdout");
    assert_eq!((b.code, b.stderr.as_str()), (0, ""));
    // The same paths swapped between the commits: same class, still low.
    let back = commit_all(&r.dir, &[("docs/a.md", "one\n"), ("src/b.rs", "fn a() {}\n")]);
    let c = run(&r, &t, &head, &back, &[]);
    assert_eq!((c.code, c.stderr.as_str()), (0, ""));
    assert_eq!(last(&c), format!("risk=low ticket=T-1 from={head} to={back} files=2 why=mapped-low"));
    // Planted twin: the declared `normal` alone gives nothing. A path no entry covers is high.
    let away = commit_all(&r.dir, &[("src/b.rs", "fn c() {}\n"), ("other/x.md", "x\n")]);
    let d = run(&r, &t, &back, &away, &[]);
    assert_eq!(d.code, 0, "stderr: {}", d.stderr);
    assert_eq!(d.stdout, format!("path=other/x.md class=high why=unmapped\npath=src/b.rs class=low why=mapped-low\nrisk=high ticket=T-1 from={back} to={away} files=2 why=unmapped\n"));
}

/// High end: a `gates/` path and the map file are high for a ticket that declares `normal`, also when the
/// map says `low` for them.
#[test]
fn risk_gates_path_is_high_even_when_ticket_is_normal() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\ngates low\ngates/risk-map.txt low\n"), ("gates/x.json", "{}\n"), ("docs/a.md", "one\n")]);
    let head = commit_all(&r.dir, &[("gates/x.json", "{\n}\n")]);
    let t = ticket(&r, "T-1", "normal");
    let o = run(&r, &t, &base, &head, &[]);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr, "");
    assert_eq!(lines(&o), vec!["path=gates/x.json class=high why=gates-path", format!("risk=high ticket=T-1 from={base} to={head} files=1 why=gates-path").as_str()]);
    // Twin: the same ticket on a low path of the same base is low (the gate file is back at its base bytes).
    let low = commit_all(&r.dir, &[("gates/x.json", "{}\n"), ("docs/a.md", "two\n")]);
    let o = run(&r, &t, &base, &low, &[]);
    assert_eq!(last(&o), format!("risk=low ticket=T-1 from={base} to={low} files=1 why=mapped-low"));
}

/// An edit to the risk path map is risk-routed: the map file is high by itself.
#[test]
fn risk_map_edit_is_high() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "# one\ndocs low\n"), ("docs/a.md", "one\n")]);
    let head = commit_all(&r.dir, &[("gates/risk-map.txt", "# two\ndocs low\n")]);
    let t = ticket(&r, "T-1", "normal");
    let o = run(&r, &t, &base, &head, &[]);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr, "");
    assert_eq!(o.stdout, format!("path=gates/risk-map.txt class=high why=risk-map\nrisk=high ticket=T-1 from={base} to={head} files=1 why=risk-map\n"));
    // A delete of the map counts as a change of it too.
    fs::remove_file(r.dir.join("gates/risk-map.txt")).unwrap();
    let gone = commit_all(&r.dir, &[] as &[(&str, &[u8])]);
    let o = run(&r, &t, &base, &gone, &[]);
    assert_eq!((o.code, o.stderr.as_str()), (0, ""));
    assert_eq!(last(&o), format!("risk=high ticket=T-1 from={base} to={gone} files=1 why=risk-map"));
}

/// A path no entry covers and a path under a `high` entry are high; the summary why is the why of the first
/// high path in git's order. The longest entry answers (whole components).
#[test]
fn risk_unmapped_and_mapped_high_paths_are_high() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "# m\ndocs low\nci high\n"), ("docs/a.md", "one\n"), ("ci/d.sh", "one\n")]);
    let head = commit_all(&r.dir, &[("docs/a.md", "two\n"), ("ci/d.sh", "two\n"), ("newdir/c.txt", "c\n")]);
    let t = ticket(&r, "T-1", "normal");
    let o = run(&r, &t, &base, &head, &[]);
    assert_eq!(o.code, 0, "stderr: {}", o.stderr);
    assert_eq!(o.stderr, "");
    assert_eq!(
        o.stdout,
        format!(
            "path=ci/d.sh class=high why=mapped-high\npath=docs/a.md class=low why=mapped-low\npath=newdir/c.txt class=high why=unmapped\nrisk=high ticket=T-1 from={base} to={head} files=3 why=mapped-high\n"
        )
    );
    // The low path changes too: the summary why is the why of the first high path (the only one).
    let only = commit_all(&r.dir, &[("ci/d.sh", "one\n"), ("docs/a.md", "three\n")]);
    let o = run(&r, &t, &base, &only, &[]);
    assert_eq!(o.stdout, format!("path=docs/a.md class=low why=mapped-low\npath=newdir/c.txt class=high why=unmapped\nrisk=high ticket=T-1 from={base} to={only} files=2 why=unmapped\n"));
    // Nested entries: the longest answers; a sibling that shares a string prefix is not under it.
    let n = repo();
    let nbase = commit_all(&n.dir, &[("gates/risk-map.txt", "docs low\ndocs/api high\n")]);
    let nhead = commit_all(&n.dir, &[("docs/api/x.md", "x\n"), ("docs/apix/y.md", "y\n")]);
    let o = run(&n, &ticket(&n, "T-9", "normal"), &nbase, &nhead, &[]);
    assert_eq!(o.stdout, format!("path=docs/api/x.md class=high why=mapped-high\npath=docs/apix/y.md class=low why=mapped-low\nrisk=high ticket=T-9 from={nbase} to={nhead} files=2 why=mapped-high\n"));
}

/// The map of `--from` judges the diff: never the work tree and never the map at `--to`.
#[test]
fn risk_map_is_read_from_the_base_commit() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\n"), ("docs/a.md", "one\n")]);
    let head = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\nnewdir low\n"), ("newdir/e.txt", "e\n")]);
    let t = ticket(&r, "T-1", "normal");
    let want = format!("path=gates/risk-map.txt class=high why=risk-map\npath=newdir/e.txt class=high why=unmapped\nrisk=high ticket=T-1 from={base} to={head} files=2 why=risk-map\n");
    let o = run(&r, &t, &base, &head, &[]);
    assert_eq!((o.code, o.stderr.as_str()), (0, ""), "{}", o.stdout);
    assert_eq!(o.stdout, want, "the map at --from judges: newdir is not low at --from");
    // The work tree is not read: a malformed map on disk, and no map on disk, change nothing.
    fs::write(r.dir.join("gates/risk-map.txt"), "docs medium\n").unwrap();
    let o = run(&r, &t, &base, &head, &[]);
    assert_eq!(o.stdout, want, "a bad map in the work tree is not read");
    fs::remove_file(r.dir.join("gates/risk-map.txt")).unwrap();
    let o = run(&r, &t, &base, &head, &[]);
    assert_eq!((o.code, o.stdout.as_str()), (0, want.as_str()), "a missing map in the work tree is not read");
    // Twin: with --from at the commit that maps newdir low, the same new file is low (that map is the judge).
    g(&r.dir, &["checkout", "-q", &head, "--", "."]);
    let more = commit_all(&r.dir, &[("newdir/f.txt", "f\n")]);
    let o = run(&r, &t, &head, &more, &[]);
    assert_eq!(o.stdout, format!("path=newdir/f.txt class=low why=mapped-low\nrisk=low ticket=T-1 from={head} to={more} files=1 why=mapped-low\n"));
    // Twin: a base before the map existed is refused, though the head has one and the work tree has one.
    let e = repo();
    let ebase = commit_all(&e.dir, &[("docs/a.md", "one\n")]);
    let ehead = commit_all(&e.dir, &[("docs/a.md", "two\n"), ("gates/risk-map.txt", "docs low\n")]);
    let o = run(&e, &ticket(&e, "T-1", "normal"), &ebase, &ehead, &[]);
    assert_eq!((o.code, o.stdout.as_str()), (2, ""));
    assert_eq!(o.stderr.lines().count(), 1, "{}", o.stderr);
    assert!(o.stderr.starts_with("harness: error: ") && o.stderr.contains("is missing"), "{}", o.stderr);
}

/// `--repo-dir` is the top of the repo. A folder inside is refused, so the map (one file at the top) and the
/// changed paths (named from the top) cannot be read against two different folders.
#[test]
fn risk_repo_dir_inside_the_repo_is_refused() {
    let r = repo();
    // A second map of the same name under docs/, which a read relative to the run folder takes for the map.
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\n"), ("docs/gates/risk-map.txt", "core low\ngates low\n"), ("docs/a.md", "one\n"), ("core/x.rs", "one\n")]);
    let head = commit_all(&r.dir, &[("core/x.rs", "two\n")]);
    let t = ticket(&r, "T-1", "normal");
    let log = r.t.0.join("never.log");
    for inside in ["docs", "docs/gates"] {
        let mut a = s(&[t.to_str().unwrap(), "--repo-dir", r.dir.join(inside).to_str().unwrap(), "--from", &base, "--to", &head]);
        a.extend(raising(&log));
        let o = run_raw(&a);
        assert_eq!((o.code, o.stdout.as_str()), (2, ""), "{inside}: stderr: {}", o.stderr);
        assert_eq!(o.stderr.lines().count(), 1, "{inside}: {}", o.stderr);
        assert_eq!(o.stderr, "harness: error: --repo-dir is not the top of the repo\n", "{inside}");
        assert!(!log.exists(), "{inside}: writes no log");
    }
    // Twins: each top of the same history answers, and core/x.rs is unmapped there (the top map is the
    // judge): the repo itself, a linked work tree, and a bare clone (a repo with no work tree).
    let wt = r.t.0.join("wt");
    g(&r.dir, &["worktree", "add", "-q", wt.to_str().unwrap(), &head]);
    let bare = r.t.0.join("bare.git");
    g(&r.dir, &["clone", "-q", "--bare", r.dir.to_str().unwrap(), bare.to_str().unwrap()]);
    for top in [&r.dir, &wt, &bare] {
        let o = run_raw(&s(&[t.to_str().unwrap(), "--repo-dir", top.to_str().unwrap(), "--from", &base, "--to", &head]));
        assert_eq!((o.code, o.stderr.as_str()), (0, ""), "{}: stderr: {}", top.display(), o.stderr);
        assert_eq!(o.stdout, format!("path=core/x.rs class=high why=unmapped\nrisk=high ticket=T-1 from={base} to={head} files=1 why=unmapped\n"), "{}", top.display());
    }
}

/// An override lower than the computed class is refused by every author: exit 1, one stderr line, no
/// stdout, nothing written to the log.
#[test]
fn risk_lowering_override_is_refused() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\nci high\n"), ("docs/a.md", "one\n"), ("ci/d.sh", "one\n")]);
    let head = commit_all(&r.dir, &[("ci/d.sh", "two\n")]);
    let log = r.t.0.join("override.log");
    for by in ["human", "jev", "model"] {
        let o = run(&r, &ticket(&r, "T-1", "normal"), &base, &head, &["--override", "low", "--by", by, "--reason", "r", "--log", log.to_str().unwrap()]);
        assert_eq!(o.code, 1, "{by}: stderr: {}", o.stderr);
        assert_eq!(o.stdout, "", "{by}: nothing on stdout");
        assert_eq!(o.stderr, "harness: risk refused: override-lowers computed=high override=low\n", "{by}");
        assert!(!log.exists(), "{by}: a refused override writes no log file");
    }
    // The ticket's own `high` is part of the computed class: a low diff with a high ticket cannot be lowered.
    let low_head = commit_all(&r.dir, &[("ci/d.sh", "one\n"), ("docs/a.md", "two\n")]);
    let o = run(&r, &ticket(&r, "T-2", "high"), &base, &low_head, &["--override", "low", "--by", "human", "--reason", "r", "--log", log.to_str().unwrap()]);
    assert_eq!((o.code, o.stdout.as_str()), (1, ""));
    assert_eq!(o.stderr, "harness: risk refused: override-lowers computed=high override=low\n");
    assert!(!log.exists());
    // A log that exists already is left as it was.
    fs::write(&log, "old\n").unwrap();
    let o = run(&r, &ticket(&r, "T-1", "normal"), &base, &head, &["--override", "low", "--by", "model", "--reason", "r", "--log", log.to_str().unwrap()]);
    assert_eq!(o.code, 1);
    assert_eq!(fs::read_to_string(&log).unwrap(), "old\n");
}

/// An override that raises, or keeps the class, is accepted and one line is appended to the log.
#[test]
fn risk_raising_override_is_logged() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\n"), ("docs/a.md", "one\n")]);
    let head = commit_all(&r.dir, &[("docs/a.md", "two\n")]);
    let t = ticket(&r, "T-1", "normal");
    let log = r.t.0.join("override.log");
    let o = run(&r, &t, &base, &head, &["--override", "high", "--by", "jev", "--reason", "because", "--log", log.to_str().unwrap()]);
    assert_eq!((o.code, o.stderr.as_str()), (0, ""), "{}", o.stdout);
    assert_eq!(o.stdout, format!("path=docs/a.md class=low why=mapped-low\nrisk=high ticket=T-1 from={base} to={head} files=1 why=override\n"));
    assert_eq!(fs::read_to_string(&log).unwrap(), format!("risk-override ticket=T-1 from={base} to={head} computed=low override=high by=jev effect=raised reason=because\n"));
    // A second accepted call appends a second line.
    let o = run(&r, &t, &base, &head, &["--override", "high", "--by", "model", "--reason", "again", "--log", log.to_str().unwrap()]);
    assert_eq!(o.code, 0);
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        format!("risk-override ticket=T-1 from={base} to={head} computed=low override=high by=jev effect=raised reason=because\nrisk-override ticket=T-1 from={base} to={head} computed=low override=high by=model effect=raised reason=again\n")
    );
    // An override equal to the class is accepted and logged as `same`.
    let high_head = commit_all(&r.dir, &[("ci/d.sh", "d\n")]);
    let log2 = r.t.0.join("same.log");
    let o = run(&r, &t, &base, &high_head, &["--override", "high", "--by", "human", "--reason", "already high", "--log", log2.to_str().unwrap()]);
    assert_eq!((o.code, o.stderr.as_str()), (0, ""), "{}", o.stderr);
    assert_eq!(last(&o), format!("risk=high ticket=T-1 from={base} to={high_head} files=2 why=override"));
    assert_eq!(fs::read_to_string(&log2).unwrap(), format!("risk-override ticket=T-1 from={base} to={high_head} computed=high override=high by=human effect=same reason=already high\n"));
    let log3 = r.t.0.join("keep.log");
    let o = run(&r, &t, &base, &head, &["--override", "low", "--by", "model", "--reason", "keep it low", "--log", log3.to_str().unwrap()]);
    assert_eq!(o.code, 0);
    assert_eq!(last(&o), format!("risk=low ticket=T-1 from={base} to={head} files=1 why=override"));
    assert_eq!(fs::read_to_string(&log3).unwrap(), format!("risk-override ticket=T-1 from={base} to={head} computed=low override=low by=model effect=same reason=keep it low\n"));
    // A log that does not end with a newline gets one first, so the new line is a whole line.
    let log4 = r.t.0.join("open.log");
    fs::write(&log4, "old").unwrap();
    let o = run(&r, &t, &base, &head, &["--override", "high", "--by", "jev", "--reason", "r", "--log", log4.to_str().unwrap()]);
    assert_eq!(o.code, 0);
    assert_eq!(fs::read_to_string(&log4).unwrap(), format!("old\nrisk-override ticket=T-1 from={base} to={head} computed=low override=high by=jev effect=raised reason=r\n"));
    // Twin: with no override flags no log file is written.
    let log5 = r.t.0.join("never.log");
    assert!(!log5.exists());
    let o = run(&r, &t, &base, &head, &[]);
    assert_eq!((o.code, last(&o)), (0, format!("risk=low ticket=T-1 from={base} to={head} files=1 why=mapped-low").as_str()));
    assert!(!log5.exists());
}

/// A ticket that declares `high` is high on a low diff: the declared field can raise, never lower.
#[test]
fn risk_ticket_high_raises() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\n"), ("docs/a.md", "one\n")]);
    let head = commit_all(&r.dir, &[("docs/a.md", "two\n")]);
    let o = run(&r, &ticket(&r, "T-2", "high"), &base, &head, &[]);
    assert_eq!((o.code, o.stderr.as_str()), (0, ""), "{}", o.stderr);
    assert_eq!(o.stdout, format!("path=docs/a.md class=low why=mapped-low\nrisk=high ticket=T-2 from={base} to={head} files=1 why=ticket-high\n"));
    // Twin: the same diff with a `normal` ticket is low.
    let o = run(&r, &ticket(&r, "T-1", "normal"), &base, &head, &[]);
    assert_eq!(last(&o), format!("risk=low ticket=T-1 from={base} to={head} files=1 why=mapped-low"));
}

/// Each inherited `GIT_` name is removed before git runs: a `GIT_DIR` that points at another repo changes
/// nothing.
#[test]
fn risk_inherited_git_names_are_removed() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\n"), ("docs/a.md", "one\n")]);
    let head = commit_all(&r.dir, &[("docs/a.md", "two\n")]);
    let t = ticket(&r, "T-1", "normal");
    let want = format!("path=docs/a.md class=low why=mapped-low\nrisk=low ticket=T-1 from={base} to={head} files=1 why=mapped-low\n");
    let other = repo();
    let mut c = Command::new(BIN);
    c.args(["ticket", "risk"]).args(args_of(&r, &t, &base, &head, &[])).env("GIT_DIR", other.dir.join(".git")).stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_eq!(o.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), want);
}

/// A changed submodule entry counts as a changed path. The tree compare passes `--ignore-submodules=none`,
/// so an ignore rule of `.gitmodules` (`submodule.<name>.ignore`) does not hide it.
#[test]
fn risk_submodule_change_is_listed() {
    let r = repo();
    let seed = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\nvendor/sub low\n"), (".gitmodules", "[submodule \"sub\"]\n\tpath = vendor/sub\n\tignore = all\n"), ("docs/a.md", "one\n")]);
    // A submodule entry in the index that points at `sha`; git does not check that the commit exists.
    let point = |sha: &str| {
        g(&r.dir, &["update-index", "--add", "--cacheinfo", &format!("160000,{sha},vendor/sub")]);
        commit(&r.dir)
    };
    let base = point(&seed);
    let head = point(&base);
    let o = run(&r, &ticket(&r, "T-1", "normal"), &base, &head, &[]);
    assert_eq!((o.code, o.stderr.as_str()), (0, ""), "{}", o.stderr);
    assert_eq!(o.stdout, format!("path=vendor/sub class=low why=mapped-low\nrisk=low ticket=T-1 from={base} to={head} files=1 why=mapped-low\n"), "the submodule entry is one changed path");
}

/// `--log` must be a plain file: a symlink is refused, also one that points at a plain file, and the file
/// behind the link is left as it was.
#[test]
fn risk_log_symlink_is_refused() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\n"), ("docs/a.md", "one\n")]);
    let head = commit_all(&r.dir, &[("docs/a.md", "two\n")]);
    let t = ticket(&r, "T-1", "normal");
    let real = r.t.0.join("real.log");
    fs::write(&real, "old\n").unwrap();
    let link = r.t.0.join("link.log");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let o = run(&r, &t, &base, &head, &["--override", "high", "--by", "jev", "--reason", "r", "--log", link.to_str().unwrap()]);
    assert_eq!((o.code, o.stdout.as_str()), (2, ""), "{}", o.stderr);
    assert_eq!(o.stderr, "harness: error: --log is not a plain file\n");
    assert_eq!(fs::read_to_string(&real).unwrap(), "old\n", "the file behind the link is not written");
    // A link to a missing file is refused the same way, and no file is created behind it.
    let gone = r.t.0.join("gone.log");
    std::os::unix::fs::symlink(r.t.0.join("no-such.log"), &gone).unwrap();
    let o = run(&r, &t, &base, &head, &["--override", "high", "--by", "jev", "--reason", "r", "--log", gone.to_str().unwrap()]);
    assert_eq!((o.code, o.stdout.as_str()), (2, ""), "{}", o.stderr);
    assert_eq!(o.stderr, "harness: error: --log is not a plain file\n");
    assert!(!r.t.0.join("no-such.log").exists());
    // Twin: the plain file itself takes the line.
    let mut a = args_of(&r, &t, &base, &head, &[]);
    a.extend(raising(&real));
    let o = run_raw(&a);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(fs::read_to_string(&real).unwrap(), format!("old\nrisk-override ticket=T-1 from={base} to={head} computed=low override=high by=jev effect=raised reason=ok\n"));
}

/// Flag-shape errors print the verb's usage line; content errors print one `harness: error: <what>` line.
/// Each is exit 2 with an empty stdout and writes no log.
#[test]
fn risk_refuses_bad_inputs() {
    let r = repo();
    let base = commit_all(&r.dir, &[("gates/risk-map.txt", "docs low\n"), ("docs/a.md", "one\n"), ("ci/d.sh", "one\n")]);
    let head = commit_all(&r.dir, &[("docs/a.md", "two\n")]);
    let t = ticket(&r, "T-1", "normal");
    let log = r.t.0.join("never.log");
    let lost = "1".repeat(40);

    let check_usage = |name: &str, o: Out| {
        assert_eq!(o.code, 2, "{name}: stderr: {}", o.stderr);
        assert_eq!(o.stdout, "", "{name}: stdout is not empty");
        assert_eq!(o.stderr, format!("{USAGE}\n"), "{name}");
        assert!(!log.exists(), "{name}: writes no log");
    };
    let plain = |extra: &[&str]| run(&r, &t, &base, &head, extra);
    check_usage("relative --repo-dir", run_raw(&s(&[t.to_str().unwrap(), "--repo-dir", "rel/repo", "--from", base.as_str(), "--to", head.as_str()])));
    check_usage("--repo-dir with a dot-dot part", run_raw(&s(&[t.to_str().unwrap(), "--repo-dir", &format!("{}/../repo", r.dir.display()), "--from", base.as_str(), "--to", head.as_str()])));
    check_usage("no ticket file", run_raw(&s(&["--repo-dir", r.dir.to_str().unwrap(), "--from", base.as_str(), "--to", head.as_str()])));
    check_usage("two ticket files", run_raw(&s(&[t.to_str().unwrap(), t.to_str().unwrap(), "--repo-dir", r.dir.to_str().unwrap(), "--from", base.as_str(), "--to", head.as_str()])));
    check_usage("--from twice", plain(&["--from", &head]));
    check_usage("--from is not a full sha", run(&r, &t, "abc123", &head, &[]));
    check_usage("unknown flag", plain(&["--base", &base]));
    check_usage("equals form", plain(&["--from=x"]));
    check_usage("a flag with no value", plain(&["--to"]));
    check_usage("--override without --by", plain(&["--override", "high", "--reason", "r", "--log", log.to_str().unwrap()]));
    check_usage("--override without --reason", plain(&["--override", "high", "--by", "jev", "--log", log.to_str().unwrap()]));
    check_usage("--override without --log", plain(&["--override", "high", "--by", "jev", "--reason", "r"]));
    check_usage("--log without --override", plain(&["--log", log.to_str().unwrap()]));
    check_usage("--override other than low or high", plain(&["--override", "medium", "--by", "jev", "--reason", "r", "--log", log.to_str().unwrap()]));
    check_usage("--by other", plain(&["--override", "high", "--by", "other", "--reason", "r", "--log", log.to_str().unwrap()]));
    check_usage("empty --reason", plain(&["--override", "high", "--by", "human", "--reason", "", "--log", log.to_str().unwrap()]));
    check_usage("blank --reason", plain(&["--override", "high", "--by", "human", "--reason", "   ", "--log", log.to_str().unwrap()]));
    check_usage("multi-line --reason", plain(&["--override", "high", "--by", "human", "--reason", "a\nb", "--log", log.to_str().unwrap()]));
    check_usage("--reason over 200 bytes", plain(&["--override", "high", "--by", "human", "--reason", &"r".repeat(201), "--log", log.to_str().unwrap()]));
    check_usage("relative --log", plain(&["--override", "high", "--by", "human", "--reason", "r", "--log", "never.log"]));

    // Twin: a 200 byte reason is accepted, so the cap is the only reason the line above is refused.
    let cap = r.t.0.join("cap.log");
    let o = plain(&["--override", "high", "--by", "human", "--reason", &"r".repeat(200), "--log", cap.to_str().unwrap()]);
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(fs::read_to_string(&cap).unwrap(), format!("risk-override ticket=T-1 from={base} to={head} computed=low override=high by=human effect=raised reason={}\n", "r".repeat(200)));

    // Content errors: each with a full raising override, so "writes no log" is a real check.
    let no_risk = r.t.0.join("no-risk.json");
    fs::write(&no_risk, ticket_text("T-1", "normal").replace("\"risk\":\"normal\",", "")).unwrap();
    let not_json = r.t.0.join("not-json.json");
    fs::write(&not_json, "{not json").unwrap();
    let plain_dir = r.t.0.join("plain");
    fs::create_dir_all(&plain_dir).unwrap();
    let over = raising(&log);

    fs::write(r.t.0.join("blank.json"), ticket_text("T 1", "normal")).unwrap();
    let text_of = |rel: &str| r.t.0.join(rel).to_str().unwrap().to_string();
    let (nope, blank, no_dir) = (text_of("nope.json"), text_of("blank.json"), text_of("no-such-dir"));
    let (lost, plain_dir) = (lost.clone(), plain_dir.to_str().unwrap().to_string());

    type Edit = Box<dyn Fn(&mut Vec<String>)>;
    let edit = |at: usize| move |v: String| Box::new(move |a: &mut Vec<String>| a[at] = v.clone()) as Box<dyn Fn(&mut Vec<String>)>;
    let cases: Vec<(&str, Edit, &str)> = vec![
        ("ticket file missing", edit(0)(nope), "the ticket file is missing"),
        ("ticket not json", edit(0)(not_json.to_str().unwrap().to_string()), "the ticket file is refused"),
        ("ticket without the risk key", edit(0)(no_risk.to_str().unwrap().to_string()), "`risk`"),
        ("ticket id with a blank", edit(0)(blank), "plain token"),
        ("repo dir missing", edit(2)(no_dir), "missing or not a folder"),
        ("repo dir is not a git repo", edit(2)(plain_dir), "not a git repo"),
        ("--from not a commit", edit(4)(lost.clone()), "--from is not a commit of the repo"),
        ("--to not a commit", edit(6)(lost), "--to is not a commit of the repo"),
        ("--from equal to --to", Box::new(|a| a[6] = a[4].clone()), "--from equals --to"),
    ];
    for (name, edit, needle) in cases {
        let mut a = args_of(&r, &t, &base, &head, &[]);
        a.extend(over.clone());
        edit(&mut a);
        let o = run_raw(&a);
        assert_eq!(o.code, 2, "{name}: stderr: {}", o.stderr);
        assert_eq!(o.stdout, "", "{name}: stdout is not empty");
        assert_eq!(o.stderr.lines().count(), 1, "{name}: one stderr line, got: {}", o.stderr);
        assert!(o.stderr.starts_with("harness: error: ") && o.stderr.contains(needle), "{name}: {}", o.stderr);
        assert!(!log.exists(), "{name}: writes no log");
    }

    // Map faults: each is the base commit of its own repo, and each head of the same repo has a good map.
    // The diff base to head is then judged (exit 0): the fault is the only reason the base is refused.
    let good = [("gates/risk-map.txt", "docs low\n"), ("docs/a.md", "two\n")];
    let with_map = |map: &[u8]| {
        let x = repo();
        let b = commit_all(&x.dir, &[("gates/risk-map.txt", map), ("docs/a.md", &b"one\n"[..])]);
        let h = commit_all(&x.dir, &good);
        (x, b, h)
    };
    let missing = repo();
    let missing_base = commit_all(&missing.dir, &[("docs/a.md", "one\n")]);
    let missing_head = commit_all(&missing.dir, &good);
    let (empty, empty_base, empty_head) = with_map(b"# only comments\n\n   \n");
    let (bad_class, bad_class_base, bad_class_head) = with_map(b"docs medium\n");
    let (dotdot, dotdot_base, dotdot_head) = with_map(b"../x low\n");
    let (twice, twice_base, twice_head) = with_map(b"docs low\ndocs low\n");
    let (utf8, utf8_base, utf8_head) = with_map(b"\xff\n");
    let folder = repo();
    let folder_base = commit_all(&folder.dir, &[("gates/risk-map.txt/inner.txt", "docs low\n"), ("docs/a.md", "one\n")]);
    fs::remove_dir_all(folder.dir.join("gates")).unwrap();
    let folder_head = commit_all(&folder.dir, &good);
    // A symlink of the map name is a blob too (its content is the link target): only the mode tells it.
    let link = repo();
    commit_all(&link.dir, &[("docs/a.md", "one\n")]);
    let target = g_stdin(&link.dir, &["hash-object", "-w", "--stdin"], b"docs/rule.txt");
    g(&link.dir, &["update-index", "--add", "--cacheinfo", &format!("120000,{target},gates/risk-map.txt")]);
    let link_base = commit(&link.dir);
    let link_head = commit_all(&link.dir, &good);
    // A map with valid entries only, over the 1 MiB cap.
    let big: String = (0..50_000).map(|i| format!("dir{i:06}/file.txt low\n")).collect();
    assert!(big.len() > 1024 * 1024, "the big map is over 1 MiB");
    let (huge, huge_base, huge_head) = with_map(big.as_bytes());

    // Each needle is the whole text after `harness: error: the risk map gates/risk-map.txt at --from `.
    const MAP: &str = "the risk map gates/risk-map.txt at --from ";
    let maps: Vec<(&str, &Repo, &str, &str, &str)> = vec![
        ("base with no map", &missing, &missing_base, &missing_head, "is missing or not a plain file"),
        ("map with zero entries", &empty, &empty_base, &empty_head, "has no entries"),
        ("map line with another class", &bad_class, &bad_class_base, &bad_class_head, "line 1 is malformed"),
        ("map line with dot-dot", &dotdot, &dotdot_base, &dotdot_head, "line 1 has a path that is not plain"),
        ("map path twice", &twice, &twice_base, &twice_head, "line 2 gives one path twice"),
        ("map not utf-8", &utf8, &utf8_base, &utf8_head, "is not utf-8"),
        ("map is a folder", &folder, &folder_base, &folder_head, "is missing or not a plain file"),
        ("map is a symlink", &link, &link_base, &link_head, "is missing or not a plain file"),
        ("map over 1 MiB", &huge, &huge_base, &huge_head, "is over 1 MiB"),
    ];
    for (name, x, b, h, needle) in &maps {
        let mut a = args_of(x, &t, b, h, &[]);
        a.extend(over.clone());
        let o = run_raw(&a);
        assert_eq!((o.code, o.stdout.as_str()), (2, ""), "{name}: {}", o.stderr);
        assert_eq!(o.stderr, format!("harness: error: {MAP}{needle}\n"), "{name}");
        assert!(!log.exists(), "{name}: writes no log");
    }
    // Twins: with the good map of each head as --from, the same repo answers.
    for (name, x, _b, h, _) in &maps {
        let next = commit_all(&x.dir, &[("docs/b.md", "b\n")]);
        let o = run(x, &t, h, &next, &[]);
        assert_eq!(o.code, 0, "{name}: {}", o.stderr);
        assert_eq!(last(&o), format!("risk=low ticket=T-1 from={h} to={next} files=1 why=mapped-low"), "{name}");
    }
    // A changed path that is not UTF-8, and one that holds a control character (a newline), each planted
    // through git's own index (a file system may refuse such a name) in a repo of its own, so each is the
    // only reason its run is refused. Refused on either side of the diff, with a full raising override
    // too, and writes no log.
    for path_bytes in [&b"\xff.txt"[..], b"\n.txt"] {
        let x = repo();
        let b = commit_all(&x.dir, &[("gates/risk-map.txt", "docs low\n"), ("docs/a.md", "one\n")]);
        let blob = g_stdin(&x.dir, &["hash-object", "-w", "--stdin"], b"x");
        let mut entry = format!("100644 {blob}\t").into_bytes();
        entry.extend_from_slice(path_bytes);
        entry.push(0);
        g_stdin(&x.dir, &["update-index", "-z", "--index-info"], &entry);
        let h = commit(&x.dir);
        for (from, to) in [(&h, &b), (&b, &h)] {
            let mut a = args_of(&x, &t, from, to, &[]);
            a.extend(over.clone());
            let o = run_raw(&a);
            assert_eq!((o.code, o.stdout.as_str()), (2, ""), "{path_bytes:?}");
            assert_eq!(o.stderr, "harness: error: a changed path is not utf-8 or holds a control character\n", "{path_bytes:?}");
            assert!(!log.exists(), "{path_bytes:?}: writes no log");
        }
    }
}

/// The committed map `gates/risk-map.txt` classifies the paths the ticket names.
#[test]
fn risk_seed_map_classes() {
    let seed = String::from_utf8(std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gates/risk-map.txt")).expect("the seed map is committed")).expect("the seed map is utf-8");
    let low = ["docs/x.md", "README.md", "core/rust/harness/src/plan.rs"];
    let high = [
        (".github/workflows/x.yml", "mapped-high"),
        ("ci/release-build.sh", "mapped-high"),
        ("controls/registry.json", "mapped-high"),
        ("plugins/claude/x.sh", "mapped-high"),
        ("core/rust/harness/src/scan.rs", "mapped-high"),
        ("core/rust/harness/src/detect.rs", "mapped-high"),
        ("zz-new/x", "unmapped"),
    ];
    // Runs the verb with `map` as the base map and the named paths changed; returns the stdout lines.
    let classify_with = |map: &[u8]| {
        let r = repo();
        let b = commit_all(&r.dir, &[("gates/risk-map.txt", map)]);
        let files: Vec<(&str, &[u8])> = low.iter().map(|p| (*p, &b"x\n"[..])).chain(high.iter().map(|(p, _)| (*p, &b"x\n"[..]))).collect();
        let h = commit_all(&r.dir, &files);
        let o = run(&r, &ticket(&r, "T-1", "normal"), &b, &h, &[]);
        assert_eq!((o.code, o.stderr.as_str()), (0, ""), "{}", o.stderr);
        o.stdout.lines().map(str::to_string).collect::<Vec<String>>()
    };
    // The path lines by path, and the summary line.
    let split = |all: &[String]| {
        let got: BTreeMap<String, (String, String)> = all[..all.len() - 1]
            .iter()
            .map(|l| {
                let (path, rest) = l.strip_prefix("path=").unwrap().split_once(" class=").unwrap();
                let (class, why) = rest.split_once(" why=").unwrap();
                (path.to_string(), (class.to_string(), why.to_string()))
            })
            .collect();
        (got, all[all.len() - 1].clone())
    };
    let all = classify_with(seed.as_bytes());
    let (got, summary) = split(&all);
    assert_eq!(got.len(), low.len() + high.len(), "one line per changed path: {got:?}");
    let class = |c: &str, w: &str| Some((c.to_string(), w.to_string()));
    for p in low {
        assert_eq!(got.get(p).cloned(), class("low", "mapped-low"), "{p}");
    }
    for (p, why) in high {
        assert_eq!(got.get(p).cloned(), class("high", why), "{p}");
    }
    assert!(summary.starts_with("risk=high ticket=T-1 from=") && summary.ends_with(" files=10 why=mapped-high"), "the first high path in git order is under .github: {summary}");
    // A low-only diff with the seed map is low.
    let r = repo();
    let b = commit_all(&r.dir, &[("gates/risk-map.txt", seed.as_str()), ("README.md", "one\n"), ("core/rust/harness/src/plan.rs", "one\n")]);
    let h = commit_all(&r.dir, &[("README.md", "two\n"), ("core/rust/harness/src/plan.rs", "two\n"), ("docs/y.md", "y\n")]);
    let o = run(&r, &ticket(&r, "T-1", "normal"), &b, &h, &[]);
    assert_eq!(o.stdout, format!("path=README.md class=low why=mapped-low\npath=core/rust/harness/src/plan.rs class=low why=mapped-low\npath=docs/y.md class=low why=mapped-low\nrisk=low ticket=T-1 from={b} to={h} files=3 why=mapped-low\n"));
    // Planted twin: without the `docs low` entry the same docs path is unmapped, so the test sees the entry.
    assert!(seed.contains("\ndocs low\n"), "the seed maps docs low");
    let (got, _) = split(&classify_with(seed.replace("\ndocs low\n", "\n").as_bytes()));
    assert_eq!(got.get("docs/x.md").cloned(), class("high", "unmapped"));
    assert_eq!(got.get("README.md").cloned(), class("low", "mapped-low"), "the other low entries still answer");
    // The live check of the review, on a temp repo: a commit that changes only docs/install.md is low, one
    // that changes only gates/cargo-test.json is high with the gates-path line.
    let r = repo();
    let b = commit_all(&r.dir, &[("gates/risk-map.txt", seed.as_str()), ("docs/install.md", "one\n"), ("gates/cargo-test.json", "{}\n")]);
    let t = ticket(&r, "T-1", "normal");
    let docs = commit_all(&r.dir, &[("docs/install.md", "two\n")]);
    let o = run(&r, &t, &b, &docs, &[]);
    assert_eq!((o.code, o.stderr.as_str()), (0, ""));
    assert_eq!(o.stdout, format!("path=docs/install.md class=low why=mapped-low\nrisk=low ticket=T-1 from={b} to={docs} files=1 why=mapped-low\n"));
    let gate = commit_all(&r.dir, &[("docs/install.md", "one\n"), ("gates/cargo-test.json", "{\n}\n")]);
    let o = run(&r, &t, &b, &gate, &[]);
    assert_eq!(o.stdout, format!("path=gates/cargo-test.json class=high why=gates-path\nrisk=high ticket=T-1 from={b} to={gate} files=1 why=gates-path\n"));
}

/// `docs/risk.md` holds the contract: the usage line byte-equal, the map path, every why token, the line
/// shapes and the exit codes.
#[test]
fn risk_doc_holds_the_contract() {
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../docs/risk.md")).expect("the doc is committed");
    assert!(doc.lines().any(|l| l == USAGE), "the doc holds the usage line byte-equal");
    for needle in [
        "gates/risk-map.txt",
        "<repo-relative path> <low|high>",
        "--from",
        "risk-map",
        "gates-path",
        "unmapped",
        "mapped-high",
        "mapped-low",
        "override",
        "ticket-high",
        "path=<path> class=<low|high> why=<token>",
        "risk=<low|high> ticket=<id> from=<sha> to=<sha> files=<n> why=<token>",
        "risk-override ticket=<id> from=<sha> to=<sha> computed=<c> override=<o> by=<who> effect=<raised|same> reason=<text>",
        "harness: risk refused: override-lowers computed=high override=low",
        "0",
        "1",
        "2",
        "Not covered",
    ] {
        assert!(doc.contains(needle), "the doc names {needle}");
    }
    assert!(doc.contains("## Not covered"), "the doc has a Not covered section");
}
