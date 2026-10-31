//! Process-level smoke for the re-plan threshold in `harness ticket gate` (P1A-10). Every test runs the real binary as a child process against
//! scratch repos under the system temp folder, from the folder of a DIFFERENT repo, with its own result folder and caps folder. Variables are set
//! only on the child. Every guard has a planted failing twin: the same run with one thing changed that a wrong build gets wrong.
//!
//! Fixture repo `r`: `gates/t.json` (both ends, green only when `src/value.txt` holds `ok`), `gates/p.json` (head only, green when `docs/x.txt` is
//! non-empty), `gates/path-map.json` (`src` -> t, `docs` -> p unless a test says otherwise). Every script appends one line `<name> <pwd>` to the
//! file named by `GATE_MARKER`, a file OUTSIDE `r`. "Failing ticket" = HEAD with `src/value.txt` = `bad2` over base `bad`, so every run FAILs with
//! gate `t`, class `check-failed`. "New head" = a commit that changes `src/value.txt` to a fresh non-`ok` value.
//!
//! The signature is `<gate id>:<failure class>` only. It is deliberately coarse (see the comment on `the_signature_ignores_the_message`).
#![cfg(unix)]
use harness::events;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

const HINT: &str = "re-plan threshold reached";
const CAP_HINT: &str = "fix-round cap reached";
const SIG_T: &str = "t:check-failed";

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("hrp-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

fn gate_json(id: &str, program: &str, both_ends: bool, timeout_ms: u64) -> String {
    format!("{{\"id\":\"{id}\",\"program\":\"{program}\",\"args\":[],\"timeout_ms\":{timeout_ms},\"max_output_bytes\":4096,\"both_ends\":{both_ends}}}")
}

fn script(name: &str, test: &str) -> String {
    format!("#!/bin/sh\necho \"{name} $(pwd)\" >> \"$GATE_MARKER\"\n{test}\n")
}

type File3 = (String, String, bool);

/// The two files of one extra gate (its definition and its script).
fn gate_files(id: &str, test: &str, both: bool, timeout_ms: u64) -> Vec<File3> {
    let (g, p) = (format!("gates/{id}.json"), format!("ci/{id}.sh"));
    vec![(g, gate_json(id, &p, both, timeout_ms), false), (p, script(id, test), true)]
}

const MAP_T: &str = r#"{"src":["t"],"docs":["p"]}"#;
const U_TEST: &str = "[ \"$(cat src/other.txt)\" = ok ]";

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
        Fx::with(MAP_T, vec![])
    }

    fn with(map: &str, extra: Vec<File3>) -> Fx {
        let t = Tmp::new();
        let (repo, other, marker_dir, res, caps) = (t.0.join("r"), t.0.join("o"), t.0.join("marker"), t.0.join("res"), t.0.join("caps"));
        for d in [&repo, &other, &marker_dir, &res, &caps] {
            fs::create_dir_all(d).unwrap();
        }
        let mut files: Vec<File3> = vec![
            ("gates/t.json".to_string(), gate_json("t", "ci/t.sh", true, 20000), false),
            ("gates/p.json".to_string(), gate_json("p", "ci/p.sh", false, 20000), false),
            ("gates/path-map.json".to_string(), map.to_string(), false),
            ("ci/t.sh".to_string(), script("t", "[ \"$(cat src/value.txt)\" = ok ]"), true),
            ("ci/p.sh".to_string(), script("p", "[ -s docs/x.txt ]"), true),
            ("src/value.txt".to_string(), "bad".to_string(), false),
            ("docs/x.txt".to_string(), "doc0".to_string(), false),
        ];
        files.extend(extra);
        git(&repo, &["init", "-q"]);
        for (p, b, x) in &files {
            write_file(&repo.join(p), b, *x);
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

    /// A fixture whose gate `t` and gate `u` both guard `src` (`u` is green only when `src/other.txt` holds `ok`).
    fn with_u() -> Fx {
        Fx::with(r#"{"src":["t","u"],"docs":["p"]}"#, gate_files("u", U_TEST, true, 20000))
    }

    /// A fixture with one extra HEAD-ONLY gate that guards `docs`; the gate's check is `test`.
    fn with_docs_gate(id: &str, test: &str, timeout_ms: u64) -> Fx {
        let map = format!("{{\"src\":[\"t\"],\"docs\":[\"{id}\"]}}");
        Fx::with(&map, gate_files(id, test, false, timeout_ms))
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

    fn result_file(&self, id: &str) -> PathBuf {
        self.res.join(format!("{id}.json"))
    }

    fn result_bytes(&self, id: &str) -> Vec<u8> {
        fs::read(self.result_file(id)).unwrap()
    }

    fn rec_file(&self, id: &str) -> PathBuf {
        self.caps.join(format!("{id}.rounds"))
    }

    fn rec(&self, id: &str) -> String {
        fs::read_to_string(self.rec_file(id)).unwrap_or_else(|_| panic!("no record for {id}"))
    }

    fn plant(&self, id: &str, fix_rounds: u8, in_flight: bool) {
        fs::write(self.rec_file(id), canon(id, fix_rounds, in_flight)).unwrap();
    }

    fn streak_file(&self, id: &str) -> PathBuf {
        self.caps.join(format!("{id}.replan"))
    }

    /// The streak file's exact bytes as text.
    fn streak(&self, id: &str) -> String {
        fs::read_to_string(self.streak_file(id)).unwrap_or_else(|_| panic!("no streak file for {id}"))
    }

    fn events(&self) -> Vec<String> {
        fs::read_to_string(self.caps.join("events.jsonl")).map(|s| s.lines().map(|l| l.to_string()).collect()).unwrap_or_default()
    }

    /// Can this process be stopped from writing into a mode-0555 directory? The mode is restored before this returns.
    fn readonly_works(&self, dir: &Path) -> bool {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
        let writable = fs::write(dir.join("probe"), "x").is_ok();
        if writable {
            let _ = fs::remove_file(dir.join("probe"));
        }
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        if writable {
            println!("SKIP: this process can write into a mode-0555 directory, so the read-only arm cannot be exercised here");
        }
        !writable
    }
}

fn canon(id: &str, fix_rounds: u8, in_flight: bool) -> String {
    format!("{{\"v\":1,\"ticket\":\"{id}\",\"repo_id\":\"r\",\"fix_rounds\":{fix_rounds},\"in_flight\":{in_flight}}}\n")
}

fn streak_canon(id: &str, sig: &str, head: &str, count: u8) -> String {
    format!("{{\"v\":1,\"ticket\":\"{id}\",\"repo_id\":\"r\",\"sig\":\"{sig}\",\"head\":\"{head}\",\"count\":{count}}}\n")
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

fn run_args(cwd: &Path, args: &[String], env: &[(&str, &Path)]) -> Output {
    let mut c = Command::new(BIN);
    c.arg("ticket").arg("gate").args(args).current_dir(cwd).stdin(Stdio::null());
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn full_args(fx: &Fx, id: &str) -> Vec<String> {
    let tf = ticket_file(fx, &ticket_json("r", id));
    vec![
        tf.to_str().unwrap().into(),
        "--repo-map".into(),
        format!("r={}", fx.repo.to_str().unwrap()),
        "--base".into(),
        fx.c0.clone(),
        "--result-dir".into(),
        fx.res.to_str().unwrap().into(),
        "--caps-dir".into(),
        fx.caps.to_str().unwrap().into(),
    ]
}

fn gate_id(fx: &Fx, id: &str, extra: &[(&str, &Path)]) -> Output {
    let mut env: Vec<(&str, &Path)> = vec![("GATE_MARKER", fx.marker.as_path())];
    env.extend_from_slice(extra);
    run_args(&fx.other, &full_args(fx, id), &env)
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

fn hint(o: &Output) -> bool {
    err(o).contains(HINT)
}

/// An error: exit 2, stdout empty, never `done=allowed`.
fn errored(o: &Output) {
    assert_eq!(code(o), 2, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty: {}", out(o));
}

fn fail_line(id: &str, head: &str) -> String {
    format!("ticket={id} head={head} result=fail done=refused\n")
}

fn refused_line(id: &str, head: &str, reason: &str) -> String {
    format!("ticket={id} head={head} result=refused done=refused reason={reason}\n")
}

/// A FAIL that is not a refusal and not an error: exit 1 and P1A-07's fixed FAIL line.
fn failed(o: &Output, id: &str, head: &str) {
    assert_eq!(code(o), 1, "stderr: {}", err(o));
    assert_eq!(out(o), fail_line(id, head), "stderr: {}", err(o));
}

/// A re-plan refusal: exit 1, the fixed refusal line, no `done=allowed`.
fn replan_refused(o: &Output, id: &str, head: &str) {
    assert_eq!(code(o), 1, "stderr: {}", err(o));
    assert_eq!(out(o), refused_line(id, head, "replan-threshold"));
    assert!(!leaks(o, "done=allowed"));
    assert!(err(o).contains("gate REFUSED reason=replan-threshold"), "{}", err(o));
}

fn mover() -> String {
    format!("git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t{}scratch commit -q --allow-empty -m moved", '@')
}

// AC 1: the 2nd FAIL of one gate and class on a different head latches (the hint, once); the 1st does not. No event on either.
#[test]
fn the_second_identical_failure_on_a_new_head_latches() {
    let (fx, c1) = Fx::failing();
    let o = gate(&fx);
    failed(&o, "T-1", &c1);
    assert!(!hint(&o), "twin: the 1st failure shows no hint: {}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", SIG_T, &c1, 1));
    let c2 = fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(hint(&o), "the 2nd identical failure shows the hint: {}", err(&o));
    assert!(err(&o).contains("replan_required"), "{}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", SIG_T, &c2, 2));
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false));
    assert!(!fx.caps.join("events.jsonl").exists(), "no refusal yet, so no event");
}

// AC 2: run 3 is refused even on a head that would PASS. Nothing runs, nothing is paid, every file is as it was. Twin: run 2 executes.
#[test]
fn a_latched_ticket_is_refused_before_anything_runs() {
    let (fx, _) = Fx::failing();
    assert_eq!(code(&gate(&fx)), 1);
    fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate(&fx);
    assert_eq!(code(&o), 1);
    assert!(hint(&o));
    let c3 = fx.commit(&[("src/value.txt", "ok")]);
    let (markers, result, rec, streak) = (fx.markers(), fx.result_bytes("T-1"), fx.rec("T-1"), fx.streak("T-1"));
    assert_eq!(rec, canon("T-1", 2, false));
    let o = gate(&fx);
    replan_refused(&o, "T-1", &c3);
    assert_eq!(fx.markers(), markers, "nothing ran");
    assert_eq!(fx.result_bytes("T-1"), result, "the result file is untouched");
    assert_eq!(fx.rec("T-1"), rec, "no round was paid for the refusal");
    assert_eq!(fx.streak("T-1"), streak, "the streak is untouched");
    assert!(!leaks(&o, fx.caps.to_str().unwrap()) && !leaks(&o, fx.res.to_str().unwrap()), "no folder is echoed");
    assert_eq!(fx.events().len(), 1, "exactly one event");
    assert!(fx.events()[0].contains("replan_required") && fx.events()[0].contains("replan-threshold"), "{}", fx.events()[0]);
    // A second call is another refusal and another event; the ticket stays latched.
    replan_refused(&gate(&fx), "T-1", &c3);
    assert_eq!(fx.events().len(), 2);
}

// AC 3: a failure that changes gate keeps going; only CONSECUTIVE identical failures latch. Twin: AC 1.
#[test]
fn a_failure_that_changes_gate_keeps_going() {
    let fx = Fx::with_u();
    fx.commit(&[("src/value.txt", "bad2")]);
    let o = gate(&fx);
    assert_eq!((code(&o), hint(&o)), (1, false), "run 1 (t): {}", err(&o));
    assert!(err(&o).contains("gate=t"), "{}", err(&o));
    // t now passes and u fails.
    let c2 = fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "bad")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(err(&o).contains("gate=u") && !hint(&o), "run 2 (u): {}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", "u:check-failed", &c2, 1));
    // t fails again: the earlier t failure is not "in a row" with this one.
    let c3 = fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c3);
    assert!(err(&o).contains("gate=t") && !hint(&o), "run 3 (t, in a row with nothing): {}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", SIG_T, &c3, 1));
    assert!(fx.events().is_empty());
    // Twin: the 2nd consecutive t failure latches.
    fx.commit(&[("src/value.txt", "bad4")]);
    let o = gate(&fx);
    assert_eq!((code(&o), hint(&o)), (1, true), "run 4: {}", err(&o));
}

