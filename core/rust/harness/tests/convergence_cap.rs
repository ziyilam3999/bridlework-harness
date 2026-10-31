//! Process-level smoke for the convergence cap in `harness ticket gate` (P1A-09). Every test runs the real binary as a child process against
//! scratch repos under the system temp folder, from the folder of a DIFFERENT repo, with its own result folder and caps folder. Variables are set
//! only on the child. Every refusal has a planted failing twin: the same run with one thing changed that a wrong build gets wrong.
//!
//! Fixture repo `r`: `gates/t.json` (both ends, green only when `src/value.txt` holds `ok`), `gates/p.json` (head only, green when `docs/x.txt` is
//! non-empty), `gates/path-map.json` (`src` -> t, `docs` -> p). Every script appends one line `<name> <pwd>` to the file named by `GATE_MARKER`,
//! a file OUTSIDE `r`. "Failing ticket" = HEAD with `src/value.txt` = `bad2` over base `bad`, so every run FAILs with class `check-failed`.
#![cfg(unix)]
use harness::caps::Caps;
use harness::events;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("hcap-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        // A read-only folder must be writable again before the tree can go.
        if let Ok(rd) = fs::read_dir(&self.0) {
            for e in rd.flatten() {
                let _ = fs::set_permissions(e.path(), fs::Permissions::from_mode(0o755));
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?}", args);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn gate_json(id: &str, program: &str, both_ends: bool) -> String {
    format!("{{\"id\":\"{id}\",\"program\":\"{program}\",\"args\":[],\"timeout_ms\":20000,\"max_output_bytes\":4096,\"both_ends\":{both_ends}}}")
}

fn script(name: &str, test: &str) -> String {
    format!("#!/bin/sh\necho \"{name} $(pwd)\" >> \"$GATE_MARKER\"\n{test}\n")
}

struct Cfg {
    map: String,
    value0: String,
    files: Vec<(String, String, bool)>,
    links: Vec<(String, PathBuf)>,
}

impl Cfg {
    fn new() -> Cfg {
        Cfg { map: r#"{"src":["t"],"docs":["p"]}"#.to_string(), value0: "bad".to_string(), files: vec![], links: vec![] }
    }

    fn map(mut self, m: &str) -> Cfg {
        self.map = m.to_string();
        self
    }

    fn file(mut self, path: &str, body: &str, exec: bool) -> Cfg {
        self.files.push((path.to_string(), body.to_string(), exec));
        self
    }

    fn gate(self, id: &str, test: &str, both: bool) -> Cfg {
        let (g, p) = (format!("gates/{id}.json"), format!("ci/{id}.sh"));
        let body = gate_json(id, &p, both);
        self.file(&g, &body, false).file(&p, &script(id, test), true)
    }
}

struct Fx {
    t: Tmp,
    repo: PathBuf,
    other: PathBuf,
    marker: PathBuf,
    res: PathBuf,
    caps: PathBuf,
    c0: String,
}

fn write_file(path: &Path, body: &str, exec: bool) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
}

impl Fx {
    fn new() -> Fx {
        Fx::with(Cfg::new())
    }

    fn with(cfg: Cfg) -> Fx {
        let t = Tmp::new();
        let (repo, other, marker_dir, res, caps) = (t.0.join("r"), t.0.join("o"), t.0.join("marker"), t.0.join("res"), t.0.join("caps"));
        for d in [&repo, &other, &marker_dir, &res, &caps] {
            fs::create_dir_all(d).unwrap();
        }
        let mut files: Vec<(String, String, bool)> = vec![
            ("gates/t.json".to_string(), gate_json("t", "ci/t.sh", true), false),
            ("gates/p.json".to_string(), gate_json("p", "ci/p.sh", false), false),
            ("gates/path-map.json".to_string(), cfg.map.clone(), false),
            ("ci/t.sh".to_string(), script("t", "[ \"$(cat src/value.txt)\" = ok ]"), true),
            ("ci/p.sh".to_string(), script("p", "[ -s docs/x.txt ]"), true),
            ("src/value.txt".to_string(), cfg.value0.clone(), false),
            ("docs/x.txt".to_string(), "doc0".to_string(), false),
        ];
        files.extend(cfg.files.iter().cloned());
        git(&repo, &["init", "-q"]);
        for (p, b, x) in &files {
            write_file(&repo.join(p), b, *x);
        }
        for (p, target) in &cfg.links {
            fs::create_dir_all(repo.join(p).parent().unwrap()).unwrap();
            let _ = fs::remove_file(repo.join(p));
            symlink(target, repo.join(p)).unwrap();
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c0"]);
        git(&other, &["init", "-q"]);
        write_file(&other.join("o.txt"), "other repo", false);
        git(&other, &["add", "-A"]);
        git(&other, &["commit", "-q", "-m", "other"]);
        let c0 = git(&repo, &["rev-parse", "HEAD"]);
        Fx {
            repo: repo.canonicalize().unwrap(),
            other: other.canonicalize().unwrap(),
            marker: marker_dir.join("m.log"),
            res: res.canonicalize().unwrap(),
            caps: caps.canonicalize().unwrap(),
            c0,
            t,
        }
    }

    /// The fixture plus the failing ticket's commit (`bad2` over base `bad`); returns the fixture and the failing head.
    fn failing() -> (Fx, String) {
        let fx = Fx::new();
        let c1 = fx.commit(&[("src/value.txt", "bad2")]);
        (fx, c1)
    }

    fn commit(&self, files: &[(&str, &str)]) -> String {
        for (p, b) in files {
            write_file(&self.repo.join(p), b, false);
        }
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", "next"]);
        self.head()
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn markers(&self) -> Vec<String> {
        fs::read_to_string(&self.marker).map(|s| s.lines().map(|l| l.to_string()).collect()).unwrap_or_default()
    }

    fn ran(&self, name: &str) -> usize {
        self.markers().iter().filter(|l| l.starts_with(&format!("{name} "))).count()
    }

    fn result_file(&self, id: &str) -> PathBuf {
        self.res.join(format!("{id}.json"))
    }

    fn rec_file(&self, id: &str) -> PathBuf {
        self.caps.join(format!("{id}.rounds"))
    }

    /// The record's exact bytes as text.
    fn rec(&self, id: &str) -> String {
        fs::read_to_string(self.rec_file(id)).unwrap_or_else(|_| panic!("no record for {id}"))
    }

    fn plant(&self, id: &str, fix_rounds: u8, in_flight: bool) {
        fs::write(self.rec_file(id), canon(id, fix_rounds, in_flight)).unwrap();
    }

    /// The public reader's answer for a ticket of repo `r`.
    fn rounds(&self, id: &str) -> u8 {
        Caps::open(&self.caps, &self.repo).unwrap().rounds(id, "r").unwrap()
    }

    fn cant_write_readonly(&self, dir: &Path) -> bool {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(dir.join("probe"), "x").is_ok() {
            let _ = fs::remove_file(dir.join("probe"));
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
            println!("SKIP: this process can write into a mode-0555 directory, so the read-only arm cannot be exercised here");
            return false;
        }
        true
    }
}

fn canon(id: &str, fix_rounds: u8, in_flight: bool) -> String {
    format!("{{\"v\":1,\"ticket\":\"{id}\",\"repo_id\":\"r\",\"fix_rounds\":{fix_rounds},\"in_flight\":{in_flight}}}\n")
}

fn ticket_json(repo_id: &str, id: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"repo_id\":\"{repo_id}\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"M\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}"
    )
}

fn ticket_file(fx: &Fx, body: &str) -> PathBuf {
    let d = fx.t.0.join("tickets");
    fs::create_dir_all(&d).unwrap();
    let f = d.join(format!("t{}.json", SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::write(&f, body).unwrap();
    f
}

fn command(cwd: &Path, args: &[String], env: &[(&str, &Path)]) -> Command {
    let mut c = Command::new(BIN);
    c.arg("ticket").arg("gate").args(args).current_dir(cwd).stdin(Stdio::null());
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

fn run_args(cwd: &Path, args: &[String], env: &[(&str, &Path)]) -> Output {
    command(cwd, args, env).output().unwrap()
}

/// The complete command line without `--caps-dir`.
fn base_args(tf: &Path, repo: &Path, base: &str, res: &Path) -> Vec<String> {
    vec![tf.to_str().unwrap().into(), "--repo-map".into(), format!("r={}", repo.to_str().unwrap()), "--base".into(), base.into(), "--result-dir".into(), res.to_str().unwrap().into()]
}

fn full_args(fx: &Fx, id: &str) -> Vec<String> {
    let tf = ticket_file(fx, &ticket_json("r", id));
    let mut a = base_args(&tf, &fx.repo, &fx.c0, &fx.res);
    a.extend(["--caps-dir".to_string(), fx.caps.to_str().unwrap().to_string()]);
    a
}

fn env_of<'a>(fx: &'a Fx, extra: &[(&'a str, &'a Path)]) -> Vec<(&'a str, &'a Path)> {
    let mut env: Vec<(&str, &Path)> = vec![("GATE_MARKER", fx.marker.as_path())];
    env.extend_from_slice(extra);
    env
}

fn gate_id(fx: &Fx, id: &str, extra: &[(&str, &Path)]) -> Output {
    run_args(&fx.other, &full_args(fx, id), &env_of(fx, extra))
}

fn gate(fx: &Fx) -> Output {
    gate_id(fx, "T-1", &[])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or_else(|| panic!("the gate was killed by a signal: {}", err(o)))
}

fn leaks(o: &Output, needle: &str) -> bool {
    out(o).contains(needle) || err(o).contains(needle)
}

/// An error: exit 2, stdout empty, never `done=allowed`.
fn errored(o: &Output) {
    assert_eq!(code(o), 2, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty: {}", out(o));
}

fn names(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

fn listing(d: &Path) -> Vec<String> {
    fn walk(d: &Path, base: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            if p.is_dir() && !p.is_symlink() {
                walk(&p, base, out);
            }
        }
    }
    let mut v = Vec::new();
    walk(d, d, &mut v);
    v.sort();
    v
}

fn fail_line(head: &str) -> String {
    format!("ticket=T-1 head={head} result=fail done=refused\n")
}

fn refused_line(id: &str, head: &str) -> String {
    format!("ticket={id} head={head} result=refused done=refused reason=fix-round-cap\n")
}

const HINT: &str = "fix-round cap reached; the next run is refused and escalated";

fn event_lines(fx: &Fx) -> Vec<String> {
    fs::read_to_string(fx.caps.join("events.jsonl")).map(|s| s.lines().map(|l| l.to_string()).collect()).unwrap_or_default()
}

fn mover() -> String {
    format!("git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t{}scratch commit -q --allow-empty -m moved", '@')
}

// AC 1 + 2: runs 1-4 execute and record their round; run 5 is the 4th fix round and is refused. Twin of each: the other.
#[test]
fn runs_one_to_four_execute_and_run_five_is_refused() {
    let (fx, c1) = Fx::failing();
    let mut per_run = 0;
    for k in 1..=4u8 {
        let o = gate(&fx);
        assert_eq!(code(&o), 1, "run {k}: {}", err(&o));
        assert_eq!(out(&o), fail_line(&c1), "run {k}: stdout is P1A-07's fixed line");
        assert!(err(&o).contains("class=check-failed"), "run {k}: {}", err(&o));
        if k == 1 {
            per_run = fx.markers().len();
            assert!(per_run > 0, "the checks ran");
        }
        assert_eq!(fx.markers().len(), per_run * k as usize, "run {k} ran the checks once more");
        assert_eq!(fx.rec("T-1"), canon("T-1", k, false), "after run {k}");
        assert_eq!(err(&o).contains(HINT), k == 4, "run {k}: the hint is printed only when the cap is reached: {}", err(&o));
    }
    assert!(!fx.caps.join("events.jsonl").exists(), "no refusal yet, so no event");
    let (before_markers, before_result, before_rec) = (fx.markers(), fs::read(fx.result_file("T-1")).unwrap(), fx.rec("T-1"));
    let o = gate(&fx);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(out(&o), refused_line("T-1", &c1));
    assert!(!leaks(&o, "done=allowed"));
    assert!(err(&o).contains("gate REFUSED reason=fix-round-cap"), "{}", err(&o));
    assert_eq!(fx.markers(), before_markers, "nothing ran");
    assert_eq!(fs::read(fx.result_file("T-1")).unwrap(), before_result, "the result file is untouched");
    assert_eq!(fx.rec("T-1"), before_rec, "no round was paid for the refusal");
    assert!(!leaks(&o, fx.caps.to_str().unwrap()) && !leaks(&o, fx.res.to_str().unwrap()), "no folder is echoed");
    assert_eq!(event_lines(&fx).len(), 1, "exactly one escalation event");
}

// AC 3: the event is a valid escalation with the lowercased id; each refused call appends one. Twin: a ticket under the cap writes no log.
#[test]
fn the_refusal_writes_a_valid_escalation_event() {
    let (fx, c1) = Fx::failing();
    assert_eq!(code(&gate_id(&fx, "EX-1", &[])), 1);
    assert!(!fx.caps.join("events.jsonl").exists(), "twin: under the cap there is no event log");
    fx.plant("EX-1", 4, false);
    let o = gate_id(&fx, "EX-1", &[]);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(out(&o), refused_line("EX-1", &c1));
    let listed = names(&fx.caps);
    assert!(listed.contains(&"EX-1.rounds".to_string()) && !listed.contains(&"ex-1.rounds".to_string()), "the record keeps the exact-case name: {listed:?}");
    let log = fx.caps.join("events.jsonl");
    let v = Command::new(BIN).args(["events", "verify", "--log", log.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&v), 0, "{}", err(&v));
    let l = Command::new(BIN).args(["events", "list", "--log", log.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&l), 0);
    for needle in ["escalation", "ex-1", "fix-round-cap"] {
        assert!(out(&l).contains(needle), "events list lacks `{needle}`: {}", out(&l));
    }
    let read = events::read_log(&log).unwrap();
    assert!(read.is_clean());
    assert_eq!(read.events.len(), 1);
    let e = &read.events[0];
    assert_eq!((e.kind.as_str(), e.ticket.as_str(), e.repo.as_str(), e.reason.as_str()), ("escalation", "ex-1", "r", "fix-round-cap"));
    assert!(e.ts_ms > 0);
    assert_eq!(code(&gate_id(&fx, "EX-1", &[])), 1);
    assert_eq!(event_lines(&fx).len(), 2, "a second refused call appends a second event");
}

// AC 4: the count is per ticket. Twin: the capped ticket itself.
#[test]
fn the_count_is_per_ticket() {
    let (fx, c1) = Fx::failing();
    for _ in 0..2 {
        assert_eq!(code(&gate_id(&fx, "T-1", &[])), 1);
    }
    for _ in 0..4 {
        assert_eq!(code(&gate_id(&fx, "T-2", &[])), 1);
    }
    let first = fx.rec("T-1");
    assert_eq!(first, canon("T-1", 2, false));
    let o = gate_id(&fx, "T-2", &[]);
    assert_eq!(out(&o), refused_line("T-2", &c1), "twin: T-2 is capped");
    assert_eq!(fx.rec("T-1"), first, "T-2's refusal did not touch T-1's record");
    let before = fx.markers().len();
    let o = gate_id(&fx, "T-1", &[]);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(out(&o), fail_line(&c1), "T-1 at two rounds still runs");
    assert!(fx.markers().len() > before, "its checks ran");
    assert_eq!(fx.rec("T-1"), canon("T-1", 3, false));
    assert_eq!(event_lines(&fx).len(), 1, "only T-2's refusal is an event");
}

// AC 5 + 22: a PASS keeps the count; the public reader says 2, 3, 0. Twin: a FAIL keeps its round.
#[test]
fn a_pass_keeps_the_count_and_the_public_reader_agrees() {
    let (fx, _) = Fx::failing();
    assert_eq!(fx.rounds("T-1"), 0, "no run yet");
    assert!(!fx.rec_file("T-1").exists(), "and no record file");
    assert_eq!(code(&gate(&fx)), 1);
    assert_eq!(fx.rounds("T-1"), 1, "one FAIL run");
    assert_eq!(code(&gate(&fx)), 1);
    fx.commit(&[("src/value.txt", "ok")]);
    let o = gate(&fx);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("done=allowed"));
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false), "the PASS refunded its own round and nothing else");
    assert_eq!(fx.rounds("T-1"), 2);
    fx.commit(&[("src/value.txt", "bad3")]);
    assert_eq!(code(&gate(&fx)), 1);
    assert_eq!(fx.rec("T-1"), canon("T-1", 3, false));
    assert_eq!(fx.rounds("T-1"), 3);
    // FAIL then PASS is still 1; a first run that PASSes is 0.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2")]);
    assert_eq!(code(&gate(&fx)), 1);
    fx.commit(&[("src/value.txt", "ok")]);
    assert_eq!(code(&gate(&fx)), 0);
    assert_eq!(fx.rounds("T-1"), 1);
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    assert_eq!(code(&gate(&fx)), 0);
    assert_eq!(fx.rec("T-1"), canon("T-1", 0, false));
    assert_eq!(fx.rounds("T-1"), 0);
}

// AC 6: errors BEFORE the pay point cost nothing and leave no record. Twin: AC 7 (an error after it costs a round).
#[test]
fn errors_before_the_pay_point_cost_nothing() {
    // A dirty repo folder.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    write_file(&fx.repo.join("stray.txt"), "u", false);
    errored(&gate(&fx));
    assert!(fx.markers().is_empty() && !fx.rec_file("T-1").exists(), "dirty folder: nothing ran, no record");
    // A ticket refusal (exit 1).
    let tf = ticket_file(&fx, &ticket_json("q", "T-1"));
    let mut a = base_args(&tf, &fx.repo, &fx.c0, &fx.res);
    a.extend(["--caps-dir".to_string(), fx.caps.to_str().unwrap().to_string()]);
    assert_eq!(code(&run_args(&fx.other, &a, &env_of(&fx, &[]))), 1);
    assert!(!fx.rec_file("T-1").exists());
    // An unloadable program of a HEAD-ONLY gate (a link to a script outside the repo), as the merged `z` case.
    let fx0 = Fx::new();
    let outside = fx0.t.0.join("outside");
    fs::create_dir_all(&outside).unwrap();
    write_file(&outside.join("z.sh"), &script("zout", "true"), true);
    let map = r#"{"src":["t"],"docs":["p","z"]}"#;
    let cfg = Cfg { links: vec![("ci/z.sh".to_string(), outside.join("z.sh"))], ..Cfg::new().map(map) }.file("gates/z.json", &gate_json("z", "ci/z.sh", false), false);
    let fx = Fx::with(cfg);
    fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "doc1")]);
    errored(&gate(&fx));
    assert!(fx.markers().is_empty() && !fx.rec_file("T-1").exists(), "unloadable head-only program: nothing ran, no record");
    // A stale result temp.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::write(fx.res.join(".T-1.json.tmp"), "stale").unwrap();
    errored(&gate(&fx));
    assert_eq!(fs::read_to_string(fx.res.join(".T-1.json.tmp")).unwrap(), "stale");
    assert!(fx.markers().is_empty() && !fx.rec_file("T-1").exists(), "stale temp: refused before a round is paid");
    // A busy lock.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    let held = fs::File::create(fx.caps.join(".T-1.lock")).unwrap();
    held.lock().unwrap();
    let o = gate(&fx);
    errored(&o);
    assert!(err(&o).contains("in progress"), "{}", err(&o));
    assert!(fx.markers().is_empty() && !fx.rec_file("T-1").exists(), "busy lock: nothing ran, no record");
    drop(held);
    assert_eq!(code(&gate(&fx)), 0, "twin: the lock is free again");
    // A never-run ticket reads 0 and has no file.
    assert_eq!(fx.rounds("T-9"), 0);
    assert!(!fx.rec_file("T-9").exists());
}

