//! Process-level smoke for the per-ticket wall-time budget in `harness ticket gate` (P1A-11 part B, ticket 93). Every test runs
//! the real binary as a child process against scratch repos under the system temp folder, from the folder of a DIFFERENT repo,
//! with its own result folder, caps folder and settings file. Variables are set only on the child. Every row names its planted
//! failing twin (the same run with one thing changed, WHICH MUST FAIL) and the mutant it kills — both ends.
//!
//! Fixture repo `r`: both-ends gate `t` (green only when `src/value.txt` holds `ok`), both-ends gate `u` on `src/other.txt`
//! (green only when it holds `ok`), `gates/path-map.json` maps `src` -> t,u and `docs` -> p unless a row says otherwise. Every
//! script appends one line `<name> <pwd>` to the file named by `GATE_MARKER`, a file OUTSIDE `r`, so "nothing ran" is "the
//! marker file is absent". The settings file carries the FULL v4 key set with `m_wall_seconds: 3600` unless the row says
//! otherwise. Time passes by PLANTING a budget file with an earlier `started_ms` (no test clock override) in every row except
//! the B8 rows, the one bounded real waits (60 s deadline, polled, never a fixed sleep).
#![cfg(unix)]
use harness::events;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

const BUDGET_HINT: &str = "wall-time budget spent";
const REPLAN_HINT: &str = "re-plan threshold reached";

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("hbud-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        if let Ok(rd) = fs::read_dir(&self.0) {
            for e in rd.flatten() {
                let _ = fs::set_permissions(e.path(), fs::Permissions::from_mode(0o755));
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One git call in a scratch repo: no hooks, no inherited repo-redirecting names, no stdin, fixed identity.
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

fn gate_files(id: &str, test: &str, both: bool, timeout_ms: u64) -> Vec<File3> {
    let (g, p) = (format!("gates/{id}.json"), format!("ci/{id}.sh"));
    vec![(g, gate_json(id, &p, both, timeout_ms), false), (p, script(id, test), true)]
}

const MAP_TU: &str = r#"{"src":["t","u"],"docs":["p"]}"#;

fn write_file(path: &Path, body: &str, exec: bool) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
}

/// The full v4 settings key set. The wall-time limits are the row's own; every other number is generous or pinned policy.
fn settings_text(s: u64, m: u64, l: u64) -> String {
    format!(
        "repos:\n\nbudgets:\n  s_wall_seconds: {s}\n  s_tokens: 1000000\n  m_wall_seconds: {m}\n  m_tokens: 1000000\n  l_wall_seconds: {l}\n  l_tokens: 1000000\n\nshadow:\n  p1a_cap: 5\n  p4a_cap: 10\n  d2a_cap: 20\n  p7_cap: 10\n  p1a_token_budget: 1\n  p4a_token_budget: 1\n  d2a_token_budget: 1\n  p7_token_budget: 1\n\nlanes:\n  count: 4\n\nfriction:\n  inbox: /placeholder/friction-inbox.jsonl\n\nfactory:\n  design_tier: T3\n"
    )
}

struct Fx {
    t: Tmp,
    repo: PathBuf,
    other: PathBuf,
    marker: PathBuf,
    res: PathBuf,
    caps: PathBuf,
    cfg: PathBuf,
    c0: String,
}

impl Fx {
    /// The default fixture: `m_wall_seconds` 3600 (S and L generous), map `src` -> t,u.
    fn new() -> Fx {
        Fx::with(MAP_TU, vec![])
    }

    fn with(map: &str, extra: Vec<File3>) -> Fx {
        let t = Tmp::new();
        let (repo, other, marker_dir, res, caps) = (t.0.join("r"), t.0.join("o"), t.0.join("marker"), t.0.join("res"), t.0.join("caps"));
        for d in [&repo, &other, &marker_dir, &res, &caps] {
            fs::create_dir_all(d).unwrap();
        }
        let mut files: Vec<File3> = vec![
            ("gates/t.json".to_string(), gate_json("t", "ci/t.sh", true, 20000), false),
            ("gates/u.json".to_string(), gate_json("u", "ci/u.sh", true, 20000), false),
            ("gates/p.json".to_string(), gate_json("p", "ci/p.sh", false, 20000), false),
            ("gates/path-map.json".to_string(), map.to_string(), false),
            ("ci/t.sh".to_string(), script("t", "[ \"$(cat src/value.txt)\" = ok ]"), true),
            ("ci/u.sh".to_string(), script("u", "[ \"$(cat src/other.txt)\" = ok ]"), true),
            ("ci/p.sh".to_string(), script("p", "[ -s docs/x.txt ]"), true),
            ("src/value.txt".to_string(), "bad".to_string(), false),
            ("src/other.txt".to_string(), "bad".to_string(), false),
            ("docs/x.txt".to_string(), "doc0".to_string(), false),
        ];
        files.extend(extra);
        git(&repo, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&repo, &["config", k, v]);
        }
        for (p, b, x) in &files {
            write_file(&repo.join(p), b, *x);
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c0"]);
        git(&other, &["init", "-q"]);
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&other, &["config", k, v]);
        }
        write_file(&other.join("o.txt"), "other repo", false);
        git(&other, &["add", "-A"]);
        git(&other, &["commit", "-q", "-m", "other"]);
        let c0 = git(&repo, &["rev-parse", "HEAD"]);
        let cfg = t.0.join("settings.yaml");
        fs::write(&cfg, settings_text(3600, 3600, 604800)).unwrap();
        Fx { repo: repo.canonicalize().unwrap(), other: other.canonicalize().unwrap(), marker: marker_dir.join("m.log"), res: res.canonicalize().unwrap(), caps: caps.canonicalize().unwrap(), cfg, c0, t }
    }

    /// The fixture whose `src` gate waits for `$RELEASE` before judging `src/value.txt` (timeout 30 s), so a row can hold one
    /// run open inside its checks.
    fn holding() -> Fx {
        let test = "while [ ! -e \"$RELEASE\" ]; do sleep 0.05; done\n[ \"$(cat src/value.txt)\" = ok ]";
        Fx::with(r#"{"src":["h"],"docs":["p"]}"#, gate_files("h", test, false, 30000))
    }

    /// A head-only gate `w` on `docs` that waits for `$RELEASE` then fails (timeout 30 s).
    fn waiting_docs_fail() -> Fx {
        let test = "while [ ! -e \"$RELEASE\" ]; do sleep 0.05; done\nfalse";
        Fx::with(r#"{"src":["t","u"],"docs":["w"]}"#, gate_files("w", test, false, 30000))
    }

    /// A head-only gate `w` on `docs` that waits for `$RELEASE` then passes (timeout 30 s).
    fn waiting_docs_pass() -> Fx {
        let test = "while [ ! -e \"$RELEASE\" ]; do sleep 0.05; done\ntrue";
        Fx::with(r#"{"src":["t","u"],"docs":["w"]}"#, gate_files("w", test, false, 30000))
    }

    /// A head-only gate `k` on `docs` that SIGKILLs the gate process on its first run (and fails on later runs).
    fn killing() -> Fx {
        let test = "if [ ! -e \"$ONCE\" ]; then : > \"$ONCE\"; kill -9 $PPID; sleep 5; fi\nfalse";
        Fx::with(r#"{"src":["t","u"],"docs":["k"]}"#, gate_files("k", test, false, 30000))
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

    fn rec(&self, id: &str) -> String {
        fs::read_to_string(self.rec_file(id)).unwrap_or_else(|_| panic!("no record for {id}"))
    }

    fn budget_file(&self, id: &str) -> PathBuf {
        self.caps.join(format!("{id}.budget"))
    }

    fn budget_text(&self, id: &str) -> String {
        fs::read_to_string(self.budget_file(id)).unwrap_or_else(|_| panic!("no budget file for {id}"))
    }

    fn events(&self) -> Vec<String> {
        fs::read_to_string(self.caps.join("events.jsonl")).map(|s| s.lines().map(|l| l.to_string()).collect()).unwrap_or_default()
    }

    fn caps_listing(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.caps).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }
}

fn budget_canon(id: &str, size: &str, started_ms: u64) -> String {
    format!("{{\"v\":1,\"ticket\":\"{id}\",\"repo_id\":\"r\",\"size\":\"{size}\",\"started_ms\":{started_ms}}}\n")
}

fn rec_canon(id: &str, fix_rounds: u8, in_flight: bool) -> String {
    format!("{{\"v\":1,\"ticket\":\"{id}\",\"repo_id\":\"r\",\"fix_rounds\":{fix_rounds},\"in_flight\":{in_flight}}}\n")
}

fn streak_canon(id: &str, sig: &str, head: &str, count: u8) -> String {
    format!("{{\"v\":1,\"ticket\":\"{id}\",\"repo_id\":\"r\",\"sig\":\"{sig}\",\"head\":\"{head}\",\"count\":{count}}}\n")
}

fn plant_record(fx: &Fx, id: &str, fix_rounds: u8, in_flight: bool) {
    fs::write(fx.rec_file(id), rec_canon(id, fix_rounds, in_flight)).unwrap();
}

fn plant_budget(fx: &Fx, id: &str, size: &str, started_ms: u64) {
    fs::write(fx.budget_file(id), budget_canon(id, size, started_ms)).unwrap();
}

/// The test's own clock (planting only; never a value the gate reads).
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// `started_ms` read out of a canonical budget line.
fn started_ms_of(text: &str) -> u64 {
    let i = text.find("\"started_ms\":").expect("a canonical budget line") + "\"started_ms\":".len();
    text[i..].trim_end_matches(['}', '\n']).parse().expect("digits")
}

fn ticket_json(repo_id: &str, id: &str, size: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"repo_id\":\"{repo_id}\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"{size}\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}"
    )
}

fn ticket_file(fx: &Fx, id: &str, size: &str) -> PathBuf {
    let d = fx.t.0.join("tickets");
    fs::create_dir_all(&d).unwrap();
    let f = d.join(format!("t{}.json", SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::write(&f, ticket_json("r", id, size)).unwrap();
    f
}

/// The gate child for `id`/`size`: `--config` names the fixture's settings file (its default source), the run comes from the
/// unrelated repo, the repo-redirecting and run-id names are stripped (a refusal event self-issues its run id), and the run
/// identity of an event is set. `env` pairs are applied LAST, so a row may override anything, including `HARNESS_CONFIG`.
fn gate_child(fx: &Fx, id: &str, size: &str, env: &[(&str, &str)]) -> Command {
    let tf = ticket_file(fx, id, size);
    let mut c = Command::new(BIN);
    c.arg("ticket")
        .arg("gate")
        .arg(tf.to_str().unwrap())
        .arg("--repo-map")
        .arg(format!("r={}", fx.repo.to_str().unwrap()))
        .arg("--config")
        .arg(fx.cfg.to_str().unwrap())
        .arg("--base")
        .arg(&fx.c0)
        .arg("--result-dir")
        .arg(fx.res.to_str().unwrap())
        .arg("--caps-dir")
        .arg(fx.caps.to_str().unwrap())
        .current_dir(&fx.other)
        .stdin(Stdio::null());
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "HARNESS_RUN_ID", "HARNESS_CONFIG"] {
        c.env_remove(k);
    }
    // The run identity of a refusal event (v2): the role and model of the gate run, never defaulted by the harness.
    c.env("HARNESS_ROLE", "executor").env("HARNESS_MODEL", "m-1");
    c.env("GATE_MARKER", fx.marker.to_str().unwrap());
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

/// The same child with the settings source given explicitly: `None` passes NO `--config` flag (the row's env decides),
/// `Some("")` passes an empty flag value, anything else is the flag's file.
fn gate_child_src(fx: &Fx, id: &str, size: &str, config: Option<&str>, env: &[(&str, &str)]) -> Command {
    let tf = ticket_file(fx, id, size);
    let mut c = Command::new(BIN);
    c.arg("ticket").arg("gate").arg(tf.to_str().unwrap()).arg("--repo-map").arg(format!("r={}", fx.repo.to_str().unwrap()));
    if let Some(cfg) = config {
        c.arg("--config").arg(cfg);
    }
    c.arg("--base").arg(&fx.c0).arg("--result-dir").arg(fx.res.to_str().unwrap()).arg("--caps-dir").arg(fx.caps.to_str().unwrap()).current_dir(&fx.other).stdin(Stdio::null());
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "HARNESS_RUN_ID", "HARNESS_CONFIG"] {
        c.env_remove(k);
    }
    // The run identity of a refusal event (v2): the role and model of the gate run, never defaulted by the harness.
    c.env("HARNESS_ROLE", "executor").env("HARNESS_MODEL", "m-1");
    c.env("GATE_MARKER", fx.marker.to_str().unwrap());
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

/// The default run: ticket `T-1` of size `M`, `--config` naming the fixture's settings file.
fn gate(fx: &Fx) -> Output {
    gate_child(fx, "T-1", "M", &[]).output().unwrap()
}

fn gate_id(fx: &Fx, id: &str, size: &str, env: &[(&str, &str)]) -> Output {
    gate_child(fx, id, size, env).output().unwrap()
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

/// A FAIL that is not a refusal and not an error: exit 1 and the fixed FAIL line.
fn failed(o: &Output, id: &str, head: &str) {
    assert_eq!(code(o), 1, "stderr: {}", err(o));
    assert_eq!(out(o), format!("ticket={id} head={head} result=fail done=refused\n"), "stderr: {}", err(o));
}

fn refused_line(id: &str, head: &str, reason: &str) -> String {
    format!("ticket={id} head={head} result=refused done=refused reason={reason}\n")
}

/// A refusal: exit 1, the exact stdout line, the stderr line, never `done=allowed`.
fn refused(o: &Output, id: &str, head: &str, reason: &str) {
    assert_eq!(code(o), 1, "stderr: {}", err(o));
    assert_eq!(out(o), refused_line(id, head, reason), "stderr: {}", err(o));
    assert!(!leaks(o, "done=allowed"));
    assert!(err(o).contains(&format!("gate REFUSED reason={reason}")), "{}", err(o));
}

fn budget_refused(o: &Output, id: &str, head: &str) {
    refused(o, id, head, "wall-time-budget");
}

/// Poll `cond` until it holds; 60 s deadline, never a fixed sleep.
fn wait_for(what: &str, cond: &dyn Fn() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < Duration::from_secs(60), "timed out (60 s) waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait until the test clock reads past `started_ms + 2000` (the B8 rows' one real wait, bounded at 60 s).
fn wait_past_limit(fx: &Fx, id: &str) {
    let start = started_ms_of(&fx.budget_text(id));
    wait_for("the clock to pass the 2 s limit", &|| now_ms() >= start + 2000);
}

// B1: inside the budget a run is untouched: it executes, FAILs normally with no budget hint and no event, and afterwards the
// meter exists, pinned to the ticket's size, started inside the run's own wall window. Twin: the same ticket planted at
// `now - 3500 s` (B2's inside-the-budget plant) still EXECUTES and can PASS. Kills: never writing the start.
#[test]
fn b01_inside_budget_untouched() {
    let fx = Fx::new();
    let c1 = fx.commit(&[("src/value.txt", "bad2")]);
    let t0 = now_ms();
    let o = gate(&fx);
    failed(&o, "T-1", &c1);
    assert!(!err(&o).contains(BUDGET_HINT), "no budget hint inside the budget: {}", err(&o));
    assert!(fx.ran("t") >= 1, "the check ran (the marker grew)");
    assert!(!fx.caps.join("events.jsonl").exists(), "no refusal, so no event");
    let text = fx.budget_text("T-1");
    assert!(text.contains("\"size\":\"M\""), "the size is pinned from the ticket: {text}");
    let started = started_ms_of(&text);
    assert!(started >= t0.saturating_sub(2_000) && started <= now_ms() + 2_000, "started_ms inside the run's wall window: {started}");
    // Twin: B2's plant one second inside the limit, on a head that PASSES — the run executes and allows Done.
    let fx2 = Fx::new();
    let c2 = fx2.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx2, "T-1", "M", now_ms() - 3_500_000);
    let o = gate(&fx2);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(fx2.ran("t") >= 1, "twin: the inside-the-budget run executes");
    assert_eq!(out(&o), format!("ticket=T-1 head={c2} result=pass done=allowed\n"));
}

// B2: a spent ticket is refused before anything runs, on a head that would PASS. Marker, record, streak, budget and result
// bytes are all unchanged, and exactly one budget_exceeded event is written. Twin: the same plant one second inside the limit
// executes. Kills: refusing after running the checks; paying a round on a refusal; exit 0 or 2.
#[test]
fn b02_spent_is_refused_before_anything_runs() {
    let fx = Fx::new();
    plant_record(&fx, "T-1", 1, false);
    let head = fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx, "T-1", "M", now_ms() - 3_601_000);
    let (budget, record) = (fx.budget_text("T-1"), fx.rec("T-1"));
    let o = gate(&fx);
    budget_refused(&o, "T-1", &head);
    assert!(fx.markers().is_empty(), "nothing ran");
    assert!(!fx.result_file("T-1").exists(), "no result file");
    assert_eq!(fx.rec("T-1"), record, "no round was paid for the refusal");
    assert_eq!(fx.budget_text("T-1"), budget, "the budget file is untouched");
    assert!(!fx.caps.join("T-1.replan").exists(), "no streak is created");
    assert_eq!(fx.events().iter().filter(|l| l.contains("\"budget_exceeded\"")).count(), 1, "exactly one budget_exceeded event");
    assert_eq!(fx.events().len(), 2, "its run_issued line and the event");
    assert!(!leaks(&o, fx.caps.to_str().unwrap()) && !leaks(&o, fx.res.to_str().unwrap()), "no folder is echoed");
    // Twin: the same plant at now - 3500 s executes and PASSes.
    let fx2 = Fx::new();
    plant_record(&fx2, "T-1", 1, false);
    fx2.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx2, "T-1", "M", now_ms() - 3_500_000);
    assert_eq!(code(&gate(&fx2)), 0, "twin: one second inside the limit executes");
}

// B3: the refusal event is a valid v2 line: `events verify` accepts the log, the kind is budget_exceeded, the ticket is the
// lowercased id, the repo and reason are fixed, ts_ms is real, role/model come from the child env and the run id is self-issued
// (no HARNESS_RUN_ID) and shared with the run_issued line. A second refused call appends another run. Twin: a ticket inside
// the budget writes no log at all. Kills: kind escalation; a raw uppercase ticket; an empty run_id.
#[test]
fn b03_the_event_is_valid_v2() {
    // Twin: an inside-the-budget run of a fresh fixture writes no log.
    let plain = Fx::new();
    plain.commit(&[("src/value.txt", "bad2")]);
    assert_eq!(code(&gate(&plain)), 1);
    assert!(!plain.caps.join("events.jsonl").exists(), "twin: no refusal, no log");
    let fx = Fx::new();
    plant_record(&fx, "EX-1", 1, false);
    let head = fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx, "EX-1", "M", now_ms() - 3_601_000);
    let o = gate_id(&fx, "EX-1", "M", &[]);
    budget_refused(&o, "EX-1", &head);
    let log = fx.caps.join("events.jsonl");
    let v = Command::new(BIN).args(["events", "verify", "--log", log.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&v), 0, "{}", err(&v));
    let l = Command::new(BIN).args(["events", "list", "--log", log.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&l), 0);
    for needle in ["budget_exceeded", "ex-1", "wall-time-budget"] {
        assert!(out(&l).contains(needle), "events list lacks `{needle}`: {}", out(&l));
    }
    let read = events::read_log(&log).unwrap();
    assert!(read.is_clean());
    assert_eq!(read.events.len(), 2, "the self-issued run_issued line plus the event");
    assert_eq!(read.events[0].kind.as_str(), "run_issued");
    let e = &read.events[1];
    assert_eq!((e.kind.as_str(), e.ticket.as_str(), e.repo.as_str(), e.reason.as_str()), ("budget_exceeded", "ex-1", "r", "wall-time-budget"));
    assert!(e.ts_ms > 0);
    assert_eq!((e.role.as_str(), e.model.as_str()), ("executor", "m-1"), "the identity of the gate child");
    assert!(!e.run_id.is_empty(), "a self-issued run id");
    assert_eq!(read.events[0].run_id, e.run_id, "the run_issued line and the event share the run");
    // A second refused call appends another run (run_issued + event).
    assert_eq!(code(&gate_id(&fx, "EX-1", "M", &[])), 1);
    assert_eq!(fx.events().len(), 4);
}

// B4: the settings file alone flips the same ticket: planted at now - 120 s, `m_wall_seconds: 3600` executes; editing ONLY that
// line to 60 refuses the same head. Kills: a cached or compiled-in limit.
#[test]
fn b04_config_alone_flips_it() {
    let fx = Fx::new();
    let head = fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx, "T-1", "M", now_ms() - 120_000);
    let o = gate(&fx);
    assert_eq!(code(&o), 0, "twin: the 3600 run executes: {}", err(&o));
    fs::write(&fx.cfg, settings_text(3600, 60, 604800)).unwrap();
    let o = gate(&fx);
    budget_refused(&o, "T-1", &head);
}

