//! The nine P1b event kinds (P1B-01): round trip and refusal tests, through the library writer and the real binary's
//! `events list` / `events verify`. DISCIPLINE: every line this file builds is built FROM the loader's declared key
//! list (`Kind::keys()`), never from a key list typed here, so an envelope change (the PRE-B run identity landing in
//! the one shared declaration) cannot silently pass these tests. The values are synthetic and were never shown to the
//! loader: the loader's accept/refuse logic reads only its own table, never a vector from this file.
#![cfg(unix)]
use harness::events::{append_record, read_log, AppendError, Event, Field, Kind, LINE_CAP, Record};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);
/// A synthetic stamp: fixed, so the appended bytes are deterministic.
const TS: u64 = 1731000000123;

/// The nine P1b kinds, derived from the loader's own list (never typed here as a set of names).
fn p1b_kinds() -> Vec<Kind> {
    Kind::ALL.iter().copied().filter(|k| !k.is_base()).collect()
}

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("p1b-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    fn log(&self) -> PathBuf {
        self.0.join("events.jsonl")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
    // The identity contract is not under test here, but an ambient value must never steer a child either way.
    for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL"] {
        c.env_remove(k);
    }
    c.output().unwrap()
}

fn verify(log: &Path) -> Output {
    run(&["events", "verify", "--log", log.to_str().unwrap()])
}