// AC 7: errors AFTER the pay point cost the round (in_flight stays true) and the next run folds it in first. Twin: AC 6.
#[test]
fn errors_after_the_pay_point_cost_a_round() {
    // A head-only check that moves HEAD once, then fails.
    let once_dir = Tmp::new();
    let once = once_dir.0.join("once");
    let test = format!("[ -e \"$ONCE\" ] || {{ : > \"$ONCE\"; {}; }}\n[ \"$(cat docs/x.txt)\" = ok ]", mover());
    let fx = Fx::with(Cfg::new().map(r#"{"src":["t"],"docs":["m"]}"#).gate("m", &test, false));
    fx.commit(&[("docs/x.txt", "doc1")]);
    let before = fx.head();
    errored(&gate_id(&fx, "T-1", &[("ONCE", once.as_path())]));
    assert_ne!(fx.head(), before, "the check really moved HEAD");
    assert_eq!(fx.rec("T-1"), canon("T-1", 0, true), "the paid round stays");
    assert_eq!(fx.rounds("T-1"), 1, "a reader counts the run in flight");
    let o = gate_id(&fx, "T-1", &[("ONCE", once.as_path())]);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false), "the dead run was folded in BEFORE this run's verdict");
    // A both-ends check that dirties the repo folder, then a clean PASS: the dead run is still counted.
    let dirtier = "[ -z \"$GATE_REPO\" ] || echo x > \"$GATE_REPO/stray.txt\"\n[ \"$(cat src/value.txt)\" = ok ]";
    let fx = Fx::with(Cfg::new().gate("t", dirtier, true));
    fx.commit(&[("src/value.txt", "ok")]);
    let repo = fx.repo.clone();
    errored(&gate_id(&fx, "T-1", &[("GATE_REPO", repo.as_path())]));
    assert!(fx.repo.join("stray.txt").exists());
    assert_eq!(fx.rec("T-1"), canon("T-1", 0, true));
    fs::remove_file(fx.repo.join("stray.txt")).unwrap();
    assert_eq!(code(&gate(&fx)), 0);
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, false), "a PASS refunds only its own round");
    // A both-ends gate whose program is MISSING at HEAD is found in the run loop: exit 2, round paid.
    let cfg = Cfg::new().file("gates/t.json", &gate_json("t", "ci/missing.sh", true), false);
    let fx = Fx::with(cfg);
    fx.commit(&[("src/value.txt", "ok")]);
    errored(&gate(&fx));
    assert_eq!(fx.rec("T-1"), canon("T-1", 0, true), "the missing both-ends program cost a round");
    // A check that kills the gate process (SIGKILL) leaves the run in flight.
    let kill_dir = Tmp::new();
    let once = kill_dir.0.join("once");
    let test = "if [ ! -e \"$ONCE\" ]; then : > \"$ONCE\"; kill -9 $PPID; sleep 5; fi\nfalse";
    let fx = Fx::with(Cfg::new().map(r#"{"src":["t"],"docs":["k"]}"#).gate("k", test, false));
    fx.commit(&[("docs/x.txt", "doc1")]);
    let o = gate_id(&fx, "T-1", &[("ONCE", once.as_path())]);
    assert!(o.status.code().is_none(), "the gate process was killed: {:?}", o.status);
    assert_eq!(fx.rec("T-1"), canon("T-1", 0, true));
    let o = gate_id(&fx, "T-1", &[("ONCE", once.as_path())]);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false), "the dead run is folded first");
}