// B5: the ticket's size picks the limit: S 60, M 3600, L in the days; an S ticket planted at now - 120 s is refused, the same
// plant on an M ticket executes. Kills: ignoring the size (always using M).
#[test]
fn b05_size_picks_the_limit() {
    let fx = Fx::new();
    let head = fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    fs::write(&fx.cfg, settings_text(60, 3600, 604800)).unwrap();
    plant_budget(&fx, "S-1", "S", now_ms() - 120_000);
    let o = gate_id(&fx, "S-1", "S", &[]);
    budget_refused(&o, "S-1", &head);
    // Twin: the same plant for an M ticket executes.
    plant_budget(&fx, "T-1", "M", now_ms() - 120_000);
    let o = gate_id(&fx, "T-1", "M", &[]);
    assert_eq!(code(&o), 0, "twin: the M ticket is inside its own limit: {}", err(&o));
}

// B6: the size is pinned at the meter's start: the budget file says M and the ticket file now says L -> exit 2, nothing ran,
// no event, every byte unchanged. Twin: the matching size executes. Kills: re-reading the size from the ticket; rewriting the
// file.
#[test]
fn b06_size_is_pinned() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx, "T-1", "M", now_ms() - 3_500_000);
    let budget = fx.budget_text("T-1");
    let o = gate_id(&fx, "T-1", "L", &[]);
    errored(&o);
    assert!(err(&o).contains("size"), "the error names the pinned size: {}", err(&o));
    assert!(fx.markers().is_empty(), "nothing ran");
    assert_eq!(fx.budget_text("T-1"), budget, "the budget bytes are unchanged");
    assert!(!fx.caps.join("events.jsonl").exists(), "no event");
    assert!(!fx.result_file("T-1").exists(), "no result file");
    // Twin: the matching size executes.
    let o = gate_id(&fx, "T-1", "M", &[]);
    assert_eq!(code(&o), 0, "twin: a matching size executes: {}", err(&o));
}