fn list(log: &Path) -> Output {
    run(&["events", "list", "--log", log.to_str().unwrap()])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

// ---------------------------------------------------------------- synthetic values (made up, drawn from the table's shapes)

/// The value this test uses for one key of one kind. Synthetic: made-up uuids, digests and paths; the loader has
/// never seen them. Any key NOT named here (an envelope key a later ticket adds to the one shared declaration, such
/// as `run_id`) gets a plain-token value, so an envelope change cannot break this file's builds.
fn value_for(kind: Kind, key: &str) -> Field {
    let v = match (kind.as_str(), key) {
        ("session_started", "session_id") => "3f2a9c01-8d4e-4b6f-9a2c-7e5d1c0b4a83",
        ("session_started", "source") => "launch",
        ("unmanaged_session", "session_id") => "1b7e0d4a-2c9f-4e8a-b3d6-5a0f8e2c9d17",
        ("unmanaged_session", "ticket") => "t-91",
        ("unmanaged_session", "reason") => "no_launch_record",
        ("launch_refused", "launch_role") => "worker",
        ("launch_refused", "brief_sha256") => "c51b3f0e7d29a64e8b1c0f3d57a92e6b48cd1a30f5e92c7b6d8014a3f2e5c9b1",
        ("launch_refused", "matched_entry") => "builder-state-deny",
        ("timed_out", "ticket") => "t-77",
        ("timed_out", "repo") => "r-2",
        ("timed_out", "cause") => "timeout",
        ("timed_out", "checkpoint") => "9f1c4a7be02d53c86b0a1d4e7f2c8b5a3d6e9f1c",
        ("timed_out", "recipe") => "commit_push_then_resume",
        ("timed_out", "handover") => "handover/t-77.md",
        ("timed_out", "session_id") => "6d0c8e2b-4a1f-4c7d-8e3b-2f5a9c0d1e64",
        ("check_in", "ticket") => "t-77",
        ("check_in", "repo") => "r-2",
        ("step_limit_stop", "ticket") => "t-77",
        ("step_limit_stop", "repo") => "r-2",
        ("cleanup_result", "ticket") => "t-77",
        ("cleanup_result", "repo") => "r-2",
        ("cleanup_result", "result") => "pass",
        ("quarantined", "ticket") => "t-77",
        ("quarantined", "repo") => "r-2",
        ("quarantined", "from") => "tmp/planted-dir",
        ("quarantined", "to") => "quarantine/planted-dir",
        ("quarantined", "reason") => "unmanifested",
        ("human_origin_refused", "target") => "config",
        ("human_origin_refused", "path") => "cfg/harness.conf",
        (_, "steps") => return Field::Num(120),
        (_, "soft_limit") => return Field::Num(100),
        (_, "hard_limit") => return Field::Num(300),
        (_, "manifest_left") => return Field::Num(0),
        _ => return Field::Str(format!("value-{key}")),
    };
    Field::Str(v.to_string())
}

/// The declared pairs of a kind, straight from the loader's key list: every key it declares beyond `v`/`kind`/`ts_ms`,
/// with this test's synthetic value. No key list is typed here.
fn pairs_for(kind: Kind) -> Vec<(&'static str, Field)> {
    kind.keys()
        .iter()
        .filter(|k| !matches!(**k, "v" | "kind" | "ts_ms"))
        .map(|k| (*k, value_for(kind, k)))
        .collect()
}

/// Build a good record through the library's door.
fn good_record(kind: Kind) -> Record {
    Record::build(kind, TS, &pairs_for(kind)).unwrap_or_else(|e| panic!("{}: {e}", kind.as_str()))
}

fn json(v: &Field) -> String {
    match v {
        Field::Str(s) => format!("\"{s}\""),
        Field::Num(n) => n.to_string(),
    }
}

/// A raw line of this kind, built from the SAME derived pairs (so the read-side tests share the loader's key list),
/// with `f` free to drop, replace or add pairs. `v` comes from the kind's declared version.
fn raw_line(kind: Kind, f: impl FnOnce(Vec<(&'static str, Field)>) -> Vec<(&'static str, Field)>) -> String {
    let mut line = format!("{{\"v\":{},\"kind\":\"{}\",\"ts_ms\":{}", kind.version(), kind.as_str(), TS);
    for (k, v) in f(pairs_for(kind)) {
        line.push_str(&format!(",\"{k}\":{}", json(&v)));
    }
    line.push('}');
    line
}

/// Write one line as the whole log and expect `verify` to refuse it at line 1 naming `want`.
fn refused_at_read(kind: Kind, line: &str, want: &str) {
    let t = Tmp::new();
    fs::write(t.log(), format!("{line}\n")).unwrap();
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(1), "{}: {line}: {}", kind.as_str(), err(&v));
    assert!(err(&v).contains("line 1"), "{}: names line 1: {}", kind.as_str(), err(&v));
    assert!(err(&v).contains(want), "{}: names {want:?}: {}", kind.as_str(), err(&v));
    let l = list(&t.log());
    assert_eq!(l.status.code(), Some(1));
    assert_eq!(out(&l), "", "{}: a refused line prints nothing", kind.as_str());
}

/// Expect the library writer to refuse these pairs, naming `want`.
fn refused_at_write(kind: Kind, pairs: &[(&str, Field)], want: &str) {
    match Record::build(kind, TS, pairs) {
        Err(e) => assert!(e.contains(want), "{}: {e}", kind.as_str()),
        Ok(_rec) => panic!("{}: build accepted {pairs:?}", kind.as_str()),
    }
    // A record mutated after building is refused at the door too, and nothing is written. (A pair whose key the kind
    // does not declare cannot even be held in the typed record: `Record::build` is the only door for it.)
    let t = Tmp::new();
    let mut rec = good_record(kind);
    rec.identity.clear();
    let declared: Vec<(&'static str, Field)> = pairs
        .iter()
        .filter_map(|(k, v)| kind.keys().iter().find(|d| **d == *k).map(|d| (*d, v.clone())))
        .collect();
    if declared == pairs_for(kind) {
        // The only defect was a key the record type cannot hold; `build` refusing it is the write refusal.
        assert!(!t.log().exists());
        return;
    }
    rec.payload = declared;
    match append_record(&t.0, &t.log(), &rec) {
        Err(e) => assert!(e.to_string().contains(want), "{}: {e}", kind.as_str()),
        Ok(()) => panic!("{}: append accepted a mutated record", kind.as_str()),
    }
    assert!(!t.log().exists(), "{}: a refused record leaves no file", kind.as_str());
}

/// The valid twin: the line verifies clean.
fn accepted_at_read(kind: Kind, line: &str) {
    let t = Tmp::new();
    fs::write(t.log(), format!("{line}\n")).unwrap();
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(0), "{}: {line}: {}", kind.as_str(), err(&v));
}

// ---------------------------------------------------------------- AC 1: round trip

#[test]
fn ac1_each_p1b_kind_round_trips_byte_identical_through_the_library_writer() {
    for kind in p1b_kinds() {
        let t = Tmp::new();
        let rec = good_record(kind);
        append_record(&t.0, &t.log(), &rec).unwrap();
        let bytes = fs::read(t.log()).unwrap();
        let line = rec.encode().unwrap();
        assert_eq!(bytes, line.as_bytes(), "{}: the file holds exactly the appended line", kind.as_str());
        let v = verify(&t.log());
        assert_eq!(v.status.code(), Some(0), "{}: {}", kind.as_str(), err(&v));
        let l = list(&t.log());
        assert_eq!(l.status.code(), Some(0));
        assert_eq!(out(&l), line, "{}: list prints it back byte-identical", kind.as_str());
        eprintln!("round trip ok: {}", kind.as_str());
    }
    // Two kinds in one log: both lines list, in append order.
    let t = Tmp::new();
    append_record(&t.0, &t.log(), &good_record(Kind::SessionStarted)).unwrap();
    append_record(&t.0, &t.log(), &good_record(Kind::Quarantined)).unwrap();
    let l = list(&t.log());
    assert_eq!(l.status.code(), Some(0));
    assert_eq!(out(&l).lines().count(), 2);
    assert!(out(&l).starts_with("{\"v\":2,\"kind\":\"session_started\""));
    assert!(out(&l).contains("\"kind\":\"quarantined\""));
}

// ---------------------------------------------------------------- AC 2: a missing field

#[test]
fn ac2_every_declared_field_of_every_kind_is_required_at_read_and_at_write() {
    let mut cases = 0;
    for kind in p1b_kinds() {
        for key in kind.keys() {
            if matches!(key, "v" | "kind" | "ts_ms") {
                continue;
            }
            // Read: the line with that one pair removed is refused naming the key.
            refused_at_read(kind, &raw_line(kind, |ps| ps.into_iter().filter(|(k, _)| *k != key).collect()), &format!("{key} missing"));
            // Write: the pairs without that key are refused naming the key.
            refused_at_write(kind, &pairs_for(kind).into_iter().filter(|(k, _)| *k != key).collect::<Vec<_>>(), &format!("{key} missing"));
            cases += 1;
        }
    }
    assert!(cases >= 27, "one case per kind x declared key: {cases}");
    eprintln!("missing-field cases: {cases} (read) + {cases} (write)");
}

// ---------------------------------------------------------------- AC 3: an extra field, a cross-kind field, a duplicate

#[test]
fn ac3_unknown_extra_cross_kind_and_duplicate_keys_are_refused() {
    for kind in p1b_kinds() {
        // An unknown key.
        refused_at_read(kind, &raw_line(kind, |mut ps| { ps.push(("note", Field::Str("x".into()))); ps }), "unexpected key");
        refused_at_write(kind, &{ let mut ps = pairs_for(kind); ps.push(("note", Field::Str("x".into()))); ps }, "unexpected key");
        // A key that is valid for ANOTHER of the nine (derived from the other kind's key list, not typed here).
        let other = p1b_kinds().into_iter().find(|o| o.keys().iter().any(|k| !kind.keys().contains(k))).unwrap();
        let foreign = *other.keys().iter().find(|k| !kind.keys().contains(k)).unwrap();
        let foreign_value = value_for(other, foreign);
        refused_at_read(kind, &raw_line(kind, |mut ps| { ps.push((foreign, foreign_value.clone())); ps }), "unexpected key");
        refused_at_write(kind, &{ let mut ps = pairs_for(kind); ps.push((foreign, foreign_value)); ps }, "unexpected key");
        // A duplicate key (the parser refuses it before the table is consulted).
        refused_at_read(kind, &raw_line(kind, |mut ps| { let first = ps[0].clone(); ps.push(first); ps }), "duplicate key");
        refused_at_write(kind, &{ let mut ps = pairs_for(kind); let first = ps[0].clone(); ps.push(first); ps }, "duplicate key");
    }
}

// ---------------------------------------------------------------- AC 4: value shapes

#[test]
fn ac4_values_outside_their_declared_shape_are_refused_and_their_twins_pass() {
    let uuid_short = "1a2b3c4d";
    let uuid_upper = "3F2A9C01-8D4E-4B6F-9A2C-7E5D1C0B4A83";
    let hex63 = "c51b3f0e7d29a64e8b1c0f3d57a92e6b48cd1a30f5e92c7b6d8014a3f2e9c";
    let sha39 = "9f1c4a7be02d53c86b0a1d4e7f2c8b5a3d6e9f1";
    let sha40 = "9f1c4a7be02d53c86b0a1d4e7f2c8b5a3d6e9f1c";
    let bads: Vec<(Kind, &str, Field)> = vec![
        (Kind::SessionStarted, "session_id", Field::Str(uuid_short.to_string())),
        (Kind::SessionStarted, "session_id", Field::Str(uuid_upper.to_string())),
        (Kind::UnmanagedSession, "session_id", Field::Str(uuid_short.to_string())),
        (Kind::LaunchRefused, "brief_sha256", Field::Str(hex63.to_string())),
        (Kind::TimedOut, "checkpoint", Field::Str(sha39.to_string())),
        (Kind::Quarantined, "from", Field::Str("/x".to_string())),
        (Kind::Quarantined, "from", Field::Str("../x".to_string())),
        (Kind::Quarantined, "from", Field::Str("a/../b".to_string())),
        (Kind::Quarantined, "from", Field::Str("a//b".to_string())),
        (Kind::Quarantined, "from", Field::Str("~x".to_string())),
        (Kind::Quarantined, "from", Field::Str("a/./b".to_string())),
        (Kind::HumanOriginRefused, "path", Field::Str("/x".to_string())),
        (Kind::HumanOriginRefused, "path", Field::Str("~cfg/harness.conf".to_string())),
        (Kind::SessionStarted, "source", Field::Str("cli".to_string())),
        (Kind::TimedOut, "cause", Field::Str("vanished".to_string())),
    ];
    for (kind, key, bad) in bads {
        refused_at_read(kind, &raw_line(kind, |ps| ps.into_iter().map(|(k, v)| (k, if k == key { bad.clone() } else { v })).collect()), key);
        refused_at_write(kind, &pairs_for(kind).into_iter().map(|(k, v)| (k, if k == key { bad.clone() } else { v })).collect::<Vec<_>>(), key);
    }
    // A number shape: a string where an unsigned integer is declared.
    refused_at_write(Kind::CheckIn, &pairs_for(Kind::CheckIn).into_iter().map(|(k, v)| (k, if k == "steps" { Field::Str("many".into()) } else { v })).collect::<Vec<_>>(), "steps");
    refused_at_read(Kind::CheckIn, &raw_line(Kind::CheckIn, |ps| ps.into_iter().map(|(k, v)| (k, if k == "steps" { Field::Str("many".into()) } else { v })).collect()), "steps");
    // cleanup_result honesty (OPEN-12): pass with leftovers is refused at both ends; the twins pass.
    for (result, left, ok) in [("pass", 1u64, false), ("pass", 0u64, true), ("fail", 1u64, true)] {
        let kind = Kind::CleanupResult;
        let pairs: Vec<(&str, Field)> = pairs_for(kind)
            .into_iter()
            .map(|(k, v)| {
                (k, match k {
                    "result" => Field::Str(result.to_string()),
                    "manifest_left" => Field::Num(left),
                    _ => v,
                })
            })
            .collect();
        let line = raw_line(kind, |ps| ps.into_iter().map(|(k, v)| (k, if k == "result" { Field::Str(result.to_string()) } else if k == "manifest_left" { Field::Num(left) } else { v })).collect());
        if ok {
            assert!(Record::build(kind, TS, &pairs).is_ok(), "{result}/{left} is a valid twin");
            accepted_at_read(kind, &line);
        } else {
            refused_at_write(kind, &pairs, "manifest_left must be 0 when result is pass");
            refused_at_read(kind, &line, "manifest_left must be 0 when result is pass");
        }
    }
    // The valid twins of the refused shapes pass.
    accepted_at_read(Kind::SessionStarted, &raw_line(Kind::SessionStarted, |ps| ps.into_iter().map(|(k, v)| (k, if k == "session_id" { Field::Str("3f2a9c01-8d4e-4b6f-9a2c-7e5d1c0b4a83".into()) } else { v })).collect()));
    accepted_at_read(Kind::SessionStarted, &raw_line(Kind::SessionStarted, |ps| ps.into_iter().map(|(k, v)| (k, if k == "source" { Field::Str("hook".into()) } else { v })).collect()));
    accepted_at_read(Kind::LaunchRefused, &raw_line(Kind::LaunchRefused, |ps| ps.into_iter().map(|(k, v)| (k, if k == "brief_sha256" { Field::Str("d".repeat(64)) } else { v })).collect()));
    accepted_at_read(Kind::TimedOut, &raw_line(Kind::TimedOut, |ps| ps.into_iter().map(|(k, v)| (k, if k == "checkpoint" { Field::Str(sha40.to_string()) } else { v })).collect()));
    accepted_at_read(Kind::TimedOut, &raw_line(Kind::TimedOut, |ps| ps.into_iter().map(|(k, v)| (k, if k == "checkpoint" { Field::Str("e".repeat(64)) } else { v })).collect()));
    accepted_at_read(Kind::Quarantined, &raw_line(Kind::Quarantined, |ps| ps.into_iter().map(|(k, v)| (k, if k == "from" { Field::Str("tmp/a/b/c".into()) } else { v })).collect()));
    accepted_at_read(Kind::Quarantined, &raw_line(Kind::Quarantined, |ps| ps.into_iter().map(|(k, v)| (k, if k == "to" { Field::Str("quarantine/2026-11-01/planted-dir".into()) } else { v })).collect()));
}

// ---------------------------------------------------------------- AC 5: the version rule

#[test]
fn ac5_every_kind_exists_only_at_v_2_any_other_v_is_refused_naming_v() {
    for kind in p1b_kinds() {
        let good = raw_line(kind, |ps| ps);
        for bad_v in [1u64, 3, 0] {
            let bad = good.replacen(&format!("\"v\":{}", kind.version()), &format!("\"v\":{bad_v}"), 1);
            assert_ne!(&bad, &good);
            refused_at_read(kind, &bad, &format!("unsupported v {bad_v}"));
        }
    }
    // An OLD kind at a v it never had is refused the same way (the base kinds are v 2 only).
    let base = "{\"v\":3,\"kind\":\"escalation\",\"ts_ms\":1,\"run_id\":\"run-1\",\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\",\"role\":\"e\",\"model\":\"m\"}";
    refused_at_read(Kind::Escalation, base, "unsupported v 3");
    let base_v1 = "{\"v\":1,\"kind\":\"escalation\",\"ts_ms\":1,\"run_id\":\"run-1\",\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\",\"role\":\"e\",\"model\":\"m\"}";
    refused_at_read(Kind::Escalation, base_v1, "unsupported v 1");
}

#[test]
fn ac5_the_base_kinds_still_read_through_the_old_cli_shape() {
    // The old path is untouched: a base-kind append through the CLI (identity flags and all) verifies and lists.
    let t = Tmp::new();
    let a = run(&[
        "events", "append", "--log", t.log().to_str().unwrap(), "--kind", "escalation", "--ticket", "t-1", "--repo", "r",
        "--reason", "cap.hit", "--role", "executor", "--model", "m-1",
    ]);
    assert_eq!(a.status.code(), Some(0), "{}", err(&a));
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(0), "{}", err(&v));
    let l = list(&t.log());
    assert_eq!(out(&l).lines().count(), 2, "the self-issued run_issued line plus the escalation");
}