// AC 8: verdicts that run no check still cost a round. Twin: a check-failed FAIL (AC 1).
#[test]
fn verdicts_that_run_no_check_count_as_rounds() {
    let fx = Fx::new();
    fx.commit(&[("other/x.txt", "new")]);
    let o = gate(&fx);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("class=unmapped-path"));
    assert!(fx.markers().is_empty());
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, false));
    let fx = Fx::new();
    git(&fx.repo, &["commit", "-q", "--allow-empty", "-m", "empty"]);
    let o = gate(&fx);
    assert_eq!(code(&o), 1);
    assert_eq!(fs::read_to_string(fx.result_file("T-1")).unwrap().contains("no-checks"), true);
    assert!(fx.markers().is_empty());
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, false));
}

// AC 9: a record write failure never allows a run or a PASS. Twin: the same runs in a writable folder.
#[test]
fn a_record_write_failure_never_allows_a_run() {
    // The pay write fails: record absent, then record at 3 rounds.
    for planted in [None, Some(3u8)] {
        let (fx, _) = Fx::failing();
        // The lock file exists before the chmod, so the run reaches the pay write and not a lock-creation error.
        fs::write(fx.caps.join(".T-1.lock"), "").unwrap();
        if let Some(n) = planted {
            fx.plant("T-1", n, false);
        }
        let before = fx.rec_file("T-1").exists().then(|| fx.rec("T-1"));
        if !fx.cant_write_readonly(&fx.caps) {
            return;
        }
        let o = gate(&fx);
        fs::set_permissions(&fx.caps, fs::Permissions::from_mode(0o755)).unwrap();
        errored(&o);
        assert!(!leaks(&o, "done=allowed") && !leaks(&o, fx.caps.to_str().unwrap()));
        assert!(fx.markers().is_empty(), "nothing ran");
        assert!(!fx.result_file("T-1").exists(), "no result file");
        assert_eq!(fx.rec_file("T-1").exists().then(|| fx.rec("T-1")), before, "the record is as it was");
        assert!(fx.caps.join(".T-1.lock").exists(), "the lock file is never deleted");
        assert_eq!(code(&gate(&fx)), 1, "twin: a writable folder runs");
    }
    // A check that makes the caps folder read-only mid-run.
    let readonly = "chmod 555 \"$GATE_CAPS\"";
    for pass in [false, true] {
        let fx = Fx::with(Cfg::new().map(r#"{"src":["t"],"docs":["r"]}"#).gate("r", &format!("{readonly}\n{}", if pass { "true" } else { "false" }), false));
        fx.commit(&[("docs/x.txt", "doc1")]);
        let caps = fx.caps.clone();
        if !fx.cant_write_readonly(&fx.caps) {
            return;
        }
        fs::set_permissions(&fx.caps, fs::Permissions::from_mode(0o755)).unwrap();
        let o = gate_id(&fx, "T-1", &[("GATE_CAPS", caps.as_path())]);
        fs::set_permissions(&fx.caps, fs::Permissions::from_mode(0o755)).unwrap();
        errored(&o);
        assert!(!leaks(&o, "done=allowed"));
        assert_eq!(fx.rec("T-1"), canon("T-1", 0, true), "{}: the paid round stays and nothing was refunded or added", if pass { "PASS" } else { "FAIL" });
        assert!(!fx.result_file("T-1").exists(), "no result file was written");
    }
}

// AC 10: a link is never followed, read, created through or appended through. Twin: the same names as regular files.
#[test]
fn links_are_never_followed() {
    let keep = "keep me";
    let setup = || {
        let (fx, _) = Fx::failing();
        let outside = fx.t.0.join("outside.txt");
        fs::write(&outside, keep).unwrap();
        (fx, outside)
    };
    // The record is a link to a valid record elsewhere; or dangling.
    let (fx, outside) = setup();
    fs::write(&outside, canon("T-1", 0, false)).unwrap();
    symlink(&outside, fx.rec_file("T-1")).unwrap();
    let o = gate(&fx);
    errored(&o);
    assert!(fx.markers().is_empty() && !leaks(&o, fx.caps.to_str().unwrap()));
    assert_eq!(fs::read_to_string(&outside).unwrap(), canon("T-1", 0, false), "the target is untouched");
    let (fx, _) = setup();
    let ghost = fx.t.0.join("ghost.txt");
    symlink(&ghost, fx.rec_file("T-1")).unwrap();
    errored(&gate(&fx));
    assert!(!ghost.exists(), "nothing was created through the dangling link");
    assert!(fx.markers().is_empty());
    // The lock is a link.
    let (fx, outside) = setup();
    symlink(&outside, fx.caps.join(".T-1.lock")).unwrap();
    errored(&gate(&fx));
    assert!(fx.markers().is_empty());
    assert_eq!(fs::read_to_string(&outside).unwrap(), keep);
    // The record temp is a link: never written through (either it works around it or it refuses before paying).
    let (fx, outside) = setup();
    symlink(&outside, fx.caps.join(".T-1.rounds.tmp")).unwrap();
    let o = gate(&fx);
    assert!([1, 2].contains(&code(&o)), "{}", err(&o));
    assert_eq!(fs::read_to_string(&outside).unwrap(), keep, "the temp link was not written through");
    // events.jsonl is a link out, on a capped ticket.
    let (fx, outside) = setup();
    fx.plant("T-1", 4, false);
    symlink(&outside, fx.caps.join("events.jsonl")).unwrap();
    let o = gate(&fx);
    errored(&o);
    assert!(!leaks(&o, fx.caps.to_str().unwrap()));
    assert_eq!(fs::read_to_string(&outside).unwrap(), keep);
    assert!(fx.markers().is_empty());
    // events.jsonl is a link INSIDE the caps folder: onto this record, and onto another ticket's record.
    for onto in ["T-1.rounds", "T-2.rounds"] {
        let (fx, _) = setup();
        fx.plant("T-1", 4, false);
        fx.plant("T-2", 2, false);
        symlink(fx.caps.join(onto), fx.caps.join("events.jsonl")).unwrap();
        let o = gate(&fx);
        errored(&o);
        assert_eq!(fx.rec("T-1"), canon("T-1", 4, false), "{onto}: no event line landed on a record");
        assert_eq!(fx.rec("T-2"), canon("T-2", 2, false), "{onto}");
        assert!(fx.markers().is_empty());
    }
    // Twin: the same capped ticket with a regular events.jsonl is refused normally with one event.
    let (fx, _) = setup();
    fx.plant("T-1", 4, false);
    fs::write(fx.caps.join("events.jsonl"), "").unwrap();
    assert_eq!(code(&gate(&fx)), 1);
    assert_eq!(event_lines(&fx).len(), 1);
}

// AC 11: --caps-dir is refused unless absolute, real, outside the repo. Twin: such a folder.
#[test]
fn a_caps_dir_that_is_not_a_real_outside_directory_is_refused() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::create_dir_all(fx.other.join("caps-rel")).unwrap();
    fs::create_dir_all(fx.repo.join("out")).unwrap();
    fs::create_dir_all(fx.repo.join("out2")).unwrap();
    let link = fx.t.0.join("link-caps");
    symlink(&fx.caps, &link).unwrap();
    let into_repo = fx.t.0.join("lnk");
    symlink(&fx.repo, &into_repo).unwrap();
    let afile = fx.t.0.join("afile");
    fs::write(&afile, "x").unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("relative", "caps-rel".to_string()),
        ("dot-dot", format!("{}/../caps", fx.caps.display())),
        ("symlink to a real directory", link.display().to_string()),
        ("inside the repo", fx.repo.join("out").display().to_string()),
        ("parent component is a link into the repo", into_repo.join("out2").display().to_string()),
        ("missing", fx.t.0.join("nope").display().to_string()),
        ("a regular file", afile.display().to_string()),
    ];
    let run_with = |caps: Option<&[&str]>| {
        let tf = ticket_file(&fx, &ticket_json("r", "T-1"));
        let mut a = base_args(&tf, &fx.repo, &fx.c0, &fx.res);
        for c in caps.unwrap_or(&[]) {
            a.extend(["--caps-dir".to_string(), c.to_string()]);
        }
        run_args(&fx.other, &a, &env_of(&fx, &[]))
    };
    let mut all: Vec<(String, Output, Option<String>)> = cases.iter().map(|(l, d)| (l.to_string(), run_with(Some(&[d.as_str()][..])), Some(d.clone()))).collect();
    all.push(("absent option".to_string(), run_with(None), None));
    let cp = fx.caps.to_str().unwrap().to_string();
    all.push(("given twice".to_string(), run_with(Some(&[cp.as_str(), cp.as_str()][..])), None));
    for (label, o, dir) in &all {
        errored(o);
        if let Some(d) = dir {
            assert!(!leaks(o, d), "{label}: the folder must not be echoed: {}", err(o));
        }
        assert!(fx.markers().is_empty(), "{label}: nothing ran");
        assert!(names(&fx.res).is_empty() && names(&fx.caps).is_empty(), "{label}: nothing written");
        assert!(listing(&fx.repo.join("out")).is_empty() && listing(&fx.repo.join("out2")).is_empty(), "{label}: nothing written into the repo");
    }
    let o = run_with(Some(&[cp.as_str()][..]));
    assert_eq!(code(&o), 0, "twin: an absolute real folder outside the repo; stderr: {}", err(&o));
}