// B7: a clock behind the start is exit 2 (fail closed, never "elapsed 0", never a refusal with an event): planted at
// now + 3600 s nothing runs, no round is paid, no event, the bytes are unchanged. Twin: now - 1 s executes. Kills: treating it
// as elapsed 0; refusing with an event.
#[test]
fn b07_a_clock_behind_the_start_fails_closed() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx, "T-1", "M", now_ms() + 3_600_000);
    let budget = fx.budget_text("T-1");
    let o = gate(&fx);
    errored(&o);
    assert!(err(&o).contains("clock"), "the error names the clock: {}", err(&o));
    assert!(fx.markers().is_empty(), "nothing ran");
    assert!(!fx.rec_file("T-1").exists(), "no round was paid");
    assert!(!fx.caps.join("events.jsonl").exists(), "no event (an event records a refusal, this is an error)");
    assert_eq!(fx.budget_text("T-1"), budget, "the budget bytes are unchanged");
    // Twin: now - 1 s executes.
    let fx2 = Fx::new();
    fx2.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx2, "T-1", "M", now_ms() - 1_000);
    assert_eq!(code(&gate(&fx2)), 0, "twin: a one-second-old start executes");
}

// B8a (the one bounded real wait; no plant, so point B cannot race): `m_wall_seconds: 2`, no budget file; the first run starts
// the meter at its pay point and its FAIL check waits on a release file; the test reads `started_ms` back and releases only
// past `started_ms + 2000`: exit 1, the budget hint, no event; the next run is refused wall-time-budget. Twin: the same run
// under `m_wall_seconds: 3600` released at once shows no hint. Kills: a hint on PASS (B8b); an event on the hinted run; never
// hinting; both hints on a latching run (B8c).
#[test]
fn b08a_a_fail_ending_spent_prints_the_budget_hint() {
    let hold = Tmp::new();
    let release = hold.0.join("release");
    let fx = Fx::waiting_docs_fail();
    fs::write(&fx.cfg, settings_text(3600, 2, 604800)).unwrap();
    fx.commit(&[("docs/x.txt", "doc1")]);
    let child = gate_child(&fx, "T-1", "M", &[("RELEASE", release.to_str().unwrap())])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for("the meter to start (the budget file to exist)", &|| fx.budget_file("T-1").exists());
    wait_past_limit(&fx, "T-1");
    fs::write(&release, "").unwrap();
    let o = child.wait_with_output().unwrap();
    failed(&o, "T-1", &fx.head());
    assert!(err(&o).contains(BUDGET_HINT), "the budget hint: {}", err(&o));
    assert!(!fx.caps.join("events.jsonl").exists(), "no event on the hinted run");
    // The next run is refused with budget_exceeded.
    let o = gate_child(&fx, "T-1", "M", &[("RELEASE", release.to_str().unwrap())]).output().unwrap();
    budget_refused(&o, "T-1", &fx.head());
    // Twin: the same held-open FAIL under m_wall_seconds 3600, released at once, shows no hint.
    let fx2 = Fx::waiting_docs_fail();
    fx2.commit(&[("docs/x.txt", "doc1")]);
    let rel2 = hold.0.join("release2");
    fs::write(&rel2, "").unwrap();
    let o = gate_child(&fx2, "T-1", "M", &[("RELEASE", rel2.to_str().unwrap())]).output().unwrap();
    assert_eq!(code(&o), 1, "twin: the run still fails: {}", err(&o));
    assert!(!err(&o).contains(BUDGET_HINT), "twin: a generous limit shows no hint: {}", err(&o));
}

