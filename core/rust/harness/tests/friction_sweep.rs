//! Process-level tests for `harness friction sweep` (ticket 200): the weekly sweep that feeds the T-9.3
//! event-to-row mapper the event log and files its rows through the add path's own checks. Every check runs
//! the real binary as a child process and reads the real inbox file, so the exit-code contract (0 ok, 1 a
//! refusal — a refused sweep leaves the inbox byte-identical, 2 a usage or setup error) and "nothing on
//! stdout when erroring" are exercised end to end. Every event line, row and deny word is synthetic, built
//! at run time; the privacy list is injected on the child only, and no test reads an environment variable.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
const PLACEHOLDER: &str = "/placeholder/friction-inbox.jsonl";
/// A synthetic deny word: the ONLY entry of the injected list. No clean fixture may contain it.
const DENY: &str = "zzdenyzz";
/// The sweep's event-log bound (the same number the refusal names): 4 MiB.
const LOG_CAP: usize = 4 * 1024 * 1024;
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("sweep-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// A scratch config: the public example with the placeholder inbox path swapped for the given inbox.
    fn config_for(&self, inbox: &Path) -> PathBuf {
        let text = fs::read_to_string(EXAMPLE).unwrap();
        let text = text.replace(PLACEHOLDER, inbox.to_str().unwrap());
        assert!(text.contains(inbox.to_str().unwrap()), "the example must carry the placeholder inbox path");
        self.file("config.yaml", text)
    }

    /// The usual scratch config: this scratch inbox.
    fn config(&self) -> PathBuf {
        self.config_for(&self.inbox())
    }

    fn inbox(&self) -> PathBuf {
        self.0.join("inbox.jsonl")
    }

    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let f = self.0.join(name);
        fs::write(&f, body).unwrap();
        f
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The child with no config or list variable inherited: every input arrives as a flag or an explicit `.env`.
fn base() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET");
    c
}