// AC 12: a strict record. Twin: the canonical record.
#[test]
fn a_record_that_is_not_canonical_is_refused_never_read_as_zero() {
    let (fx, _) = Fx::failing();
    let good = canon("T-1", 1, false);
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("not JSON", b"nope\n".to_vec()),
        ("empty", vec![]),
        ("extra key", good.replace("\"in_flight\"", "\"x\":1,\"in_flight\"").into_bytes()),
        ("missing key", good.replace("\"v\":1,", "").into_bytes()),
        ("reordered", "{\"v\":1,\"repo_id\":\"r\",\"ticket\":\"T-1\",\"fix_rounds\":1,\"in_flight\":false}\n".as_bytes().to_vec()),
        ("fix_rounds 5", good.replace(":1,\"in", ":5,\"in").into_bytes()),
        ("fix_rounds -1", good.replace(":1,\"in", ":-1,\"in").into_bytes()),
        ("fix_rounds 1.0", good.replace(":1,\"in", ":1.0,\"in").into_bytes()),
        ("fix_rounds string", good.replace(":1,\"in", ":\"1\",\"in").into_bytes()),
        ("fix_rounds 01", good.replace(":1,\"in", ":01,\"in").into_bytes()),
        ("fix_rounds u64 max", canon("T-1", 0, true).replace(":0,\"in", ":18446744073709551615,\"in").into_bytes()),
        ("fix_rounds 4 in flight", canon("T-1", 4, true).into_bytes()),
        ("in_flight not a bool", good.replace("false", "0").into_bytes()),
        ("v 2", good.replace("\"v\":1", "\"v\":2").into_bytes()),
        ("trailing bytes", format!("{}x", good).into_bytes()),
        ("second line", format!("{good}{good}").into_bytes()),
        ("NUL byte", [good.as_bytes(), b"\0"].concat()),
        ("non-UTF-8", vec![0xff, 0xfe, 0xfd]),
        ("no newline", good.trim_end().as_bytes().to_vec()),
    ];
    for (label, bytes) in &bad {
        fs::write(fx.rec_file("T-1"), bytes).unwrap();
        let o = gate(&fx);
        errored(&o);
        assert_eq!(&fs::read(fx.rec_file("T-1")).unwrap(), bytes, "{label}: the record bytes are unchanged");
        assert!(fx.markers().is_empty(), "{label}: nothing ran");
        assert!(!fx.result_file("T-1").exists(), "{label}: a corrupt record is not read as 0 rounds");
    }
    fs::write(fx.rec_file("T-1"), &good).unwrap();
    assert_eq!(code(&gate(&fx)), 1, "twin: the canonical record runs");
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false));
}