// B8b: the same held-open run on a PASS head: exit 0 and NO hint (a PASS never hints).
#[test]
fn b08b_a_pass_never_hints() {
    let hold = Tmp::new();
    let release = hold.0.join("release");
    let fx = Fx::waiting_docs_pass();
    fs::write(&fx.cfg, settings_text(3600, 2, 604800)).unwrap();
    fx.commit(&[("docs/x.txt", "doc1")]);
    let child = gate_child(&fx, "T-1", "M", &[("RELEASE", release.to_str().unwrap())])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for("the meter to start (the budget file to exist)", &|| fx.budget_file("T-1").exists());
    wait_past_limit(&fx, "T-1");
    fs::write(&release, "").unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 0, "the waiting check passed: {}", err(&o));
    assert!(!err(&o).contains(BUDGET_HINT), "a PASS never hints: {}", err(&o));
    assert!(!fx.caps.join("events.jsonl").exists(), "no event");
}

// B8c: latched AND spent when latched: a FAIL on H1 starts the meter; the budget file is moved out (absent is legal);
// `m_wall_seconds: 2`; a FAIL on H2 with the same failure, released past its own start + 2000: exit 1, the re-plan hint and
// NOT the budget hint; the next run is refused replan-threshold. Kills: both hints on a latching run.
#[test]
fn b08c_a_latching_run_shows_only_the_replan_hint() {
    let hold = Tmp::new();
    let release = hold.0.join("release");
    let fx = Fx::holding();
    let c1 = fx.commit(&[("src/value.txt", "bad2")]);
    fs::write(&release, "").unwrap();
    let o = gate_child(&fx, "T-1", "M", &[("RELEASE", release.to_str().unwrap())]).output().unwrap();
    failed(&o, "T-1", &c1);
    assert!(!err(&o).contains(REPLAN_HINT) && !err(&o).contains(BUDGET_HINT), "run 1 hints nothing: {}", err(&o));
    // The meter is moved out (absent is legal for every pre-rule ticket) and the limit drops to 2 s.
    fs::rename(fx.budget_file("T-1"), hold.0.join("moved.budget")).unwrap();
    fs::write(&fx.cfg, settings_text(3600, 2, 604800)).unwrap();
    let c2 = fx.commit(&[("src/value.txt", "bad3")]);
    fs::remove_file(&release).unwrap();
    let child = gate_child(&fx, "T-1", "M", &[("RELEASE", release.to_str().unwrap())])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for("the meter to restart (a fresh budget file to exist)", &|| fx.budget_file("T-1").exists());
    wait_past_limit(&fx, "T-1");
    fs::write(&release, "").unwrap();
    let o = child.wait_with_output().unwrap();
    failed(&o, "T-1", &c2);
    assert!(err(&o).contains(REPLAN_HINT), "the 2nd identical failure latches: {}", err(&o));
    assert!(!err(&o).contains(BUDGET_HINT), "the re-plan hint wins; the budget line would be false here: {}", err(&o));
    assert!(!fx.caps.join("events.jsonl").exists(), "no event on the hinted run");
    // The next run is refused replan-threshold (the more specific cause), not wall-time-budget.
    let o = gate_child(&fx, "T-1", "M", &[("RELEASE", release.to_str().unwrap())]).output().unwrap();
    refused(&o, "T-1", &c2, "replan-threshold");
}