/// The usual child: the synthetic deny list injected as the secret.
fn run(args: &[String]) -> Output {
    let mut c = base();
    c.args(args).env("HARNESS_PRIVACY_LIST_SECRET", DENY).stdin(Stdio::null());
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

fn s(v: &str) -> String {
    v.to_string()
}

// ---------------------------------------------------------------- synthetic event lines (canonical key order)

/// A `replan_required` line (the base v2 shape: the run identity trio plus the six identity keys).
fn replan_line(t: u64, repo: &str, ticket: &str, reason: &str) -> String {
    format!(
        "{{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":{t},\"run_id\":\"run-1\",\"ticket\":\"{ticket}\",\"repo\":\"{repo}\",\"reason\":\"{reason}\",\"role\":\"executor\",\"model\":\"worker\"}}"
    )
}

/// A `launch_refused` line: the brief sha differs per brief while the mapped cause stays the same.
fn launch_line(t: u64, role: &str, brief: &str, entry: &str) -> String {
    format!(
        "{{\"v\":2,\"kind\":\"launch_refused\",\"ts_ms\":{t},\"launch_role\":\"{role}\",\"brief_sha256\":\"{brief}\",\"matched_entry\":\"{entry}\"}}"
    )
}

/// Two synthetic 40-hex shas (the `head_sha` a gate judged): same cause, different heads.
const HEAD_A: &str = "aa11bb22cc33dd44ee55ff6600112233445566aa";

/// A `gate_fail` line.
fn gate_fail_line(t: u64, repo: &str, ticket: &str, gate_id: &str, failure_class: &str, head_sha: &str) -> String {
    format!(
        "{{\"v\":2,\"kind\":\"gate_fail\",\"ts_ms\":{t},\"ticket\":\"{ticket}\",\"repo\":\"{repo}\",\"gate_id\":\"{gate_id}\",\"failure_class\":\"{failure_class}\",\"head_sha\":\"{head_sha}\",\"result\":\"fail\"}}"
    )
}

/// An `escalation` line: a kind OFF the mapper's closed list, so it maps to no row (the filler of the
/// boundary-twin logs, and line 5 of the fixture).
fn escalation_line(t: u64, reason: &str) -> String {
    format!(
        "{{\"v\":2,\"kind\":\"escalation\",\"ts_ms\":{t},\"run_id\":\"run-1\",\"ticket\":\"9\",\"repo\":\"ai-brain\",\"reason\":\"{reason}\",\"role\":\"executor\",\"model\":\"worker\"}}"
    )
}

/// The plan's fixture F, in order: two same-cause replans in W45, a launch refusal, a gate fail, an
/// escalation (off the closed list) and the same replan cause in W46.
fn fixture_lines() -> Vec<String> {
    vec![
        replan_line(1793577600000, "ai-brain", "193", "replan-threshold"),
        replan_line(1793750400000, "ai-brain", "193", "replan-threshold"),
        launch_line(1793664000000, "worker", "aa11bb22cc33dd44ee55ff6600112233445566aa11bb22cc33dd44ee55ff6600", "builder-state-deny"),
        gate_fail_line(1793577600000, "bridlework-harness", "199", "rust-test", "suite-fail", HEAD_A),
        escalation_line(1793577600000, "manual"),
        replan_line(1794182400000, "ai-brain", "193", "replan-threshold"),
    ]
}

fn fixture_log(t: &Tmp) -> PathBuf {
    let body: String = fixture_lines().iter().map(|l| format!("{l}\n")).collect();
    t.file("events.jsonl", body)
}

/// The three W45 rows the plan pins, byte for byte, in id order (each line + newline).
const R45_GATE: &str = "{\"id\":\"evt.gate_fail.18_bridlework-harness.3_199.9_rust-test.10_suite-fail.2026-w45\",\"date\":\"2026-11-02\",\"ticket\":\"t-9.3\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"event gate_fail repo=bridlework-harness ticket=199 gate_id=rust-test failure_class=suite-fail\",\"source\":\"event\",\"at\":\"2026-11-02T00:00:00Z\",\"impact\":\"high\"}";
const R45_LAUNCH: &str = "{\"id\":\"evt.launch_refused.6_worker.18_builder-state-deny.2026-w45\",\"date\":\"2026-11-03\",\"ticket\":\"t-9.3\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"event launch_refused launch_role=worker matched_entry=builder-state-deny\",\"source\":\"event\",\"at\":\"2026-11-03T00:00:00Z\",\"impact\":\"medium\"}";
const R45_REPLAN: &str = "{\"id\":\"evt.replan_required.8_ai-brain.3_193.16_replan-threshold.2026-w45\",\"date\":\"2026-11-02\",\"ticket\":\"t-9.3\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"event replan_required repo=ai-brain ticket=193 reason=replan-threshold\",\"source\":\"event\",\"at\":\"2026-11-02T00:00:00Z\",\"impact\":\"medium\"}";
const R46_REPLAN: &str = "{\"id\":\"evt.replan_required.8_ai-brain.3_193.16_replan-threshold.2026-w46\",\"date\":\"2026-11-09\",\"ticket\":\"t-9.3\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"event replan_required repo=ai-brain ticket=193 reason=replan-threshold\",\"source\":\"event\",\"at\":\"2026-11-09T00:00:00Z\",\"impact\":\"medium\"}";

/// `harness friction sweep` over the given log and week, with this scratch config.
fn sweep(t: &Tmp, log: &Path, week: &str) -> Output {
    let args: Vec<String> = vec![
        s("friction"),
        s("sweep"),
        s("--log"),
        log.to_str().unwrap().to_string(),
        s("--week"),
        s(week),
        s("--config"),
        t.config().to_str().unwrap().to_string(),
    ];
    run(&args)
}

/// The same child with NO privacy list injected: the sweep must fail closed, never file unscanned.
fn sweep_no_list(t: &Tmp, log: &Path, week: &str) -> Output {
    let args: Vec<String> = vec![
        s("friction"),
        s("sweep"),
        s("--log"),
        log.to_str().unwrap().to_string(),
        s("--week"),
        s(week),
        s("--config"),
        t.config().to_str().unwrap().to_string(),
    ];
    let mut c = base();
    c.args(&args).stdin(Stdio::null());
    c.output().unwrap()
}

/// AC-7: no refusal or usage error ever carries the log path, the inbox path, the config path or the
/// scratch folder that contains them all.
fn no_leak(o: &Output, t: &Tmp, extra: &[&str]) {
    let text = format!("{}{}", out(o), err(o));
    assert!(!text.contains(t.0.to_str().unwrap()), "a path leaked: {text}");
    for v in extra {
        assert!(!text.contains(v), "a value leaked: {text}");
    }
}

fn inbox_bytes(t: &Tmp) -> Option<Vec<u8>> {
    fs::read(t.inbox()).ok()
}

// ---------------------------------------------------------------- AC-1..AC-3: filing, idempotence, next week

/// AC-1: the first W45 sweep files exactly the three pinned rows, byte for byte, id order on stdout.
#[test]
fn ac1_files_the_expected_week_rows() {
    let t = Tmp::new();
    let log = fixture_log(&t);
    let o = sweep(&t, &log, "2026-w45");
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let want_out = "stored id=evt.gate_fail.18_bridlework-harness.3_199.9_rust-test.10_suite-fail.2026-w45\nstored id=evt.launch_refused.6_worker.18_builder-state-deny.2026-w45\nstored id=evt.replan_required.8_ai-brain.3_193.16_replan-threshold.2026-w45\nsweep week=2026-w45 filed=3 already=0\n";
    assert_eq!(out(&o), want_out);
    assert_eq!(err(&o), "");
    let want_inbox = format!("{R45_GATE}\n{R45_LAUNCH}\n{R45_REPLAN}\n");
    assert_eq!(String::from_utf8(inbox_bytes(&t).unwrap()).unwrap(), want_inbox);
}

/// AC-2: the same week swept again files nothing (every id is already stored, skipped not refused).
#[test]
fn ac2_rerun_in_the_same_week_files_nothing() {
    let t = Tmp::new();
    let log = fixture_log(&t);
    let first = sweep(&t, &log, "2026-w45");
    assert_eq!(code(&first), 0, "stderr: {}", err(&first));
    let after_first = inbox_bytes(&t).unwrap();
    let second = sweep(&t, &log, "2026-w45");
    assert_eq!(code(&second), 0, "stderr: {}", err(&second));
    assert_eq!(out(&second), "sweep week=2026-w45 filed=0 already=3\n");
    assert_eq!(err(&second), "");
    assert_eq!(inbox_bytes(&t).unwrap(), after_first, "the second run must leave the inbox byte-identical");
}

/// AC-3: the next week's sweep adds only that week's row, after the rows already stored.
#[test]
fn ac3_next_week_adds_only_the_new_rows() {
    let t = Tmp::new();
    let log = fixture_log(&t);
    assert_eq!(code(&sweep(&t, &log, "2026-w45")), 0);
    assert_eq!(code(&sweep(&t, &log, "2026-w45")), 0);
    let o = sweep(&t, &log, "2026-w46");
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "stored id=evt.replan_required.8_ai-brain.3_193.16_replan-threshold.2026-w46\nsweep week=2026-w46 filed=1 already=0\n");
    assert_eq!(err(&o), "");
    let want = format!("{R45_GATE}\n{R45_LAUNCH}\n{R45_REPLAN}\n{R46_REPLAN}\n");
    assert_eq!(String::from_utf8(inbox_bytes(&t).unwrap()).unwrap(), want);
}