// AC 14: a record for another ticket or repo is refused. Twin: the matching record.
#[test]
fn a_record_for_another_ticket_or_repo_is_refused() {
    let (fx, _) = Fx::failing();
    fs::write(fx.rec_file("T-2"), canon("T-1", 1, false)).unwrap();
    errored(&gate_id(&fx, "T-2", &[]));
    fs::write(fx.rec_file("T-1"), canon("T-1", 1, false).replace("\"r\"", "\"q\"")).unwrap();
    errored(&gate(&fx));
    assert!(fx.markers().is_empty(), "nothing ran");
    fs::write(fx.rec_file("T-1"), canon("T-1", 1, false)).unwrap();
    assert_eq!(code(&gate(&fx)), 1, "twin: the matching record runs");
}

// AC 15: a result without its record is a deleted record. Twin: neither present, or both.
#[test]
fn a_result_file_without_its_record_is_refused() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::write(fx.result_file("T-1"), "old").unwrap();
    errored(&gate(&fx));
    assert!(fx.markers().is_empty() && !fx.rec_file("T-1").exists(), "nothing ran, no record created");
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    assert_eq!(code(&gate(&fx)), 0, "twin: neither present is a normal first run");
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::write(fx.result_file("T-1"), "old").unwrap();
    fx.plant("T-1", 0, false);
    assert_eq!(code(&gate(&fx)), 0, "twin: the result file with a canonical record proceeds");
}