// B9: order with re-plan: two identical FAILs on two heads latch, AND the budget is planted spent: the next run is refused
// replan-threshold with one replan_required event and NO budget_exceeded. Twin: latched only. Kills: budget beating re-plan;
// two events.
#[test]
fn b09_replan_wins_over_budget() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2")]);
    assert_eq!(code(&gate(&fx)), 1);
    let c2 = fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate(&fx);
    failed(&o, "T-1", &c2);
    assert!(err(&o).contains(REPLAN_HINT), "the 2nd identical failure latches: {}", err(&o));
    // Now the budget is ALSO spent.
    plant_budget(&fx, "T-1", "M", now_ms() - 3_601_000);
    let c3 = fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    let o = gate(&fx);
    refused(&o, "T-1", &c3, "replan-threshold");
    let ev = fx.events();
    assert_eq!(ev.iter().filter(|l| l.contains("\"replan_required\"")).count(), 1, "exactly one replan event");
    assert!(ev.iter().all(|l| !l.contains("\"budget_exceeded\"")), "no budget event when re-plan wins");
    // Twin: latched only (no spent plant) is the same refusal.
    let fx2 = Fx::new();
    fx2.commit(&[("src/value.txt", "bad2")]);
    assert_eq!(code(&gate(&fx2)), 1);
    fx2.commit(&[("src/value.txt", "bad3")]);
    assert_eq!(code(&gate(&fx2)), 1);
    fx2.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    let o = gate(&fx2);
    refused(&o, "T-1", &fx2.head(), "replan-threshold");
}