// AC 4: the same gate failing with a different class is a different failure. Twin: AC 1 (the same class latches).
#[test]
fn the_same_gate_with_a_different_class_does_not_latch() {
    let test = "v=$(cat docs/x.txt)\n[ \"$v\" = bad ] && exit 1\n[ \"$v\" = hang ] && exec sleep 30\ntrue";
    // The budget is generous: the first start of a freshly written script can take a second on some hosts, and that must stay `check-failed`.
    let fx = Fx::with_docs_gate("h", test, 5000);
    fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "bad")]);
    let o = gate(&fx);
    assert!(err(&o).contains("class=check-failed gate=h") && !hint(&o), "{}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", "h:check-failed", &fx.head(), 1));
    let c2 = fx.commit(&[("docs/x.txt", "hang")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(err(&o).contains("class=timeout gate=h"), "the check timed out: {}", err(&o));
    assert!(!hint(&o), "a different class is not the same failure: {}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", "h:timeout", &c2, 1));
    fx.commit(&[("docs/x.txt", "bad")]);
    let o = gate(&fx);
    assert_eq!(code(&o), 1, "run 3 executes: {}", err(&o));
    assert!(!out(&o).contains("result=refused") && fx.events().is_empty());
}

// AC 5: the event is valid and uses the lowercased id; the streak file keeps the exact-case name.
#[test]
fn the_refusal_writes_a_valid_replan_event() {
    let (fx, c1) = Fx::failing();
    assert_eq!(code(&gate_id(&fx, "EX-1", &[])), 1);
    assert!(!fx.caps.join("events.jsonl").exists(), "twin: one failure writes no log");
    let c2 = fx.commit(&[("src/value.txt", "bad3")]);
    assert_eq!(code(&gate_id(&fx, "EX-1", &[])), 1);
    assert!(c1 != c2);
    let listed: Vec<String> = fs::read_dir(&fx.caps).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(listed.contains(&"EX-1.replan".to_string()) && !listed.contains(&"ex-1.replan".to_string()), "{listed:?}");
    let o = gate_id(&fx, "EX-1", &[]);
    replan_refused(&o, "EX-1", &c2);
    let log = fx.caps.join("events.jsonl");
    let v = Command::new(BIN).args(["events", "verify", "--log", log.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&v), 0, "{}", err(&v));
    let l = Command::new(BIN).args(["events", "list", "--log", log.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&l), 0);
    for needle in ["replan_required", "ex-1", "replan-threshold"] {
        assert!(out(&l).contains(needle), "events list lacks `{needle}`: {}", out(&l));
    }
    let read = events::read_log(&log).unwrap();
    assert!(read.is_clean());
    assert_eq!(read.events.len(), 1);
    let e = &read.events[0];
    assert_eq!((e.kind.as_str(), e.ticket.as_str(), e.repo.as_str(), e.reason.as_str()), ("replan_required", "ex-1", "r", "replan-threshold"));
    assert!(e.ts_ms > 0);
    assert_eq!(code(&gate_id(&fx, "EX-1", &[])), 1);
    assert_eq!(fx.events().len(), 2, "a second refused call appends a second event");
}