// ---------------------------------------------------------------- AC 6: the old CLI shape cannot write a P1b kind

#[test]
fn ac6_the_old_cli_append_refuses_every_p1b_kind_and_writes_nothing() {
    for kind in p1b_kinds() {
        let t = Tmp::new();
        // The identity flags PRE-B requires are passed, so the refusal is attributable to the kind, not to a missing
        // envelope value.
        let a = run(&[
            "events", "append", "--log", t.log().to_str().unwrap(), "--kind", kind.as_str(), "--ticket", "t", "--repo", "r",
            "--reason", "x", "--role", "executor", "--model", "m-1",
        ]);
        assert_eq!(a.status.code(), Some(2), "{}: {}", kind.as_str(), err(&a));
        assert!(err(&a).contains("EVENT NOT RECORDED"), "{}: {}", kind.as_str(), err(&a));
        assert!(err(&a).contains(kind.as_str()), "{}: the refusal names the kind: {}", kind.as_str(), err(&a));
        assert!(!t.log().exists(), "{}: absent stays absent", kind.as_str());
    }
}

// ---------------------------------------------------------------- F1 (round-1 review): the caller's pair order never reaches the bytes

/// The written bytes follow the kind's DECLARED key order whatever order the caller passes pairs in: for each of the
/// nine kinds the record built from its pairs REVERSED (still derived from `Kind::keys()`, so an envelope change keeps
/// this file honest) encodes, is written and lists back byte-identically to the canonical-order record.
#[test]
fn f1_reversed_pairs_write_the_declared_order_byte_identically_for_every_kind() {
    // v5 measurement hook: the canonical-order bytes of every kind, printed before any assertion so a round-1
    // (red) run emits them too; a diff against the fixed run must be empty (correct callers see no change).
    for kind in p1b_kinds() {
        eprintln!("v5-canonical {}: {}", kind.as_str(), good_record(kind).encode().unwrap().trim_end());
    }
    for kind in p1b_kinds() {
        let canonical = good_record(kind).encode().unwrap();
        let mut rev = pairs_for(kind);
        rev.reverse();
        let rec = Record::build(kind, TS, &rev).unwrap_or_else(|e| panic!("{}: {e}", kind.as_str()));
        let line = rec.encode().unwrap();
        assert_eq!(line, canonical, "{}: reversed pairs write the same bytes", kind.as_str());
        let t = Tmp::new();
        append_record(&t.0, &t.log(), &rec).unwrap();
        assert_eq!(fs::read(t.log()).unwrap(), canonical.as_bytes(), "{}: the file holds the canonical line", kind.as_str());
        let read = read_log(&t.log()).unwrap();
        assert!(read.is_clean(), "{}: {:?}", kind.as_str(), read);
        assert_eq!(read.events.len(), 1);
        assert_eq!(read.events[0].encode().unwrap(), canonical, "{}: read then encode gives the same bytes", kind.as_str());
        assert_eq!(out(&list(&t.log())), canonical, "{}: events list prints the canonical line", kind.as_str());
    }
}

