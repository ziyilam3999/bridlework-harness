//! Both-ends tests for the T-9.3 event-to-row mapper (ticket 193): `friction_event` maps one event-log line
//! to one friction-inbox row, or to nothing, over a fixed closed list of kinds, with ONE row per (kind,
//! signature) per ISO week. Every check runs the real mapper (and, for the lint and `add` legs, the real
//! binary as a child process reading a real inbox file); every event line and row is a synthetic fixture
//! built at run time. DISCIPLINE: the closed-list test derives its fixtures from the loader's own
//! `Kind::ALL` and `Kind::keys()`, never from a key list typed here, so a new event kind cannot silently
//! pass as mapped or unmapped.
#![cfg(unix)]
use harness::events::{Field, Kind, Record};
use harness::friction::{encode_row, parse_row};
use harness::friction_event::{map_line, map_lines, map_record};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
const PLACEHOLDER: &str = "/placeholder/friction-inbox.jsonl";
/// A synthetic deny word: the ONLY entry of the injected list. No clean fixture may contain it.
const DENY: &str = "zzdenyzz";
/// Two synthetic 64-hex digests (the `brief_sha256` of a launch refusal): same cause, different briefs.
const BRIEF_A: &str = "c51b3f0e7d29a64e8b1c0f3d57a92e6b48cd1a30f5e92c7b6d8014a3f2e5c9b1";
const BRIEF_B: &str = "d51b3f0e7d29a64e8b1c0f3d57a92e6b48cd1a30f5e92c7b6d8014a3f2e5c9b1";
static SEQ: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------- time (every fixture names its own calendar day)

/// Days from the epoch to a civil date (the standard civil-calendar arithmetic): each fixture's ts_ms is
/// built from a calendar date, so the fixture itself carries the week it lands in.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Midnight UTC of a calendar date, in ms.
fn ts(y: i64, m: u32, d: u32) -> u64 {
    days_from_civil(y, m, d) as u64 * 86_400_000
}

/// An arbitrary time of day of a calendar date, in ms.
fn ts_ms(y: i64, m: u32, d: u32, secs: u32, millis: u32) -> u64 {
    ts(y, m, d) + secs as u64 * 1000 + millis as u64
}

// ---------------------------------------------------------------- synthetic event lines (canonical key order)

/// A `replan_required` line: the one kind with a writer today (the cap check).
fn replan_line(t: u64, repo: &str, ticket: &str, reason: &str) -> String {
    format!(
        "{{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":{t},\"run_id\":\"run-1\",\"ticket\":\"{ticket}\",\"repo\":\"{repo}\",\"reason\":\"{reason}\",\"role\":\"executor\",\"model\":\"worker\"}}"
    )
}

/// A `launch_refused` line: declared, no writer yet.
fn launch_line(t: u64, role: &str, brief: &str, entry: &str) -> String {
    format!(
        "{{\"v\":2,\"kind\":\"launch_refused\",\"ts_ms\":{t},\"launch_role\":\"{role}\",\"brief_sha256\":\"{brief}\",\"matched_entry\":\"{entry}\"}}"
    )
}

/// Two synthetic 40-hex shas (the `head_sha` a gate judged): same cause, different heads.
const HEAD_A: &str = "aa11bb22cc33dd44ee55ff6600112233445566aa";
const HEAD_B: &str = "bb11bb22cc33dd44ee55ff6600112233445566bb";

/// A `gate_fail` line: declared, no writer yet (the gate runner's writer is a later ticket).
fn gate_fail_line(t: u64, repo: &str, ticket: &str, gate_id: &str, failure_class: &str, head_sha: &str) -> String {
    format!(
        "{{\"v\":2,\"kind\":\"gate_fail\",\"ts_ms\":{t},\"ticket\":\"{ticket}\",\"repo\":\"{repo}\",\"gate_id\":\"{gate_id}\",\"failure_class\":\"{failure_class}\",\"head_sha\":\"{head_sha}\",\"result\":\"fail\"}}"
    )
}