// AC 6: a plain re-run of one head is not a fix attempt; the cap still bounds it. Twin: AC 1 (two heads latch).
#[test]
fn a_rerun_of_the_same_head_does_not_extend_the_streak() {
    let (fx, c1) = Fx::failing();
    for k in 1..=4u8 {
        let o = gate(&fx);
        failed(&o, "T-1", &c1);
        assert!(!hint(&o), "run {k}: no re-plan hint: {}", err(&o));
        assert_eq!(err(&o).contains(CAP_HINT), k == 4, "run {k}: only the 4th run shows the cap hint: {}", err(&o));
        assert_eq!(fx.streak("T-1"), streak_canon("T-1", SIG_T, &c1, 1), "run {k}: the streak stays at 1");
    }
    assert!(fx.events().is_empty());
    let o = gate(&fx);
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), refused_line("T-1", &c1, "fix-round-cap"), "the 5th run is the cap's, not the re-plan's");
    assert_eq!(fx.events().len(), 1);
    assert!(fx.events()[0].contains("escalation"));
}

// AC 7: an empty commit is a new head and counts. Twin: AC 6 (the same head does not).
#[test]
fn a_cosmetic_new_head_counts_as_a_fix_attempt() {
    let (fx, c1) = Fx::failing();
    assert_eq!(code(&gate(&fx)), 1);
    git(&fx.repo, &["commit", "-q", "--allow-empty", "-m", "cosmetic"]);
    let c2 = fx.head();
    assert_ne!(c1, c2);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(hint(&o), "{}", err(&o));
    replan_refused(&gate(&fx), "T-1", &c2);
}