// AC 16: a case twin of the record name is a collision, by name. Twin: the exact name is replaced.
#[test]
fn a_case_variant_of_the_record_name_is_refused() {
    let (fx, _) = Fx::failing();
    fs::write(fx.caps.join("ex-1.rounds"), "keep me").unwrap();
    let before = names(&fx.caps);
    errored(&gate_id(&fx, "EX-1", &[]));
    assert_eq!(fs::read_to_string(fx.caps.join("ex-1.rounds")).unwrap(), "keep me");
    assert_eq!(names(&fx.caps), before, "no new file");
    assert!(fx.markers().is_empty());
    let (fx, _) = Fx::failing();
    fx.plant("EX-1", 0, false);
    assert_eq!(code(&gate_id(&fx, "EX-1", &[])), 1, "twin: the exact name is replaced normally");
    assert_eq!(fx.rec("EX-1"), canon("EX-1", 1, false));
}

// AC 17: names come only from the validated id. Twin: ids that look alike but are valid.
#[test]
fn names_come_only_from_the_validated_id() {
    let (fx, _) = Fx::failing();
    let before: Vec<String> = listing(&fx.t.0).into_iter().filter(|p| !p.starts_with("tickets")).collect();
    for bad in ["../escape", "a/b", ".hidden", &"a".repeat(65)] {
        errored(&gate_id(&fx, bad, &[]));
        let after: Vec<String> = listing(&fx.t.0).into_iter().filter(|p| !p.starts_with("tickets")).collect();
        assert_eq!(after, before, "{bad}: nothing created anywhere under the temp root, the caps folder included");
    }
    assert_eq!(code(&gate_id(&fx, "EX-1.v2", &[])), 1);
    assert!(fx.caps.join("EX-1.v2.rounds").is_file(), "twin: EX-1.v2 writes EX-1.v2.rounds");
    assert_eq!(code(&gate_id(&fx, "a", &[])), 1);
    assert_eq!(code(&gate_id(&fx, "a.rounds", &[])), 1);
    assert_eq!(fx.rec("a"), canon("a", 1, false));
    assert_eq!(fx.rec("a.rounds"), canon("a.rounds", 1, false));
    assert!(fx.caps.join("a.rounds").is_file() && fx.caps.join("a.rounds.rounds").is_file());
    assert!(fx.result_file("a").is_file() && fx.result_file("a.rounds").is_file(), "results a.json and a.rounds.json");
    assert_eq!(code(&gate_id(&fx, "a", &[])), 1);
    assert_eq!(fx.rec("a"), canon("a", 2, false), "a's own record, not a.rounds's");
    assert_eq!(fx.rec("a.rounds"), canon("a.rounds", 1, false));
}