// B10: order with the cap: a record at fix_rounds=4 AND a planted spent budget -> refused wall-time-budget with one
// budget_exceeded event and no escalation. Refused runs never start the meter: fix_rounds=4 with NO budget file is refused
// fix-round-cap and afterwards no budget file exists. Twin: the cap only. Kills: the cap beating the budget; two events;
// writing the meter on a refused run.
#[test]
fn b10_budget_wins_over_the_cap_and_refusals_never_start_the_meter() {
    let fx = Fx::new();
    plant_record(&fx, "T-1", 4, false);
    let head = fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx, "T-1", "M", now_ms() - 3_601_000);
    let o = gate(&fx);
    budget_refused(&o, "T-1", &head);
    let ev = fx.events();
    assert_eq!(ev.iter().filter(|l| l.contains("\"budget_exceeded\"")).count(), 1, "exactly one budget event");
    assert!(ev.iter().all(|l| !l.contains("\"escalation\"")), "no escalation event when the budget wins");
    // Twin: the cap only (no budget file) is refused fix-round-cap AND starts no meter.
    plant_record(&fx, "T-2", 4, false);
    let o = gate_id(&fx, "T-2", "M", &[]);
    refused(&o, "T-2", &head, "fix-round-cap");
    assert!(!fx.budget_file("T-2").exists(), "a refused run never starts the meter");
}

// B11: the settings source. No `--config` and no `HARNESS_CONFIG`: exit 2 `no config configured`, nothing ran, nothing was
// created in the caps folder. `HARNESS_CONFIG` alone works. Flag and env set to different files: the flag's limit applies.
// Empty `--config ""` and empty `HARNESS_CONFIG`: exit 2, no fallback. Kills: defaulting to the example; env beating the flag;
// an empty value falling back.
#[test]
fn b11_the_settings_source_resolves_flag_then_env_fail_closed() {
    // No source at all: exit 2 before anything runs, nothing created in the caps folder.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    let o = gate_child_src(&fx, "T-1", "M", None, &[]).output().unwrap();
    errored(&o);
    assert!(err(&o).contains("no config configured"), "{}", err(&o));
    assert!(fx.markers().is_empty(), "nothing ran");
    assert!(fx.caps_listing().is_empty(), "nothing was created in the caps folder: {:?}", fx.caps_listing());
    assert!(!fx.result_file("T-1").exists(), "no result file");
    // `HARNESS_CONFIG` alone works.
    let o = gate_child_src(&fx, "T-2", "M", None, &[("HARNESS_CONFIG", fx.cfg.to_str().unwrap())]).output().unwrap();
    assert_eq!(code(&o), 0, "env alone works: {}", err(&o));
    // The flag's limit applies when both name different files: the generous flag (m 3600) beats the tight env (m 1) on a
    // meter planted two seconds ago.
    let tight = fx.t.0.join("tight.yaml");
    fs::write(&tight, settings_text(3600, 1, 604800)).unwrap();
    plant_budget(&fx, "T-3", "M", now_ms() - 2_000);
    let o = gate_child(&fx, "T-3", "M", &[("HARNESS_CONFIG", tight.to_str().unwrap())]).output().unwrap();
    assert_eq!(code(&o), 0, "the generous flag beats the tight env: {}", err(&o));
    // Twin: the tight file alone (no flag) refuses the same plant.
    plant_budget(&fx, "T-4", "M", now_ms() - 2_000);
    let o = gate_child_src(&fx, "T-4", "M", None, &[("HARNESS_CONFIG", tight.to_str().unwrap())]).output().unwrap();
    budget_refused(&o, "T-4", &fx.head());
    // An empty flag value is refused and never falls back to the env file.
    let o = gate_child_src(&fx, "T-5", "M", Some(""), &[("HARNESS_CONFIG", fx.cfg.to_str().unwrap())]).output().unwrap();
    errored(&o);
    assert!(err(&o).contains("the config source is empty"), "{}", err(&o));
    // An empty env value is refused and never falls back to any default.
    let o = gate_child_src(&fx, "T-6", "M", None, &[("HARNESS_CONFIG", "")]).output().unwrap();
    errored(&o);
    assert!(err(&o).contains("the config source is empty"), "{}", err(&o));
}