fn tripwire_test(kind: &str) -> String {
    let act = match kind {
        "move" => mover(),
        _ => "kill -9 $PPID; sleep 5".to_string(),
    };
    format!("if [ -e \"$TRIG\" ] && [ ! -e \"$ONCE\" ]; then : > \"$ONCE\"; {act}; fi\n[ \"$(cat docs/x.txt)\" = ok ]")
}

// AC 8: an error (or a killed run) between two failures neither breaks nor extends the streak. Twin: the same sequence without the error.
#[test]
fn an_error_between_two_failures_does_not_touch_the_streak() {
    for kind in ["move", "kill"] {
        let fx = Fx::with_docs_gate("m", &tripwire_test(kind), 20000);
        let tmp = Tmp::new();
        let (trig, once) = (tmp.0.join("trig"), tmp.0.join("once"));
        let env = [("TRIG", trig.as_path()), ("ONCE", once.as_path())];
        let c1 = fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "bad1")]);
        let o = gate_id(&fx, "T-1", &env);
        failed(&o, "T-1", &c1);
        assert!(err(&o).contains("gate=m") && !hint(&o), "{kind} run 1: {}", err(&o));
        fs::write(&trig, "").unwrap();
        let o = gate_id(&fx, "T-1", &env);
        if kind == "move" {
            errored(&o);
        } else {
            assert!(o.status.code().is_none(), "the gate process was killed: {:?}", o.status);
        }
        assert_eq!(fx.rec("T-1"), canon("T-1", 1, true), "{kind}: the dead run keeps its paid round");
        assert_eq!(fx.streak("T-1"), streak_canon("T-1", "m:check-failed", &c1, 1), "{kind}: the streak is unchanged by the error");
        let c3 = fx.commit(&[("docs/x.txt", "bad2")]);
        let o = gate_id(&fx, "T-1", &env);
        failed(&o, "T-1", &c3);
        assert!(hint(&o), "{kind}: FAIL, error, FAIL (the same signature) still latches: {}", err(&o));
        replan_refused(&gate_id(&fx, "T-1", &env), "T-1", &c3);
    }
    // Different second signature: not latched, and the next run executes.
    let fx = Fx::with_docs_gate("m", &tripwire_test("move"), 20000);
    let tmp = Tmp::new();
    let (trig, once) = (tmp.0.join("trig"), tmp.0.join("once"));
    let env = [("TRIG", trig.as_path()), ("ONCE", once.as_path())];
    fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "bad1")]);
    assert_eq!(code(&gate_id(&fx, "T-1", &env)), 1);
    fs::write(&trig, "").unwrap();
    errored(&gate_id(&fx, "T-1", &env));
    // `m` now passes and `t` fails instead (a different gate).
    let c3 = fx.commit(&[("src/value.txt", "bad5"), ("docs/x.txt", "ok")]);
    let o = gate_id(&fx, "T-1", &env);
    failed(&o, "T-1", &c3);
    assert!(err(&o).contains("gate=t") && !hint(&o), "a different signature after an error is not latched: {}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", SIG_T, &c3, 1));
    fx.commit(&[("src/value.txt", "bad6")]);
    let o = gate_id(&fx, "T-1", &env);
    assert_eq!(code(&o), 1);
    assert!(!out(&o).contains("result=refused"), "{}", out(&o));
}

// AC 9: a PASS clears the streak. Twin: AC 1.
#[test]
fn a_pass_clears_the_streak() {
    let (fx, _) = Fx::failing();
    assert_eq!(code(&gate(&fx)), 1);
    assert!(fx.streak_file("T-1").exists());
    fx.commit(&[("src/value.txt", "ok")]);
    let o = gate(&fx);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!fx.streak_file("T-1").exists(), "the PASS removed the streak file");
    let c3 = fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c3);
    assert!(!hint(&o), "FAIL, PASS, FAIL is not two in a row: {}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", SIG_T, &c3, 1));
    fx.commit(&[("src/value.txt", "bad4")]);
    let o = gate(&fx);
    assert_eq!(code(&o), 1);
    assert!(!out(&o).contains("result=refused"), "run 4 executes");
}