// AC 18: a second run for one ticket exits 2 while the first is held open; another ticket runs at the same time. Twin: two sequential runs.
#[test]
fn a_second_run_for_a_ticket_in_progress_is_refused() {
    let hold = Tmp::new();
    let (release, started) = (hold.0.join("release"), hold.0.join("started"));
    let test = "while [ ! -e \"$RELEASE\" ]; do sleep 0.05; done\nfalse";
    let fx = Fx::with(Cfg::new().map(r#"{"src":["t"],"docs":["w"]}"#).gate("w", &format!(": > \"$STARTED\"\n{test}"), false));
    fx.commit(&[("docs/x.txt", "doc1")]);
    let wait_for = |cond: &dyn Fn() -> bool| {
        let t0 = Instant::now();
        while !cond() {
            assert!(t0.elapsed() < Duration::from_secs(30), "timed out waiting for the held-open check");
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let env = |fx: &Fx| -> Vec<(String, PathBuf)> { vec![("GATE_MARKER".into(), fx.marker.clone()), ("RELEASE".into(), release.clone()), ("STARTED".into(), started.clone())] };
    let spawn = |id: &str| -> Child {
        let mut c = command(&fx.other, &full_args(&fx, id), &[]);
        for (k, v) in env(&fx) {
            c.env(k, v);
        }
        c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()
    };
    let first = spawn("T-1");
    wait_for(&|| started.exists() && fx.ran("w") >= 1);
    let second_ticket = spawn("T-2");
    wait_for(&|| fx.ran("w") >= 2);
    let o = run_args(&fx.other, &full_args(&fx, "T-1"), &[("GATE_MARKER", fx.marker.as_path()), ("RELEASE", release.as_path()), ("STARTED", started.as_path())]);
    errored(&o);
    assert_eq!(fx.ran("w"), 2, "the refused run's check never ran (T-1 once, T-2 once)");
    fs::write(&release, "").unwrap();
    for (label, child) in [("T-1", first), ("T-2", second_ticket)] {
        let o = child.wait_with_output().unwrap();
        assert_eq!(o.status.code(), Some(1), "{label}: {}", String::from_utf8_lossy(&o.stderr));
    }
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, false), "exactly ONE round for the first run");
    assert_eq!(fx.rec("T-2"), canon("T-2", 1, false), "the other ticket ran at the same time and finished normally");
    // Twin: two sequential runs are two rounds.
    assert_eq!(code(&run_args(&fx.other, &full_args(&fx, "T-1"), &[("GATE_MARKER", fx.marker.as_path()), ("RELEASE", release.as_path()), ("STARTED", started.as_path())])), 1);
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false));
}