// B12: a bad settings file (a missing v4 key, `m_wall_seconds: 0`, a tab, a directory, over 1 MiB) is exit 2 with nothing run,
// and stderr never carries the settings path or any value from the file. Kills: a lenient load; printing the path.
#[test]
fn b12_a_bad_settings_file_stops_the_run_without_echoing_it() {
    let good = settings_text(3600, 3600, 604800);
    let big = format!("# {}\n{}", "x".repeat(1024 * 1024 + 64), good);
    let variants: Vec<(&str, String)> = vec![
        ("missing key", good.replace("  l_tokens: 1000000\n", "")),
        ("a zero limit", good.replace("  m_wall_seconds: 3600", "  m_wall_seconds: 0")),
        ("a tab", good.replace("\n  m_wall_seconds: 3600", "\n\tm_wall_seconds: 3600")),
        ("over 1 MiB", big),
    ];
    for (what, text) in &variants {
        let fx = Fx::new();
        fx.commit(&[("src/value.txt", "bad2")]);
        fs::write(&fx.cfg, text).unwrap();
        let o = gate(&fx);
        errored(&o);
        assert!(fx.markers().is_empty(), "{what}: nothing ran");
        assert!(!fx.rec_file("T-1").exists(), "{what}: no round was paid");
        assert!(err(&o).starts_with("harness: error: config:"), "{what}: the loader's own refusal: {}", err(&o));
        assert!(!leaks(&o, fx.cfg.to_str().unwrap()), "{what}: the settings path is never echoed");
        assert!(!leaks(&o, "1000000") && !leaks(&o, "/placeholder/friction-inbox.jsonl"), "{what}: no value from the file is echoed");
    }
    // A directory as the source.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2")]);
    let o = Command::new(BIN)
        .arg("ticket")
        .arg("gate")
        .arg(ticket_file(&fx, "T-1", "M").to_str().unwrap())
        .arg("--repo-map")
        .arg(format!("r={}", fx.repo.to_str().unwrap()))
        .arg("--config")
        .arg(fx.res.to_str().unwrap())
        .arg("--base")
        .arg(&fx.c0)
        .arg("--result-dir")
        .arg(fx.res.to_str().unwrap())
        .arg("--caps-dir")
        .arg(fx.caps.to_str().unwrap())
        .current_dir(&fx.other)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    errored(&o);
    assert!(fx.markers().is_empty(), "a directory source: nothing ran");
    // Twin: the valid file executes (the same fixture, one commit ahead, back on the valid settings).
    let fx2 = Fx::new();
    fx2.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    assert_eq!(code(&gate(&fx2)), 0, "twin: the valid file executes");
}

// B13: a strict budget file at the gate: every part-A bad-decode case planted at `<caps>/<id>.budget` gives exit 2, nothing
// ran, the bytes are unchanged; a copy from T-1 at T-2.budget exits 2; `ex-1.budget` present for EX-1 exits 2; a `<id>.budget`
// link exits 2 and its target is unchanged. Twin: the canonical file executes. Kills: reading a bad file as "not started".
#[test]
fn b13_a_strict_budget_file_is_never_read_as_not_started() {
    let good = budget_canon("T-1", "M", 1_761_955_200_000);
    let bad: Vec<(&str, String)> = vec![
        ("schema v2", good.replace("\"v\":1", "\"v\":2")),
        ("a zero start", good.replace("1761955200000", "0")),
        ("a leading zero", good.replace("1761955200000", "0176195520000")),
        ("no trailing newline", good.trim_end().to_string()),
        ("another repo", good.replace("\"repo_id\":\"r\"", "\"repo_id\":\"q\"")),
        ("junk", "nope\n".to_string()),
    ];
    for (what, text) in &bad {
        let fx = Fx::new();
        fx.commit(&[("src/value.txt", "bad2")]);
        fs::write(fx.budget_file("T-1"), text).unwrap();
        let o = gate(&fx);
        errored(&o);
        assert!(fx.markers().is_empty(), "{what}: nothing ran");
        assert!(!fx.rec_file("T-1").exists(), "{what}: no round was paid");
        assert_eq!(fs::read_to_string(fx.budget_file("T-1")).unwrap(), *text, "{what}: the bytes are unchanged");
        assert!(!fx.caps.join("events.jsonl").exists(), "{what}: no event");
    }
    // A copy from T-1 at T-2.budget: canonical bytes, but ANOTHER ticket's.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2")]);
    fs::write(fx.budget_file("T-2"), budget_canon("T-1", "M", 1_761_955_200_000)).unwrap();
    errored(&gate_id(&fx, "T-2", "M", &[]));
    assert!(fx.markers().is_empty(), "another ticket's canonical budget: nothing ran");
    // A case twin: `ex-1.budget` present for ticket EX-1.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2")]);
    fs::write(fx.caps.join("ex-1.budget"), budget_canon("ex-1", "M", 1_761_955_200_000)).unwrap();
    errored(&gate_id(&fx, "EX-1", "M", &[]));
    assert!(fx.markers().is_empty(), "a case twin of the budget name: nothing ran");
    // A link at the budget name: refused, its target untouched.
    let outside = Tmp::new();
    let victim = outside.0.join("victim");
    fs::write(&victim, "victim bytes").unwrap();
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2")]);
    symlink(&victim, fx.budget_file("T-1")).unwrap();
    errored(&gate(&fx));
    assert!(fx.markers().is_empty(), "a link at the budget name: nothing ran");
    assert_eq!(fs::read_to_string(&victim).unwrap(), "victim bytes", "the link's target is untouched");
    // Twin: the canonical file executes.
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    plant_budget(&fx, "T-1", "M", now_ms() - 1_000);
    assert_eq!(code(&gate(&fx)), 0, "twin: the canonical budget file executes");
}