// AC 10: verdicts that run no check have signatures. Twin: the two-same case latches, the mixed case does not.
#[test]
fn verdicts_that_run_no_check_have_signatures() {
    // Two unmapped-path failures on two heads.
    let fx = Fx::new();
    fx.commit(&[("other/x.txt", "a")]);
    let o = gate(&fx);
    assert!(err(&o).contains("class=unmapped-path") && !hint(&o), "{}", err(&o));
    let first = fx.streak("T-1");
    assert!(first.contains("unmapped-path"), "{first}");
    let c2 = fx.commit(&[("other/y.txt", "b")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(hint(&o), "{}", err(&o));
    replan_refused(&gate(&fx), "T-1", &c2);
    assert!(fx.markers().is_empty(), "no check ever ran");
    // Two no-checks failures on two heads.
    let fx = Fx::new();
    git(&fx.repo, &["commit", "-q", "--allow-empty", "-m", "e1"]);
    let o = gate(&fx);
    assert!(fs::read_to_string(fx.result_file("T-1")).unwrap().contains("no-checks") && !hint(&o));
    git(&fx.repo, &["commit", "-q", "--allow-empty", "-m", "e2"]);
    let o = gate(&fx);
    assert!(hint(&o), "{}", err(&o));
    // unmapped-path, then no-checks: different gate ids and classes.
    let fx = Fx::new();
    fx.commit(&[("other/x.txt", "a")]);
    assert_eq!(code(&gate(&fx)), 1);
    git(&fx.repo, &["rm", "-q", "other/x.txt"]);
    git(&fx.repo, &["commit", "-q", "-m", "undo"]);
    let o = gate(&fx);
    assert_eq!(code(&o), 1);
    assert!(fs::read_to_string(fx.result_file("T-1")).unwrap().contains("no-checks"), "the diff is empty again");
    assert!(!hint(&o), "an unmapped-path failure then a no-checks failure are different: {}", err(&o));
    fx.commit(&[("other/z.txt", "c")]);
    let o = gate(&fx);
    assert_eq!(code(&o), 1);
    assert!(!out(&o).contains("result=refused") && fx.events().is_empty());
}

// AC 11: both rules at once is one `replan-threshold` event. Twin: failures that change reach the cap instead.
#[test]
fn the_cap_and_the_threshold_together_write_one_replan_event() {
    let (fx, _) = Fx::failing();
    fx.plant("T-1", 2, false);
    let o = gate(&fx);
    assert_eq!((code(&o), hint(&o)), (1, false), "{}", err(&o));
    let c2 = fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    let e = err(&o);
    assert!(e.contains(CAP_HINT) && e.contains(HINT), "both hints: {e}");
    assert!(e.find(CAP_HINT) < e.find(HINT), "the cap line first: {e}");
    assert_eq!(fx.rec("T-1"), canon("T-1", 4, false));
    replan_refused(&gate(&fx), "T-1", &c2);
    let lines = fx.events();
    assert_eq!(lines.len(), 1, "exactly one event: {lines:?}");
    assert!(lines[0].contains("replan_required") && !lines[0].contains("escalation"), "{}", lines[0]);
    // Twin: failures that change gate reach the cap, as in P1A-09.
    let fx = Fx::with_u();
    fx.plant("T-1", 2, false);
    fx.commit(&[("src/value.txt", "bad2")]);
    assert_eq!(code(&gate(&fx)), 1);
    let c2 = fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "bad")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(err(&o).contains(CAP_HINT) && !hint(&o), "{}", err(&o));
    let o = gate(&fx);
    assert_eq!(out(&o), refused_line("T-1", &c2, "fix-round-cap"));
    assert!(fx.events().len() == 1 && fx.events()[0].contains("escalation"));
}

// AC 12: the signature ignores the message. Intended coarse behaviour (plan Open question 6): a repo whose whole suite is one gate latches after any
// two failing fix attempts on new heads, even when a different test now fails. A finer signature would be worker-controllable.
#[test]
fn the_signature_ignores_the_message() {
    let fx = Fx::with_docs_gate("m", "echo \"got $(cat docs/x.txt)\"\nexit 1", 20000);
    fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "x1")]);
    let o = gate(&fx);
    assert_eq!((code(&o), hint(&o)), (1, false), "{}", err(&o));
    let m1 = fs::read_to_string(fx.result_file("T-1")).unwrap();
    let c2 = fx.commit(&[("docs/x.txt", "x2")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    let m2 = fs::read_to_string(fx.result_file("T-1")).unwrap();
    assert_ne!(m1, m2, "the two result files differ (message and head), so the signature is proven to ignore the message");
    assert!(m1.contains("got x1") && m2.contains("got x2"), "{m1} / {m2}");
    assert!(hint(&o), "{}", err(&o));
    replan_refused(&gate(&fx), "T-1", &c2);
}

// AC 13: tampering with the streak file DURING a run changes nothing for that run. Twin: no tampering (latches too).
#[test]
fn tampering_during_a_run_does_not_reset_the_streak() {
    let forged = format!("printf '%s\\n' '{}' > \"$GATE_CAPS/T-1.replan\"", streak_canon("T-1", "zz:check-failed", &"a".repeat(40), 1).trim_end());
    for (label, tamper) in [("delete", "rm -f \"$GATE_CAPS/T-1.replan\"".to_string()), ("forge", forged)] {
        let fx = Fx::with_docs_gate("k", &format!("{tamper}\nfalse"), 20000);
        let caps = fx.caps.clone();
        let env = [("GATE_CAPS", caps.as_path())];
        fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "d1")]);
        let o = gate_id(&fx, "T-1", &env);
        assert_eq!((code(&o), hint(&o)), (1, false), "{label} run 1: {}", err(&o));
        let c2 = fx.commit(&[("docs/x.txt", "d2")]);
        let o = gate_id(&fx, "T-1", &env);
        failed(&o, "T-1", &c2);
        assert!(hint(&o), "{label}: the tampering did not restart the count: {}", err(&o));
        assert_eq!(fx.streak("T-1"), streak_canon("T-1", "k:check-failed", &c2, 2), "{label}: written from the value read before the run");
        replan_refused(&gate_id(&fx, "T-1", &env), "T-1", &c2);
    }
}