/// The same replan event through the library's door (a `Record`, not a line).
fn replan_record(t: u64, repo: &str, ticket: &str, reason: &str) -> Record {
    Record::build(
        Kind::ReplanRequired,
        t,
        &[
            ("run_id", Field::Str("run-1".to_string())),
            ("ticket", Field::Str(ticket.to_string())),
            ("repo", Field::Str(repo.to_string())),
            ("reason", Field::Str(reason.to_string())),
            ("role", Field::Str("executor".to_string())),
            ("model", Field::Str("worker".to_string())),
        ],
    )
    .unwrap()
}

/// A mapped row, or a panic naming the refusal: the happy path only.
fn mapped(line: &str) -> harness::friction::Row {
    match map_line(line) {
        Ok(Some(row)) => row,
        Ok(None) => panic!("the fixture must map to a row, not to none"),
        Err(e) => panic!("the fixture must map to a row, not be refused: {e}"),
    }
}

// ---------------------------------------------------------------- AC 1-2: the mapped kinds, byte for byte

#[test]
fn mapped_replan_required_row() {
    let line = replan_line(ts(2026, 11, 2), "ai-brain", "193", "replan-threshold");
    let row = mapped(&line);
    assert_eq!(row.tag, "memory", "an event row lives in memory (the catcher is the capturer)");
    assert_eq!(row.ticket, "t-9.3", "an event row never carries the event's own ticket");
    assert_eq!(row.breaks, "");
    assert_eq!(row.kind, "", "an event row is a friction row, the kind default");
    let want = "{\"id\":\"evt.replan_required.8_ai-brain.3_193.16_replan-threshold.2026-w45\",\"date\":\"2026-11-02\",\"ticket\":\"t-9.3\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"event replan_required repo=ai-brain ticket=193 reason=replan-threshold\",\"source\":\"event\"}";
    assert_eq!(encode_row(&row).unwrap(), want);
}

#[test]
fn mapped_launch_refused_row() {
    let line = launch_line(ts(2026, 11, 3), "worker", BRIEF_A, "builder-state-deny");
    let row = mapped(&line);
    let want = "{\"id\":\"evt.launch_refused.6_worker.18_builder-state-deny.2026-w45\",\"date\":\"2026-11-03\",\"ticket\":\"t-9.3\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"event launch_refused launch_role=worker matched_entry=builder-state-deny\",\"source\":\"event\"}";
    assert_eq!(encode_row(&row).unwrap(), want);
    // `brief_sha256` differs per brief while the cause stays the same: the id must not move with it.
    let other = mapped(&launch_line(ts(2026, 11, 4), "worker", BRIEF_B, "builder-state-deny"));
    assert_eq!(other.id, row.id, "the signature is the cause (launch_role, matched_entry), never the brief digest");
}

#[test]
fn mapped_gate_fail_row() {
    let line = gate_fail_line(ts(2026, 11, 2), "bridlework-harness", "199", "rust-test", "suite-fail", HEAD_A);
    let row = mapped(&line);
    let want = "{\"id\":\"evt.gate_fail.18_bridlework-harness.3_199.9_rust-test.10_suite-fail.2026-w45\",\"date\":\"2026-11-02\",\"ticket\":\"t-9.3\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"event gate_fail repo=bridlework-harness ticket=199 gate_id=rust-test failure_class=suite-fail\",\"source\":\"event\"}";
    assert_eq!(encode_row(&row).unwrap(), want);
    // `head_sha` moves with every fix push while the cause stays the same: it is in neither the id nor the text.
    assert!(!row.id.contains(HEAD_A), "head_sha is not part of the row id");
    assert!(!row.text.contains(HEAD_A), "head_sha is not part of the row text");
}