// AC 19: the escalation cannot be written, so the run is an error, not a silent refusal. Twin: a healthy log.
#[test]
fn an_escalation_that_cannot_be_written_is_an_error() {
    let torn = "{\"v\":1,\"kind\":\"escalation\"";
    for unwritable in [false, true] {
        let (fx, _) = Fx::failing();
        fx.plant("T-1", 4, false);
        fs::write(fx.result_file("T-1"), "old").unwrap();
        let log = fx.caps.join("events.jsonl");
        if unwritable {
            fs::write(&log, "").unwrap();
            fs::set_permissions(&log, fs::Permissions::from_mode(0o444)).unwrap();
            if fs::OpenOptions::new().append(true).open(&log).is_ok() {
                println!("SKIP: this process can append to a mode-0444 file");
                continue;
            }
        } else {
            fs::write(&log, torn).unwrap();
        }
        let o = gate(&fx);
        errored(&o);
        assert!(err(&o).contains("ESCALATION NOT RECORDED") && err(&o).contains("fix-round-cap"), "{}", err(&o));
        assert!(fx.markers().is_empty(), "nothing ran");
        assert_eq!(fx.rec("T-1"), canon("T-1", 4, false));
        assert_eq!(fs::read_to_string(fx.result_file("T-1")).unwrap(), "old");
        assert!(!leaks(&o, fx.caps.to_str().unwrap()));
    }
    let (fx, c1) = Fx::failing();
    fx.plant("T-1", 4, false);
    let o = gate(&fx);
    assert_eq!((code(&o), out(&o)), (1, refused_line("T-1", &c1)), "twin: a healthy log");
    assert_eq!(event_lines(&fx).len(), 1);
}

// AC 25: the PASS refund is written before the result; a FAIL is counted before it. Twin: a writable result folder.
#[test]
fn the_record_is_written_before_the_result() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    if !fx.cant_write_readonly(&fx.res) {
        return;
    }
    let o = gate(&fx);
    fs::set_permissions(&fx.res, fs::Permissions::from_mode(0o755)).unwrap();
    errored(&o);
    assert!(!leaks(&o, "done=allowed"));
    assert!(!fx.result_file("T-1").exists(), "no PASS file");
    assert_eq!(fx.rec("T-1"), canon("T-1", 0, false), "the refund stands");
    let (fx, _) = Fx::failing();
    fx.cant_write_readonly(&fx.res);
    let o = gate(&fx);
    fs::set_permissions(&fx.res, fs::Permissions::from_mode(0o755)).unwrap();
    errored(&o);
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, false), "a FAIL whose result cannot be written is still counted");
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    assert_eq!(code(&gate(&fx)), 0, "twin: a writable result folder");
}

// AC 20 (the part a test can check): this ticket's code writes only the result file into the result folder.
#[test]
fn the_result_folder_holds_only_the_result_file() {
    let (fx, _) = Fx::failing();
    for _ in 0..5 {
        gate(&fx);
    }
    assert_eq!(names(&fx.res), vec!["T-1.json".to_string()]);
    assert!(names(&fx.caps).contains(&"T-1.rounds".to_string()) && names(&fx.caps).contains(&".T-1.lock".to_string()));
}