/// A week with no mapped events is a clean zero, and it never creates a missing inbox.
#[test]
fn ac1_empty_week_never_creates_the_inbox() {
    let t = Tmp::new();
    let log = fixture_log(&t);
    let o = sweep(&t, &log, "2026-w50");
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "sweep week=2026-w50 filed=0 already=0\n");
    assert!(inbox_bytes(&t).is_none(), "a zero-row sweep must not create the inbox");
}

// ---------------------------------------------------------------- AC-4: a damaged log is refused whole

/// AC-4: a final fragment with no newline is a torn tail at line 7: exit 1, nothing filed, the missing
/// inbox still missing.
#[test]
fn ac4_torn_tail_refused_nothing_filed() {
    let t = Tmp::new();
    let mut body: String = fixture_lines().iter().map(|l| format!("{l}\n")).collect();
    body.push_str("{\"v\":2,\"kind\":\"replan");
    let log = t.file("torn.jsonl", body);
    let o = sweep(&t, &log, "2026-w45");
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: friction refused: the event log has a torn tail at line 7; nothing was filed\nharness: ROWS NOT RECORDED\n");
    assert!(inbox_bytes(&t).is_none(), "a refused sweep must not create the inbox");
    no_leak(&o, &t, &[]);
}

/// AC-4's twin: a complete-but-invalid line at line 3 is corruption, refused whole.
#[test]
fn ac4b_corrupt_line_refused_nothing_filed() {
    let t = Tmp::new();
    let mut lines = fixture_lines();
    // A complete line (newline included) that is not a valid event: the launch_refused key set is missing.
    lines[2] = "{\"v\":2,\"kind\":\"launch_refused\",\"ts_ms\":1793664000000}".to_string();
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    let log = t.file("corrupt.jsonl", body);
    let o = sweep(&t, &log, "2026-w45");
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: friction refused: the event log is corrupt at line 3; nothing was filed\nharness: ROWS NOT RECORDED\n");
    assert!(inbox_bytes(&t).is_none());
    no_leak(&o, &t, &[]);
}