// AC 14: a P1A-09-era record with no streak file still works. Twin: the same ticket with a streak file would be read as one.
#[test]
fn a_record_without_a_streak_file_is_read_as_no_streak() {
    let (fx, c1) = Fx::failing();
    fx.plant("T-1", 1, false);
    assert!(!fx.streak_file("T-1").exists());
    let o = gate(&fx);
    failed(&o, "T-1", &c1);
    assert!(!hint(&o), "one failure after a planted record does not latch: {}", err(&o));
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", SIG_T, &c1, 1));
}

fn plant_streak(fx: &Fx, bytes: &[u8]) {
    fs::write(fx.streak_file("T-1"), bytes).unwrap();
}

// AC 15: a strict streak file. Twin: the canonical file runs.
#[test]
fn a_streak_file_that_is_not_canonical_is_refused_never_read_as_none() {
    let (fx, c1) = Fx::failing();
    let h = "a".repeat(40);
    let good = streak_canon("T-1", SIG_T, &h, 1);
    let long_sig = format!("{}:c", "a".repeat(319));
    assert_eq!(long_sig.len(), 321);
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("not JSON", b"nope\n".to_vec()),
        ("empty", vec![]),
        ("extra key", good.replace("\"count\"", "\"x\":1,\"count\"").into_bytes()),
        ("missing key", good.replace("\"v\":1,", "").into_bytes()),
        ("reordered", format!("{{\"v\":1,\"repo_id\":\"r\",\"ticket\":\"T-1\",\"sig\":\"{SIG_T}\",\"head\":\"{h}\",\"count\":1}}\n").into_bytes()),
        ("count 0", good.replace("\"count\":1", "\"count\":0").into_bytes()),
        ("count 3", good.replace("\"count\":1", "\"count\":3").into_bytes()),
        ("count -1", good.replace("\"count\":1", "\"count\":-1").into_bytes()),
        ("count string", good.replace("\"count\":1", "\"count\":\"1\"").into_bytes()),
        ("count 01", good.replace("\"count\":1", "\"count\":01").into_bytes()),
        ("sig without colon", good.replace(SIG_T, "tcheck-failed").into_bytes()),
        ("sig uppercase", good.replace(SIG_T, "T:check-failed").into_bytes()),
        ("sig with a space", good.replace(SIG_T, "t:check failed").into_bytes()),
        ("sig over the bound", good.replace(SIG_T, &long_sig).into_bytes()),
        ("head not a sha", good.replace(&h, "abc").into_bytes()),
        ("head uppercase", good.replace(&h, &"A".repeat(40)).into_bytes()),
        ("v 2", good.replace("\"v\":1", "\"v\":2").into_bytes()),
        ("trailing bytes", format!("{good}x").into_bytes()),
        ("second line", format!("{good}{good}").into_bytes()),
        ("NUL byte", [good.as_bytes(), b"\0"].concat()),
        ("non-UTF-8", vec![0xff, 0xfe, 0xfd]),
        ("no newline", good.trim_end().as_bytes().to_vec()),
        ("over 4 KiB", vec![b'x'; 4097]),
    ];
    for (label, bytes) in &bad {
        plant_streak(&fx, bytes);
        let o = gate(&fx);
        errored(&o);
        assert_eq!(&fs::read(fx.streak_file("T-1")).unwrap(), bytes, "{label}: the streak bytes are unchanged");
        assert!(fx.markers().is_empty(), "{label}: nothing ran");
        assert!(!fx.result_file("T-1").exists() && !fx.rec_file("T-1").exists(), "{label}: a bad streak is not read as none");
    }
    plant_streak(&fx, good.as_bytes());
    let o = gate(&fx);
    failed(&o, "T-1", &c1);
    assert!(hint(&o), "twin: the canonical streak is read (count 1 on another head, now 2): {}", err(&o));
}

// AC 16: identity and names. Twin: the matching file.
#[test]
fn a_streak_for_another_ticket_or_repo_or_case_is_refused() {
    let (fx, _) = Fx::failing();
    assert_eq!(code(&gate(&fx)), 1);
    let before = fx.markers().len();
    fs::write(fx.streak_file("T-2"), fx.streak("T-1")).unwrap();
    errored(&gate_id(&fx, "T-2", &[]));
    fs::write(fx.streak_file("T-1"), fx.streak("T-1").replace("\"repo_id\":\"r\"", "\"repo_id\":\"q\"")).unwrap();
    errored(&gate(&fx));
    // The twin holds a VALID streak for EX-1, so on a case-insensitive disk the name scan (not a parse error) is what refuses it.
    let twin = streak_canon("EX-1", SIG_T, &"a".repeat(40), 1);
    fs::write(fx.caps.join("ex-1.replan"), &twin).unwrap();
    errored(&gate_id(&fx, "EX-1", &[]));
    assert_eq!(fs::read_to_string(fx.caps.join("ex-1.replan")).unwrap(), twin);
    assert_eq!(fx.markers().len(), before, "nothing ran in any refusal case");
    fs::write(fx.streak_file("T-1"), streak_canon("T-1", SIG_T, &fx.head(), 1)).unwrap();
    assert_eq!(code(&gate(&fx)), 1, "twin: the matching file runs");
    // Ids `a` and `a.replan` keep separate streak files.
    let (fx, _) = Fx::failing();
    assert_eq!(code(&gate_id(&fx, "a", &[])), 1);
    assert_eq!(code(&gate_id(&fx, "a.replan", &[])), 1);
    assert!(fx.streak("a").contains("\"ticket\":\"a\"") && fx.streak("a.replan").contains("\"ticket\":\"a.replan\""));
    assert!(fx.caps.join("a.replan").is_file() && fx.caps.join("a.replan.replan").is_file());
    fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate_id(&fx, "a", &[]);
    assert!(hint(&o), "a's own streak, not a.replan's: {}", err(&o));
    assert_eq!(fx.streak("a.replan").matches("\"count\":1").count(), 1, "a.replan's streak is untouched");
}