// B14: the start is written before the pay: a DIRECTORY planted at `<caps>/.<id>.budget.tmp` is met only by the writer at the
// pay point — only the start write fails: exit 2 naming `budget start not written`, the marker is unchanged, no `<id>.rounds`
// is created, no result file. Twin: no planted directory — the run executes and leaves `fix_rounds=1 in_flight=false`. Kills:
// writing the record before the start; ignoring the start-write error; a point-B pre-check of the temp name (the stderr would
// not name the start write).
#[test]
fn b14_the_start_is_written_before_the_round_is_paid() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2")]);
    fs::create_dir(fx.caps.join(".T-1.budget.tmp")).unwrap();
    let o = gate(&fx);
    assert_eq!(code(&o), 2, "stderr: {}", err(&o));
    assert!(err(&o).contains("budget start not written"), "the stderr names the start write: {}", err(&o));
    assert!(fx.markers().is_empty(), "no check ran");
    assert!(!fx.rec_file("T-1").exists(), "no round was paid (no record exists)");
    assert!(!fx.result_file("T-1").exists(), "no result file");
    assert!(!fx.budget_file("T-1").exists(), "no budget file was put in place");
    assert!(!fx.caps.join("events.jsonl").exists(), "no event");
    // Twin: no planted directory — the same run executes and pays its round.
    let fx2 = Fx::new();
    let c1b = fx2.commit(&[("src/value.txt", "bad2")]);
    let o = gate(&fx2);
    failed(&o, "T-1", &c1b);
    assert_eq!(fx2.rec("T-1"), rec_canon("T-1", 1, false), "twin: the round was paid and settled");
    assert!(fx2.budget_file("T-1").exists(), "twin: the meter started");
}

// B15: write once, PASS keeps it: FAIL on H1, PASS on H2, FAIL on H3 — the budget file's bytes are identical after all three.
// Kills: rewriting the start on every run; clearing it on a PASS.
#[test]
fn b15_the_budget_is_written_once_and_a_pass_keeps_it() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2")]);
    assert_eq!(code(&gate(&fx)), 1);
    let bytes = fx.budget_text("T-1");
    fx.commit(&[("src/value.txt", "ok"), ("src/other.txt", "ok")]);
    assert_eq!(code(&gate(&fx)), 0);
    assert_eq!(fx.budget_text("T-1"), bytes, "a PASS does not clear the meter");
    fx.commit(&[("src/value.txt", "bad3")]);
    assert_eq!(code(&gate(&fx)), 1);
    assert_eq!(fx.budget_text("T-1"), bytes, "a later run never rewrites the meter");
}

// B16: a killed run starts the meter and never resets it. (a) No budget file: the first run's gate is SIGKILLed mid-check;
// afterwards `<id>.budget` exists and the record is `in_flight=true`. (b) A planted meter at now - 3500 s: a run killed the
// same way leaves the budget bytes unchanged, and the next run (which folds the dead round) still leaves them unchanged.
// Kills: starting the meter only on a decided verdict; rewriting the start after a dead run.
#[test]
fn b16_a_killed_run_starts_the_meter_and_never_resets_it() {
    // (a) the killed first run starts the meter.
    let fx = Fx::killing();
    let once = fx.t.0.join("once");
    fx.commit(&[("docs/x.txt", "doc1")]);
    let o = gate_id(&fx, "T-1", "M", &[("ONCE", once.to_str().unwrap())]);
    assert!(o.status.code().is_none(), "the gate process was killed: {:?}", o.status);
    assert!(fx.budget_file("T-1").exists(), "the meter started before the check ran");
    assert_eq!(fx.rec("T-1"), rec_canon("T-1", 0, true), "the killed run is in flight");
    // (b) a planted meter survives a killed run and the fold that follows, byte for byte.
    let fx2 = Fx::killing();
    let once2 = fx2.t.0.join("once");
    fx2.commit(&[("docs/x.txt", "doc1")]);
    plant_budget(&fx2, "T-1", "M", now_ms() - 3_500_000);
    let planted = fx2.budget_text("T-1");
    let o = gate_id(&fx2, "T-1", "M", &[("ONCE", once2.to_str().unwrap())]);
    assert!(o.status.code().is_none(), "the gate process was killed: {:?}", o.status);
    assert_eq!(fx2.budget_text("T-1"), planted, "the killed run never rewrites the meter");
    let o = gate_id(&fx2, "T-1", "M", &[("ONCE", once2.to_str().unwrap())]);
    assert_eq!(code(&o), 1, "the next run folds the dead round and runs: {}", err(&o));
    assert_eq!(fx2.rec("T-1"), rec_canon("T-1", 2, false), "the dead run was folded and the live one settled");
    assert_eq!(fx2.budget_text("T-1"), planted, "the folding run leaves the meter unchanged");
}

// B17: backward compatibility at the caps folder: a v1 record and a streak file with NO budget file are read as "not started"
// — the run executes. (The rest of B17 — every merged test passing, no removed `assert` line in the four merged test files —
// is a property of the whole suite, not one row.) Twin: a merged-era caps folder (record plus streak, exactly this shape).
// Kills: refusing an absent budget file.
#[test]
fn b17_a_merged_era_caps_folder_without_a_budget_file_still_runs() {
    let fx = Fx::new();
    let c1 = fx.commit(&[("src/value.txt", "bad2")]);
    plant_record(&fx, "T-1", 1, false);
    fs::write(fx.caps.join("T-1.replan"), streak_canon("T-1", "t:check-failed", &c1, 1)).unwrap();
    assert!(!fx.budget_file("T-1").exists(), "a merged-era caps folder has no budget file");
    let o = gate(&fx);
    failed(&o, "T-1", &c1);
    assert!(fx.ran("t") >= 1, "the run executed");
    assert!(fx.budget_file("T-1").exists(), "this run starts the meter");
    assert_eq!(fx.rec("T-1"), rec_canon("T-1", 2, false), "the round was paid and settled");
}