/// A record mutated out of the declared order after building is refused at the door (the re-check compares against
/// the kind's declared order, not against the record's own order).
#[test]
fn f1_a_record_held_out_of_the_declared_order_is_refused_at_encode() {
    let kind = Kind::TimedOut;
    let mut rec = good_record(kind);
    let last = rec.payload.pop().unwrap();
    rec.payload.insert(0, last);
    match rec.encode() {
        Err(AppendError::Invalid(m)) => assert!(m.contains("canonical order"), "{m}"),
        other => panic!("out-of-order record encoded to {other:?}"),
    }
    let t = Tmp::new();
    assert!(append_record(&t.0, &t.log(), &rec).is_err(), "an out-of-order record is not written");
    assert!(!t.log().exists(), "a refused record leaves no file");
}

// ---------------------------------------------------------------- F2 (round-1 review): the write-side line cap

/// A P1b record whose line exceeds the 4096-byte cap is refused at write with `TooLong` and nothing is written. A
/// relpath value reaches the cap with no length bound of its own (a token segment may be long), so the line cap is
/// the only bound left and this test exercises it through the record writer's own door.
#[test]
fn f2_an_over_cap_record_is_refused_at_write_with_nothing_written() {
    let kind = Kind::Quarantined;
    let long = Field::Str(format!("q/{}", "a".repeat(2100)));
    let pairs: Vec<(&str, Field)> = pairs_for(kind)
        .into_iter()
        .map(|(k, v)| (k, if k == "from" || k == "to" { long.clone() } else { v }))
        .collect();
    let rec = Record::build(kind, TS, &pairs).unwrap_or_else(|e| panic!("{e}"));
    match rec.encode() {
        Err(AppendError::TooLong(n)) => assert!(n > LINE_CAP, "the fixture is past the cap: {n}"),
        other => panic!("an over-cap record encoded to {other:?}"),
    }
    let t = Tmp::new();
    match append_record(&t.0, &t.log(), &rec) {
        Err(AppendError::TooLong(n)) => assert!(n > LINE_CAP, "{n}"),
        other => panic!("append accepted an over-cap record: {other:?}"),
    }
    assert!(!t.log().exists(), "an over-cap record leaves no file");
}