// ---------------------------------------------------------------- AC-5: the log bound, and its boundary twin

/// A clean log of exactly `total` bytes: filler escalation lines (they map to no row), then one final
/// escalation line whose `reason` is padded so the file lands on the exact byte count.
fn padded_log(t: &Tmp, name: &str, total: usize) -> PathBuf {
    assert!(total > LOG_CAP - 4096);
    let filler = escalation_line(1793577600000, "manual");
    let unit = filler.len() + 1;
    // Enough filler that the final line fits under the event reader's own 4096-byte line cap.
    let n = (total - 4096).div_ceil(unit);
    let used = n * unit;
    let last_len = total - used - 1;
    assert!(last_len <= 4095, "the padded line must stay under the event line cap");
    let base = escalation_line(1793664000000, "manual");
    assert!(last_len >= base.len(), "the padded line must at least hold a minimal escalation line");
    let pad = last_len - base.len();
    let last = escalation_line(1793664000000, &format!("manual{}", "a".repeat(pad)));
    assert_eq!(last.len(), last_len);
    let mut body = vec![0u8; 0];
    for _ in 0..n {
        body.extend_from_slice(filler.as_bytes());
        body.push(b'\n');
    }
    body.extend_from_slice(last.as_bytes());
    body.push(b'\n');
    assert_eq!(body.len(), total);
    t.file(name, body)
}

/// AC-5: a log one byte over the 4 MiB cap is refused (exit 2), nothing filed; a clean log of exactly
/// 4194304 bytes is accepted (exit 0, zero rows).
#[test]
fn ac5_oversized_log_refused_and_boundary_twin_accepted() {
    let t = Tmp::new();
    let over = padded_log(&t, "over.jsonl", LOG_CAP + 1);
    let o = sweep(&t, &over, "2026-w45");
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the event log is over the 4194304-byte cap; nothing was filed\n");
    assert!(inbox_bytes(&t).is_none());
    no_leak(&o, &t, &[]);
    // The boundary twin: exactly at the cap is accepted, files nothing (every line is an escalation).
    let exact = padded_log(&t, "exact.jsonl", LOG_CAP);
    let ok = sweep(&t, &exact, "2026-w45");
    assert_eq!(code(&ok), 0, "stderr: {}", err(&ok));
    assert_eq!(out(&ok), "sweep week=2026-w45 filed=0 already=0\n");
    assert!(inbox_bytes(&t).is_none(), "a zero-row sweep must not create the inbox");
}

// ---------------------------------------------------------------- AC-6/AC-7: fail closed elsewhere, no leak