// ---------------------------------------------------------------- AC 3: the mapped rows are real inbox rows

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("fricev-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// A scratch config: the public example with the placeholder inbox path swapped for this scratch inbox.
    fn config(&self) -> PathBuf {
        let text = fs::read_to_string(EXAMPLE).unwrap();
        let text = text.replace(PLACEHOLDER, self.inbox().to_str().unwrap());
        assert!(text.contains(self.inbox().to_str().unwrap()), "the example must carry the placeholder inbox path");
        let f = self.0.join("config.yaml");
        fs::write(&f, text).unwrap();
        f
    }

    fn inbox(&self) -> PathBuf {
        self.0.join("inbox.jsonl")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The child with no config or list variable inherited: every input arrives as a flag or an explicit `.env`.
fn run(args: &[String]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args)
        .env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET")
        .env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .stdin(Stdio::null());
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

#[test]
fn mapped_rows_pass_friction_lint() {
    let t = Tmp::new();
    let rows = map_lines(&[
        &replan_line(ts(2026, 11, 2), "ai-brain", "193", "replan-threshold"),
        &launch_line(ts(2026, 11, 3), "worker", BRIEF_A, "builder-state-deny"),
        &gate_fail_line(ts(2026, 11, 4), "bridlework-harness", "199", "rust-test", "suite-fail", HEAD_A),
    ])
    .unwrap();
    assert_eq!(rows.len(), 3);
    let mut body = String::new();
    for r in &rows {
        body.push_str(&encode_row(r).unwrap());
        body.push('\n');
    }
    fs::write(t.inbox(), body).unwrap();
    // The same leg `tests/friction_cli.rs` runs: the real binary lints the real inbox file.
    let o = run(&[s("friction"), s("lint"), s("--config"), t.config().to_str().unwrap().to_string()]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=3\n");
}

// ---------------------------------------------------------------- AC 4: the closed list covers every kind exactly

/// The value this test uses for one key of one kind. Synthetic: made-up uuids, digests and paths; the loader
/// has never seen them. Any key NOT named here gets a plain-token value, so an envelope change cannot break
/// this file's builds.
fn value_for(kind: Kind, key: &str) -> Field {
    let v = match (kind.as_str(), key) {
        ("session_started", "session_id") => "3f2a9c01-8d4e-4b6f-9a2c-7e5d1c0b4a83",
        ("session_started", "source") => "launch",
        ("unmanaged_session", "session_id") => "1b7e0d4a-2c9f-4e8a-b3d6-5a0f8e2c9d17",
        ("unmanaged_session", "reason") => "no_launch_record",
        ("timed_out", "cause") => "timeout",
        ("timed_out", "checkpoint") => "9f1c4a7be02d53c86b0a1d4e7f2c8b5a3d6e9f1c",
        ("timed_out", "recipe") => "commit_push_then_resume",
        ("timed_out", "handover") => "handover/t-77.md",
        ("timed_out", "session_id") => "6d0c8e2b-4a1f-4c7d-8e3b-2f5a9c0d1e64",
        ("cleanup_result", "result") => "pass",
        ("quarantined", "from") => "tmp/planted-dir",
        ("quarantined", "to") => "quarantine/planted-dir",
        ("quarantined", "reason") => "unmanifested",
        ("human_origin_refused", "target") => "config",
        ("human_origin_refused", "path") => "cfg/harness.conf",
        ("question_class", "size") => "S",
        ("question_class", "risk") => "low",
        ("question_class", "rule_hit") => "t2_only",
        ("gate_fail", "head_sha") => HEAD_A,
        ("gate_fail", "result") => "fail",
        (_, "steps") => return Field::Num(120),
        (_, "soft_limit") => return Field::Num(100),
        (_, "hard_limit") => return Field::Num(300),
        (_, "manifest_left") => return Field::Num(0),
        _ => return Field::Str(format!("value-{key}")),
    };
    Field::Str(v.to_string())
}

/// A good record of any kind through the library's door, its pairs derived from the loader's own key list.
fn good_record(kind: Kind) -> Record {
    let pairs: Vec<(&'static str, Field)> = kind
        .keys()
        .iter()
        .filter(|k| !matches!(**k, "v" | "kind" | "ts_ms"))
        .map(|k| (*k, value_for(kind, k)))
        .collect();
    Record::build(kind, ts(2026, 11, 2), &pairs).unwrap_or_else(|e| panic!("{}: {e}", kind.as_str()))
}

#[test]
fn unmapped_kinds_yield_none() {
    const MAPPED: [&str; 3] = ["replan_required", "launch_refused", "gate_fail"];
    let mut unmapped: Vec<&str> = Vec::new();
    for kind in Kind::ALL {
        if MAPPED.contains(&kind.as_str()) {
            continue;
        }
        unmapped.push(kind.as_str());
        // Through the record door and through the line door: neither may error, neither may file a row.
        let rec = good_record(kind);
        assert!(map_record(&rec).unwrap().is_none(), "{} is not on the closed list: it must map to no row", kind.as_str());
        let line = rec.encode().unwrap();
        let line = line.strip_suffix('\n').expect("encode writes a trailing newline");
        let got = map_line(line).unwrap_or_else(|e| panic!("{}: a valid line must not be refused: {e}", kind.as_str()));
        assert!(got.is_none(), "{} must map to no row through the line door too", kind.as_str());
    }
    // The closed list covers `Kind::ALL` exactly (15 today): nothing maps by accident, nothing is missed.
    let mut covered: Vec<&str> = MAPPED.iter().copied().chain(unmapped.iter().copied()).collect();
    covered.sort_unstable();
    let mut all: Vec<&str> = Kind::ALL.iter().map(|k| k.as_str()).collect();
    all.sort_unstable();
    assert_eq!(covered, all, "mapped + unmapped must cover every kind exactly once");
    assert_eq!(all.len(), 15);
}

// ---------------------------------------------------------------- AC 5-8: the weekly dedup key

#[test]
fn same_cause_same_week_one_row() {
    // Monday 2026-11-02 and Sunday 2026-11-08: one ISO week, one cause.
    let mon = replan_line(ts(2026, 11, 2), "ai-brain", "193", "replan-threshold");
    let sun = replan_line(ts(2026, 11, 8), "ai-brain", "193", "replan-threshold");
    let rows = map_lines(&[&mon, &sun]).unwrap();
    assert_eq!(rows.len(), 1, "the same cause twice in one week files ONE row");
    assert_eq!(rows[0].date, "2026-11-02", "the row is dated the earliest event of the week");
    let fwd: Vec<String> = rows.iter().map(|r| encode_row(r).unwrap()).collect();
    // The same lines fed in reverse order: byte-identical output.
    let rows = map_lines(&[&sun, &mon]).unwrap();
    let rev: Vec<String> = rows.iter().map(|r| encode_row(r).unwrap()).collect();
    assert_eq!(fwd, rev, "input order never changes the written bytes");
}

#[test]
fn gate_fail_same_cause_same_week_one_row() {
    // One ISO week (Mon 2026-11-02 .. Sun 2026-11-08), one cause, two DIFFERENT heads the gate judged.
    let mon = gate_fail_line(ts(2026, 11, 2), "bridlework-harness", "199", "rust-test", "suite-fail", HEAD_A);
    let sun = gate_fail_line(ts(2026, 11, 8), "bridlework-harness", "199", "rust-test", "suite-fail", HEAD_B);
    let rows = map_lines(&[&mon, &sun]).unwrap();
    assert_eq!(rows.len(), 1, "the same gate failing twice in one week files ONE row");
    assert_eq!(rows[0].date, "2026-11-02", "the row is dated the earliest event of the week");
    // The same pair a week apart (Mon 2026-11-09 opens the next week): two rows, the week is part of the key.
    let next = gate_fail_line(ts(2026, 11, 9), "bridlework-harness", "199", "rust-test", "suite-fail", HEAD_A);
    let rows = map_lines(&[&mon, &next]).unwrap();
    assert_eq!(rows.len(), 2, "the same cause in two different weeks files two rows");
    assert_ne!(rows[0].id, rows[1].id);
}

#[test]
fn week_boundary_two_rows() {
    // Sunday 23:59:59.999 UTC of one ISO week and Monday 00:00:00.000 UTC of the next: two rows.
    let sun_last = replan_line(ts_ms(2026, 11, 8, 86399, 999), "ai-brain", "193", "replan-threshold");
    let mon_first = replan_line(ts_ms(2026, 11, 9, 0, 0), "ai-brain", "193", "replan-threshold");
    let rows = map_lines(&[&sun_last, &mon_first]).unwrap();
    assert_eq!(rows.len(), 2, "the same cause in two different weeks files two rows");
    assert_ne!(rows[0].id, rows[1].id, "the week is part of the key: the ids differ");
    // And the two ids end in consecutive week suffixes (the boundary is real, not an accident of leniency).
    let end = |r: &harness::friction::Row| r.id.rsplit('.').next().unwrap().to_string();
    let (a, b) = (end(&rows[0]), end(&rows[1]));
    assert_ne!(a, b, "the two rows carry two different week suffixes: {a} {b}");
}

#[test]
fn iso_year_boundaries() {
    // The week part of the id, straight from a mapped record (rsplit: tokens may contain dots, week suffixes never).
    let suffix = |t: u64| {
        let rec = replan_record(t, "ai-brain", "193", "replan-threshold");
        let row = map_record(&rec).unwrap().expect("the fixture must map");
        row.id.rsplit('.').next().unwrap().to_string()
    };
    // 2026 is a 53-week ISO year: Thursday 2026-12-31 and Sunday 2027-01-03 are both its week 53.
    assert_eq!(suffix(ts(2026, 12, 31)), "2026-w53");
    assert_eq!(suffix(ts(2027, 1, 3)), "2026-w53");
    // Monday 2027-01-04 is week 1 of 2027.
    assert_eq!(suffix(ts(2027, 1, 4)), "2027-w01");
    // Monday 2024-12-30 already belongs to week 1 of 2025.
    assert_eq!(suffix(ts(2024, 12, 30)), "2025-w01");
}

#[test]
fn different_cause_same_week_two_rows() {
    // Two replan events in one week, differing only in ticket: two rows.
    let a = replan_line(ts(2026, 11, 2), "ai-brain", "193", "replan-threshold");
    let b = replan_line(ts(2026, 11, 3), "ai-brain", "194", "replan-threshold");
    let rows = map_lines(&[&a, &b]).unwrap();
    assert_eq!(rows.len(), 2, "a different cause in the same week files its own row");
    assert_ne!(rows[0].id, rows[1].id);

    // The collision class: joining the values UNPREFIXED with '.' gives one string for both pairs, so only
    // the length prefix keeps the ids apart. The fixture's own values prove it is of the class.
    let naive = |repo: &str, ticket: &str| format!("{repo}.{ticket}");
    assert_eq!(naive("a.b", "c"), naive("a", "b.c"), "the fixture must be a real unprefixed-join collision");
    let left = replan_line(ts(2026, 11, 2), "a.b", "c", "replan-threshold");
    let right = replan_line(ts(2026, 11, 3), "a", "b.c", "replan-threshold");
    let rows = map_lines(&[&left, &right]).unwrap();
    assert_eq!(rows.len(), 2, "the length-prefixed key tells the two causes apart");
    assert_ne!(rows[0].id, rows[1].id);
}

// ---------------------------------------------------------------- AC 9: fail closed on a malformed event

#[test]
fn malformed_event_refused() {
    let good = replan_line(ts(2026, 11, 2), "ai-brain", "193", "replan-threshold");
    // Each fixture carries its own bad condition, derived from the good line.
    let torn = "{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":1730".to_string();
    let not_json = "this line is not a json object at all".to_string();
    let unknown_kind = good.replace("replan_required", "nope");
    let missing_key = good.replace(",\"reason\":\"replan-threshold\"", "");
    let wrong_version = good.replace("\"v\":2", "\"v\":1");
    let past_year_9999 = replan_line(ts(10000, 1, 1), "ai-brain", "193", "replan-threshold");
    for (name, line) in [
        ("a torn line", &torn),
        ("a non-JSON line", &not_json),
        ("an unknown kind", &unknown_kind),
        ("a missing key", &missing_key),
        ("a v:1 line", &wrong_version),
        ("a ts_ms past year 9999", &past_year_9999),
    ] {
        match map_line(line) {
            Err(e) => assert!(!e.is_empty(), "{name}: the refusal must say why"),
            Ok(None) => panic!("{name}: must be REFUSED, never silently skipped"),
            Ok(Some(r)) => panic!("{name}: must be REFUSED, never mapped to a row ({})", r.id),
        }
    }
    // The batch helper fails closed on the whole batch wherever the bad line sits, with no rows returned.
    assert!(map_lines(&[&good, &torn]).is_err(), "one bad line fails the whole batch");
    assert!(map_lines(&[&torn, &good]).is_err(), "also when the bad line comes first");
    assert!(map_lines(&[good.as_str(), &missing_key]).is_err());
}

#[test]
fn gate_fail_malformed_refused() {
    let good = gate_fail_line(ts(2026, 11, 2), "bridlework-harness", "199", "rust-test", "suite-fail", HEAD_A);
    // Each fixture carries its own bad condition, derived from the good line.
    let missing_head = good.replace(&format!(",\"head_sha\":\"{HEAD_A}\""), "");
    let short_head = good.replace(HEAD_A, "aa11bb2");
    let result_pass = good.replace("\"result\":\"fail\"", "\"result\":\"pass\"");
    let upper_gate = good.replace("\"gate_id\":\"rust-test\"", "\"gate_id\":\"Rust-Test\"");
    let extra_key = good.replace("\"result\":\"fail\"", "\"result\":\"fail\",\"extra\":\"x\"");
    let t = Tmp::new();
    for (name, line) in [
        ("a missing head_sha", &missing_head),
        ("a 7-hex head_sha", &short_head),
        ("a result of pass", &result_pass),
        ("an uppercase gate_id", &upper_gate),
        ("an extra key", &extra_key),
    ] {
        match map_line(line) {
            Err(e) => assert!(!e.is_empty(), "{name}: the refusal must say why"),
            Ok(None) => panic!("{name}: must be REFUSED, never silently skipped"),
            Ok(Some(r)) => panic!("{name}: must be REFUSED, never mapped to a row ({})", r.id),
        }
        // Wherever the bad line sits in a batch: the whole batch is an error with no rows.
        assert!(map_lines(&[&good, line]).is_err(), "{name}: one bad line fails the whole batch");
        assert!(map_lines(&[line, &good]).is_err(), "{name}: also when the bad line comes first");
        // And the real binary's own reader refuses the same line: `events verify` exits non-zero.
        let log = t.0.join("gate-fail-bad.jsonl");
        fs::write(&log, format!("{line}\n")).unwrap();
        let o = run(&[s("events"), s("verify"), log.to_str().unwrap().to_string()]);
        assert_ne!(code(&o), 0, "{name}: events verify must refuse the line, stdout: {}", out(&o));
    }
}

// ---------------------------------------------------------------- AC 10: the `source` row field

#[test]
fn source_field_contract() {
    // `source: event` parses and re-encodes byte-identical: a real optional field, last in canonical order.
    let with = "{\"id\":\"l-1\",\"date\":\"2026-11-01\",\"ticket\":\"193\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"a hand-written row\",\"source\":\"event\"}";
    let row = parse_row(with).unwrap();
    assert_eq!(encode_row(&row).unwrap(), with);
    // A value outside the closed set is refused naming the field.
    let bad = with.replace("\"event\"", "\"x\"");
    let e = parse_row(&bad).unwrap_err();
    assert!(e.contains("source"), "the refusal must name the field: {e}");
    // A row with no `source` key — every row written before the field existed — re-encodes byte-identical.
    let plain = "{\"id\":\"l-1\",\"date\":\"2026-11-01\",\"ticket\":\"193\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"a hand-written row\"}";
    let row = parse_row(plain).unwrap();
    assert_eq!(encode_row(&row).unwrap(), plain);
}

/// The `--source` flag of `friction add`: the mapper's rows are fileable by hand, and only `event` is legal.
#[test]
fn source_flag_on_add_stores_and_refuses() {
    let t = Tmp::new();
    let cfg = t.config().to_str().unwrap().to_string();
    let add = |id: &str, source: &str| {
        let mut a: Vec<String> = vec![
            s("friction"),
            s("add"),
            s("--config"),
            cfg.clone(),
            s("--id"),
            s(id),
            s("--date"),
            s("2026-11-01"),
            s("--ticket"),
            s("193"),
            s("--where-it-lives"),
            s("memory"),
            s("--text"),
            s("clean synthetic text"),
        ];
        if !source.is_empty() {
            a.push(s("--source"));
            a.push(s(source));
        }
        run(&a)
    };
    // `--source event` stores, and the stored line carries the field.
    let o = add("l-1", "event");
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let inbox = fs::read_to_string(t.inbox()).unwrap();
    assert!(inbox.ends_with(",\"source\":\"event\"}\n"), "the stored row must carry the source: {inbox}");
    // `--source x` is refused naming the field, stdout empty, the inbox byte-identical.
    let before = fs::read(t.inbox()).ok();
    let o = add("l-2", "x");
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("source"), "{}", err(&o));
    assert_eq!(fs::read(t.inbox()).ok(), before, "a refused write leaves the inbox byte-identical");
    // And the stored event-sourced row lints clean through the real binary.
    let o = run(&[s("friction"), s("lint"), s("--config"), cfg]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=1\n");
}