// AC 17: a link is never followed. Twin: the same names as regular files (AC 15's canonical file).
#[test]
fn links_are_never_followed() {
    let keep = "keep me";
    let setup = || {
        let (fx, _) = Fx::failing();
        let outside = fx.t.0.join("outside.txt");
        fs::write(&outside, keep).unwrap();
        (fx, outside)
    };
    let (fx, outside) = setup();
    let valid = streak_canon("T-1", SIG_T, &"a".repeat(40), 1);
    fs::write(&outside, &valid).unwrap();
    symlink(&outside, fx.streak_file("T-1")).unwrap();
    errored(&gate(&fx));
    assert!(fx.markers().is_empty());
    assert_eq!(fs::read_to_string(&outside).unwrap(), valid, "the link target is untouched");
    let (fx, _) = setup();
    let ghost = fx.t.0.join("ghost.txt");
    symlink(&ghost, fx.streak_file("T-1")).unwrap();
    errored(&gate(&fx));
    assert!(!ghost.exists() && fx.markers().is_empty(), "nothing was created through the dangling link");
    // The temp name is a link (live, then dangling): never written through.
    let (fx, outside) = setup();
    symlink(&outside, fx.caps.join(".T-1.replan.tmp")).unwrap();
    let o = gate(&fx);
    assert!([1, 2].contains(&code(&o)), "{}", err(&o));
    assert_eq!(fs::read_to_string(&outside).unwrap(), keep, "the temp link was not written through");
    let (fx, _) = setup();
    let ghost = fx.t.0.join("ghost2.txt");
    symlink(&ghost, fx.caps.join(".T-1.replan.tmp")).unwrap();
    let o = gate(&fx);
    assert!([1, 2].contains(&code(&o)), "{}", err(&o));
    assert!(!ghost.exists(), "nothing was created at the dangling target");
    // Twin: no link at all.
    let (fx, _) = setup();
    assert_eq!(code(&gate(&fx)), 1);
    assert!(fx.streak_file("T-1").is_file());
}

// AC 17b: a stale regular temp left by a killed run does not wedge the ticket. Twin: the same sequence with no stale temp.
#[test]
fn a_stale_streak_temp_does_not_wedge_the_ticket() {
    let (fx, _) = Fx::failing();
    let o = gate(&fx);
    assert_eq!(code(&o), 1);
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, false));
    fs::write(fx.caps.join(".T-1.replan.tmp"), "junk from a killed run").unwrap();
    let c2 = fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(hint(&o), "{}", err(&o));
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", SIG_T, &c2, 2));
    assert!(!fx.caps.join(".T-1.replan.tmp").exists(), "the stale temp is gone");
    replan_refused(&gate(&fx), "T-1", &c2);
}

// AC 18 is the set of unit tests in `caps.rs` (golden bytes, the transition table, the start rule, the signature bound).

// AC 19: write order for a FAIL. Only sub-case a can show the order; sub-case b shows fail-closed. Twin: no directory planted.
#[test]
fn a_failed_streak_write_stops_before_the_record_and_the_result() {
    let make = || {
        let fx = Fx::with_docs_gate("d", "[ -e \"$TRIG\" ] && mkdir \"$GATE_CAPS/.T-1.replan.tmp\"\nfalse", 20000);
        fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "d1")]);
        fx
    };
    let tmp = Tmp::new();
    let trig = tmp.0.join("trig");
    let fx = make();
    let caps = fx.caps.clone();
    let env = [("TRIG", trig.as_path()), ("GATE_CAPS", caps.as_path())];
    assert_eq!(code(&gate_id(&fx, "T-1", &env)), 1);
    let (r1, s1) = (fx.result_bytes("T-1"), fx.streak("T-1"));
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, false));
    fs::write(&trig, "").unwrap();
    fx.commit(&[("docs/x.txt", "d2")]);
    let o = gate_id(&fx, "T-1", &env);
    errored(&o);
    assert_eq!(fx.result_bytes("T-1"), r1, "the result file is as it was");
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, true), "the record was not written after the failed streak write: still the pay state");
    assert_eq!(fx.streak("T-1"), s1, "the streak is as it was");
    assert!(!leaks(&o, fx.caps.to_str().unwrap()));
    // Sub-case b: the whole caps folder goes read-only mid-run.
    let fx = Fx::with_docs_gate("d", "[ -e \"$TRIG\" ] && chmod 555 \"$GATE_CAPS\"\nfalse", 20000);
    fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "d1")]);
    let caps = fx.caps.clone();
    if !fx.readonly_works(&caps) {
        return;
    }
    let tmp2 = Tmp::new();
    let trig2 = tmp2.0.join("trig");
    let env = [("TRIG", trig2.as_path()), ("GATE_CAPS", caps.as_path())];
    assert_eq!(code(&gate_id(&fx, "T-1", &env)), 1);
    let (r1, s1) = (fx.result_bytes("T-1"), fx.streak("T-1"));
    fs::write(&trig2, "").unwrap();
    fx.commit(&[("docs/x.txt", "d2")]);
    let o = gate_id(&fx, "T-1", &env);
    fs::set_permissions(&fx.caps, fs::Permissions::from_mode(0o755)).unwrap();
    errored(&o);
    assert_eq!(fx.result_bytes("T-1"), r1);
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, true));
    assert_eq!(fx.streak("T-1"), s1);
    // Twin: no directory planted, no read-only folder.
    let fx = make();
    let tmp3 = Tmp::new();
    let trig3 = tmp3.0.join("trig");
    let caps = fx.caps.clone();
    let env = [("TRIG", trig3.as_path()), ("GATE_CAPS", caps.as_path())];
    assert_eq!(code(&gate_id(&fx, "T-1", &env)), 1);
    let c2 = fx.commit(&[("docs/x.txt", "d2")]);
    let o = gate_id(&fx, "T-1", &env);
    failed(&o, "T-1", &c2);
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", "d:check-failed", &c2, 2));
}