/// AC-6: a missing log is a setup error (2), nothing filed.
#[test]
fn ac6_missing_log_exit_2() {
    let t = Tmp::new();
    let missing = t.0.join("no-such-log.jsonl");
    let o = sweep(&t, &missing, "2026-w45");
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the event log cannot be read\n");
    assert!(inbox_bytes(&t).is_none());
    no_leak(&o, &t, &[]);
}

/// AC-6: a bad or absent --week is a usage error (2) whose value is never echoed; every bad spelling of
/// the grammar is refused.
#[test]
fn ac6_bad_week_refused_without_echo() {
    let t = Tmp::new();
    let log = fixture_log(&t);
    for bad in ["2026-W45", "2026-45", "2026-w4", "26-w45", "2026-w455", "2026w45"] {
        let o = sweep(&t, &log, bad);
        assert_eq!(code(&o), 2, "week {bad:?} must be a usage error");
        assert_eq!(out(&o), "");
        assert!(!err(&o).contains(bad), "the bad week value must never be echoed: {}", err(&o));
        no_leak(&o, &t, &[bad]);
    }
    // An absent --week is the same class of failure.
    let args: Vec<String> = vec![
        s("friction"),
        s("sweep"),
        s("--log"),
        log.to_str().unwrap().to_string(),
        s("--config"),
        t.config().to_str().unwrap().to_string(),
    ];
    let o = run(&args);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert!(inbox_bytes(&t).is_none());
}

/// AC-6: with no privacy list configured the sweep fails closed (2): an unscanned row is never filed.
#[test]
fn ac6_no_privacy_list_exit_2() {
    let t = Tmp::new();
    let log = fixture_log(&t);
    let o = sweep_no_list(&t, &log, "2026-w45");
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("no privacy list configured"));
    assert!(inbox_bytes(&t).is_none());
    no_leak(&o, &t, &[]);
}

/// AC-6: a swept row whose token carries the deny word is refused (1) whole — stdout empty, the inbox
/// byte-identical, and the deny word never on stderr.
#[test]
fn ac6_deny_word_in_a_swept_token_refused() {
    let t = Tmp::new();
    // The deny word is a legal event token (lowercase), so the line itself is valid; the ROW is the hit.
    let body = format!("{}\n", replan_line(1793577600000, DENY, "193", "replan-threshold"));
    let log = t.file("deny.jsonl", body);
    let o = sweep(&t, &log, "2026-w45");
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), "");
    assert!(!err(&o).contains(DENY), "the deny word must never be echoed: {}", err(&o));
    assert!(inbox_bytes(&t).is_none(), "a refused sweep must not create the inbox");
    no_leak(&o, &t, &[DENY]);
}

/// AC-6: an inbox that already has a problem (a torn tail) fails the sweep closed (1), nothing appended.
#[test]
fn ac6_torn_inbox_refuses_the_sweep() {
    let t = Tmp::new();
    let log = fixture_log(&t);
    assert_eq!(code(&sweep(&t, &log, "2026-w45")), 0);
    // Damage the inbox: a fragment with no newline is a torn tail.
    let mut torn = inbox_bytes(&t).unwrap();
    torn.extend_from_slice(b"{\"id\":\"evt.replan");
    fs::write(t.inbox(), &torn).unwrap();
    let o = sweep(&t, &log, "2026-w46");
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), "");
    assert_eq!(inbox_bytes(&t).unwrap(), torn, "a refused sweep must leave the inbox byte-identical");
    no_leak(&o, &t, &[]);
}

/// AC-6's twin: an inbox that already holds a duplicate id fails the sweep closed (1), nothing appended.
#[test]
fn ac6_duplicate_inbox_refuses_the_sweep() {
    let t = Tmp::new();
    let log = fixture_log(&t);
    // A hand-damaged inbox: the same valid row twice is a duplicate id.
    let dup = format!("{R45_REPLAN}\n{R45_REPLAN}\n");
    fs::write(t.inbox(), &dup).unwrap();
    let o = sweep(&t, &log, "2026-w45");
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), "");
    assert_eq!(String::from_utf8(inbox_bytes(&t).unwrap()).unwrap(), dup);
    no_leak(&o, &t, &[]);
}