// ---------------------------------------------------------------- F3 (round-1 review): a number where a string shape is declared

/// A number where a string shape is declared is refused naming the key, at read and at write (here: `session_id`, a
/// uuid shape, and `source`, an enum shape — the two string shapes of the smallest kind).
#[test]
fn f3_a_number_where_a_string_shape_is_declared_is_refused_at_read_and_at_write() {
    for key in ["session_id", "source"] {
        let kind = Kind::SessionStarted;
        let swap = |ps: Vec<(&'static str, Field)>| ps.into_iter().map(|(k, v)| (k, if k == key { Field::Num(7) } else { v })).collect::<Vec<_>>();
        // Read: the stored line with a bare number for the key is refused naming the key.
        refused_at_read(kind, &raw_line(kind, |ps| swap(ps)), key);
        // Write: the pairs with a number for the key are refused naming the key.
        refused_at_write(kind, &swap(pairs_for(kind)), key);
    }
}

// ---------------------------------------------------------------- round-1 fix-cycle held-out variants (invented for this round)

/// v1 (must-fire): `timed_out` built from pairs in a rotated order (last pair first) writes the canonical bytes.
#[test]
fn v1_timed_out_from_rotated_pairs_writes_the_canonical_bytes() {
    let kind = Kind::TimedOut;
    let canonical = good_record(kind).encode().unwrap();
    let mut rotated = pairs_for(kind);
    let last = rotated.pop().unwrap();
    rotated.insert(0, last);
    let rec = Record::build(kind, TS, &rotated).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(rec.encode().unwrap(), canonical);
    let t = Tmp::new();
    append_record(&t.0, &t.log(), &rec).unwrap();
    assert_eq!(fs::read(t.log()).unwrap(), canonical.as_bytes());
    assert_eq!(out(&list(&t.log())), canonical);
}

/// v2 (must-fire): `launch_refused` with a number where `launch_role` (a short name field) is declared is refused
/// naming the key, on read and on write.
#[test]
fn v2_launch_refused_number_for_launch_role_is_refused_at_read_and_write() {
    let kind = Kind::LaunchRefused;
    let swap = |ps: Vec<(&'static str, Field)>| ps.into_iter().map(|(k, v)| (k, if k == "launch_role" { Field::Num(9) } else { v })).collect::<Vec<_>>();
    refused_at_read(kind, &raw_line(kind, |ps| swap(ps.clone())), "launch_role");
    refused_at_write(kind, &swap(pairs_for(kind)), "launch_role");
}

/// v3 (must-fire): a `quarantined` record padded with the longest allowed relpath values (both `from` and `to`, as
/// many maximal segments as needed) until the line is over the cap is refused with `TooLong` and nothing is written.
#[test]
fn v3_quarantined_padded_past_the_cap_with_relpath_segments_is_refused_too_long() {
    let kind = Kind::Quarantined;
    let pad = Field::Str(format!("{}/{}b", "s".repeat(64), "a/".repeat(1100)));
    let pairs: Vec<(&str, Field)> = pairs_for(kind)
        .into_iter()
        .map(|(k, v)| (k, if k == "from" || k == "to" { pad.clone() } else { v }))
        .collect();
    let rec = Record::build(kind, TS, &pairs).unwrap_or_else(|e| panic!("{e}"));
    let t = Tmp::new();
    match append_record(&t.0, &t.log(), &rec) {
        Err(AppendError::TooLong(n)) => assert!(n > LINE_CAP, "{n}"),
        other => panic!("append accepted an over-cap record: {other:?}"),
    }
    assert!(!t.log().exists());
}

/// v4 (must-fire): a STORED line whose keys are out of the declared order is accepted and re-encoded canonical —
/// the loader's read side stores fields in the declared order, so `events list` prints the canonical bytes. This pins
/// that arm; a mutant that lets the stored order through goes red here.
#[test]
fn v4_a_stored_line_with_keys_out_of_order_is_re_encoded_canonically() {
    for kind in p1b_kinds() {
        let canonical = good_record(kind).encode().unwrap();
        let line = raw_line(kind, |mut ps| {
            ps.reverse();
            ps
        });
        assert_ne!(line.trim_end(), canonical.trim_end(), "{}: the fixture is genuinely out of order", kind.as_str());
        let t = Tmp::new();
        fs::write(t.log(), format!("{line}\n")).unwrap();
        let v = verify(&t.log());
        assert_eq!(v.status.code(), Some(0), "{}: {}", kind.as_str(), err(&v));
        assert_eq!(out(&list(&t.log())), canonical, "{}: list prints the canonical line", kind.as_str());
    }
}

/// v6 (must-not-fire): `check_in` with `steps` as an unsigned integer is still accepted (a number where a number is
/// declared), on write and at read.
#[test]
fn v6_check_in_with_steps_as_a_number_is_still_accepted() {
    let kind = Kind::CheckIn;
    let rec = good_record(kind); // `steps` carries Field::Num(120)
    assert!(matches!(rec.field("steps"), Some(Field::Num(_))));
    let t = Tmp::new();
    append_record(&t.0, &t.log(), &rec).unwrap();
    assert_eq!(out(&list(&t.log())), rec.encode().unwrap());
    accepted_at_read(kind, &raw_line(kind, |ps| ps));
}

// ---------------------------------------------------------------- round-2 fix cycle: a value of any odd shape never panics the loader or the builder

/// `session_started` pairs with `session_id` replaced by `v`.
fn with_session_id(v: &str) -> Vec<(&'static str, Field)> {
    pairs_for(Kind::SessionStarted).into_iter().map(|(k, f)| (k, if k == "session_id" { Field::Str(v.to_string()) } else { f })).collect()
}

/// 36-byte values that are NOT a uuid and hold multibyte characters, each placed so a char straddles a group
/// boundary the slicing in `uuid_ok` would cut (a non-boundary slice panics) or so the value is only multibyte.
fn multibyte_36_byte_values() -> Vec<(&'static str, String)> {
    vec![
        ("R2-F1: accent straddling the first group end", format!("aaaaaaa\u{e9}{}", "a".repeat(27))),
        ("accent straddling the second group end", format!("aaaaaaaa-aaa\u{e9}{}", "a".repeat(22))),
        ("v1: accent in the last group", format!("3f2a9c01-8d4e-4b6f-9a2c-{}\u{e9}", "a".repeat(10))),
        ("v2: twelve three-byte characters", "\u{3042}".repeat(12)),
    ]
}

/// R2-F1 + held-out v1/v2: a 36-BYTE uuid-field value holding a multibyte character is refused, never a panic.
/// Read: `verify` exits 1 naming line 2 and `session_id`; `list` prints the intact line 1, then exits 1.
/// Write: `Record::build` returns an `Err` naming `session_id`. (A panic fails the test, so a guard-removed mutant is red.)
#[test]
fn r2f1_a_36_byte_uuid_value_with_a_multibyte_character_is_refused_not_a_panic() {
    let kind = Kind::SessionStarted;
    let good = good_record(kind).encode().unwrap();
    for (what, bad) in multibyte_36_byte_values() {
        assert_eq!(bad.len(), 36, "{what}: the fixture is 36 BYTES");
        assert!(!bad.is_ascii(), "{what}: the fixture holds a multibyte character");
        let pairs = with_session_id(&bad);
        match Record::build(kind, TS, &pairs) {
            Err(e) => assert!(e.contains("session_id"), "{what}: {e}"),
            Ok(_) => panic!("{what}: build accepted {bad:?}"),
        }
        let line = raw_line(kind, |ps| ps.into_iter().map(|(k, f)| (k, if k == "session_id" { Field::Str(bad.clone()) } else { f })).collect());
        let t = Tmp::new();
        fs::write(t.log(), format!("{good}{line}\n")).unwrap();
        let v = verify(&t.log());
        assert_eq!(v.status.code(), Some(1), "{what}: verify refuses (not a panic): {}", err(&v));
        assert!(err(&v).contains("line 2") && err(&v).contains("session_id"), "{what}: names line 2 and session_id: {}", err(&v));
        let l = list(&t.log());
        assert_eq!(l.status.code(), Some(1), "{what}: list exits non-zero without a panic: {}", err(&l));
        assert_eq!(out(&l), good, "{what}: list prints the intact earlier line first");
    }
}

/// v3 (must-fire): a 64-byte digest holding a multibyte character is refused naming the key (pinned: already safe).
#[test]
fn v3_a_64_byte_digest_with_a_multibyte_character_is_refused_naming_the_key() {
    let kind = Kind::LaunchRefused;
    let bad = format!("{}\u{e9}", "a".repeat(62));
    assert_eq!(bad.len(), 64);
    let swap = |ps: Vec<(&'static str, Field)>| ps.into_iter().map(|(k, v)| (k, if k == "brief_sha256" { Field::Str(bad.clone()) } else { v })).collect::<Vec<_>>();
    refused_at_read(kind, &raw_line(kind, |ps| swap(ps.clone())), "brief_sha256");
    refused_at_write(kind, &swap(pairs_for(kind)), "brief_sha256");
}

/// v4 (must-fire): a relpath value with a multibyte character in the middle of a segment is REFUSED today (a segment
/// is a token: ASCII lowercase, digits, `-`, `_`, `.`), on read and on write. A mutant that lets non-ASCII letters
/// into a token goes red here.
#[test]
fn v4_a_relpath_with_a_multibyte_character_mid_segment_is_refused_naming_the_key() {
    let kind = Kind::TimedOut;
    let bad = "handover/t-\u{e9}7.md".to_string();
    let swap = |ps: Vec<(&'static str, Field)>| ps.into_iter().map(|(k, v)| (k, if k == "handover" { Field::Str(bad.clone()) } else { v })).collect::<Vec<_>>();
    refused_at_read(kind, &raw_line(kind, |ps| swap(ps.clone())), "handover");
    refused_at_write(kind, &swap(pairs_for(kind)), "handover");
}

/// v5 (must-not-fire): canonical lowercase 36-character uuids (made up, none from the loader) are still accepted.
#[test]
fn v5_canonical_lowercase_uuids_are_still_accepted() {
    let kind = Kind::SessionStarted;
    for id in ["00000000-0000-0000-0000-000000000000", "ffffffff-ffff-ffff-ffff-ffffffffffff", "0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9"] {
        let rec = Record::build(kind, TS, &with_session_id(id)).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(rec.field("session_id"), Some(&Field::Str(id.to_string())));
        accepted_at_read(kind, &raw_line(kind, |ps| ps.into_iter().map(|(k, f)| (k, if k == "session_id" { Field::Str(id.to_string()) } else { f })).collect()));
    }
}

/// v6 (must-not-fire) + N-r2-2: the old base kinds through the old `Event::encode` path write the same bytes as a
/// base-kind `Record` built from the pairs REVERSED (this pins the identity sort: six envelope keys, reversed).
#[test]
fn v6_n_r2_2_base_kinds_from_reversed_pairs_write_the_canonical_old_bytes() {
    for kind in Kind::ALL.iter().copied().filter(|k| k.is_base()) {
        let canon = good_record(kind);
        let old = Event {
            kind,
            ts_ms: TS,
            run_id: canon.run_id.clone(),
            ticket: canon.ticket.clone(),
            repo: canon.repo.clone(),
            reason: canon.reason.clone(),
            role: canon.role.clone(),
            model: canon.model.clone(),
        }
        .encode()
        .unwrap();
        let mut rev = pairs_for(kind);
        rev.reverse();
        let rec = Record::build(kind, TS, &rev).unwrap_or_else(|e| panic!("{}: {e}", kind.as_str()));
        assert_eq!(rec.encode().unwrap(), old, "{}: reversed pairs write the old canonical bytes", kind.as_str());
        assert_eq!(canon.encode().unwrap(), old, "{}: canonical pairs write the old bytes", kind.as_str());
    }
}

/// v7 (must-not-fire): a log of only good lines (all nine kinds) still verifies with exit 0.
#[test]
fn v7_a_log_of_only_good_lines_verifies_clean() {
    let t = Tmp::new();
    for kind in p1b_kinds() {
        append_record(&t.0, &t.log(), &good_record(kind)).unwrap();
    }
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(0), "{}", err(&v));
}