// AC 20: a PASS clears the streak BEFORE the refund is written. Twin: a PASS with no directory planted.
#[test]
fn a_pass_clears_the_streak_before_the_refund() {
    let make = || {
        let fx = Fx::with_docs_gate("d", "[ -e \"$TRIG\" ] && mkdir \"$GATE_CAPS/.T-1.rounds.tmp\"\n[ \"$(cat docs/x.txt)\" = ok ]", 20000);
        fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "d1")]);
        fx
    };
    let tmp = Tmp::new();
    let trig = tmp.0.join("trig");
    let fx = make();
    let caps = fx.caps.clone();
    let env = [("TRIG", trig.as_path()), ("GATE_CAPS", caps.as_path())];
    assert_eq!(code(&gate_id(&fx, "T-1", &env)), 1);
    let r1 = fx.result_bytes("T-1");
    assert!(fx.streak_file("T-1").exists());
    fs::write(&trig, "").unwrap();
    fx.commit(&[("docs/x.txt", "ok")]);
    let o = gate_id(&fx, "T-1", &env);
    errored(&o);
    assert!(!leaks(&o, "done=allowed"));
    assert_eq!(fx.result_bytes("T-1"), r1, "no PASS file");
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, true), "the refund never landed: the pay state");
    assert!(!fx.streak_file("T-1").exists(), "the streak was cleared first");
    // Twin: the same PASS with no directory planted.
    let fx = make();
    let tmp2 = Tmp::new();
    let trig2 = tmp2.0.join("trig");
    let caps = fx.caps.clone();
    let env = [("TRIG", trig2.as_path()), ("GATE_CAPS", caps.as_path())];
    assert_eq!(code(&gate_id(&fx, "T-1", &env)), 1);
    fx.commit(&[("docs/x.txt", "ok")]);
    let o = gate_id(&fx, "T-1", &env);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(fx.rec("T-1"), canon("T-1", 1, false));
    assert!(!fx.streak_file("T-1").exists());
}

// AC 20b: a decided FAIL stays counted when only the result write fails. Twin: a writable result folder.
#[test]
fn a_decided_failure_stays_counted_when_the_result_cannot_be_written() {
    let fx = Fx::with_docs_gate("d", "[ -e \"$TRIG\" ] && chmod 555 \"$GATE_RES\"\nfalse", 20000);
    fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "d1")]);
    if !fx.readonly_works(&fx.res) {
        return;
    }
    let tmp = Tmp::new();
    let trig = tmp.0.join("trig");
    let (caps, res) = (fx.caps.clone(), fx.res.clone());
    let env = [("TRIG", trig.as_path()), ("GATE_CAPS", caps.as_path()), ("GATE_RES", res.as_path())];
    assert_eq!(code(&gate_id(&fx, "T-1", &env)), 1);
    let r1 = fx.result_bytes("T-1");
    fs::write(&trig, "").unwrap();
    let c2 = fx.commit(&[("docs/x.txt", "d2")]);
    let o = gate_id(&fx, "T-1", &env);
    fs::set_permissions(&fx.res, fs::Permissions::from_mode(0o755)).unwrap();
    errored(&o);
    assert!(!leaks(&o, "done=allowed") && !hint(&o), "no hint on an exit-2 run: {}", err(&o));
    assert_eq!(fx.result_bytes("T-1"), r1, "the result file is as it was");
    assert_eq!(fx.rec("T-1"), canon("T-1", 2, false));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", "d:check-failed", &c2, 2), "the FAIL is counted");
    fs::remove_file(&trig).unwrap();
    replan_refused(&gate_id(&fx, "T-1", &env), "T-1", &c2);
}

// AC 21: the longest reachable gate id (250 bytes) latches. Twin: a short gate id (AC 1).
#[test]
fn the_longest_gate_id_latches() {
    let id = "a".repeat(250);
    let (g, p) = (format!("gates/{id}.json"), "ci/long.sh".to_string());
    let extra = vec![(g, gate_json(&id, &p, false, 20000), false), (p, script("long", "false"), true)];
    let map = format!("{{\"src\":[\"t\"],\"docs\":[\"{id}\"]}}");
    let fx = Fx::with(&map, extra);
    fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "d1")]);
    let o = gate(&fx);
    assert_eq!((code(&o), hint(&o)), (1, false), "{}", err(&o));
    assert!(err(&o).contains(&format!("class=check-failed gate={id}")), "{}", err(&o));
    let c2 = fx.commit(&[("docs/x.txt", "d2")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(hint(&o), "{}", err(&o));
    assert_eq!(fx.streak("T-1"), streak_canon("T-1", &format!("{id}:check-failed"), &c2, 2));
    replan_refused(&gate(&fx), "T-1", &c2);
}
