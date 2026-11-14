//! Ticket 3601, A1: the four optional evidence keys (`at`, `impact`, `why`, `proof`) on the one inbox row.
//! The row library is exercised directly (byte-identical re-encode, the mapper) and the real binary as a child
//! process (refusals, privacy hits, a stored row). Every value is synthetic; the planted home path is assembled
//! from pieces at run time, never typed whole.
#![cfg(unix)]
mod friction_common;
use friction_common::*;
use harness::friction::{encode_row, parse_row};
use harness::friction_event::map_line;
use std::fs;

fn planted_home() -> String {
    ["/Us", "ers/", "zztester", "/proj"].concat()
}

#[test]
fn old_rows_re_encode_byte_identical() {
    let six = "{\"id\":\"les-001\",\"date\":\"2026-11-01\",\"ticket\":\"154\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"the lesson\"}";
    let kri = "{\"id\":\"pat-001-r1\",\"date\":\"2026-11-02\",\"ticket\":\"192\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"it worked again\",\"kind\":\"pattern\",\"repeats\":\"pat-001\",\"source\":\"event\"}";
    for line in [six, kri] {
        assert_eq!(encode_row(&parse_row(line).unwrap()).unwrap(), line);
    }
    // all four evidence keys round-trip in canonical order after the old optional keys
    let full = "{\"id\":\"les-002\",\"date\":\"2026-11-01\",\"ticket\":\"154\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"t\",\"source\":\"event\",\"at\":\"2026-11-01T10:20:30Z\",\"impact\":\"blocker\",\"why\":\"suspected: a stale lock\",\"proof\":\"bridlework-harness@abcdef1:docs/x.md:12; session:s-fake; pr:o/r#7; cmd:ls -l; ~/notes/a.md\"}";
    assert_eq!(encode_row(&parse_row(full).unwrap()).unwrap(), full);
}

#[test]
fn at_date_mismatch_and_bad_shapes_refused_naming_at() {
    let t = Tmp::new();
    let o = add(&t, &[("--id", "a-1"), ("--date", "2026-11-01"), ("--ticket", "t-1"), ("--where-it-lives", "memory"), ("--at", "2026-11-02T01:02:03Z"), ("--text", "x")]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("at:") && err(&o).contains("date"), "{}", err(&o));
    assert!(!t.inbox().exists() || fs::read(t.inbox()).unwrap().is_empty());
    for bad in ["2026-11-01", "2026-11-01T24:00:00Z", "2026-11-01T10:61:00Z", "2026-11-01 10:00:00Z", "2026-11-01T10:00:00", "2026-02-30T10:00:00Z"] {
        let o = add(&t, &[("--id", "a-1"), ("--date", "2026-11-01"), ("--ticket", "t-1"), ("--where-it-lives", "memory"), ("--at", bad), ("--text", "x")]);
        assert_eq!(code(&o), 1, "{bad}: {}", err(&o));
        assert!(err(&o).contains("at must be"), "{bad}: {}", err(&o));
    }
}

#[test]
fn impact_why_and_proof_shapes_are_refused_naming_the_field() {
    let t = Tmp::new();
    let base = [("--id", "a-1"), ("--date", "2026-11-01"), ("--ticket", "t-1"), ("--where-it-lives", "memory"), ("--text", "x")];
    let with = |extra: (&'static str, &str)| {
        let mut kv: Vec<(&str, &str)> = base.to_vec();
        kv.push(extra);
        add(&t, &kv)
    };
    let o = with(("--impact", "urgent"));
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("impact must be"), "{}", err(&o));
    let long = "w".repeat(1001);
    let o = with(("--why", &long));
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("why is over"), "{}", err(&o));
    let o = with(("--why", "two\nlines"));
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("why must be"), "{}", err(&o));
    for bad in ["nonsense", "session:UPPER", "pr:o/r", "pr:o/r#x", "r@abc:file", "r@abcdef1:", "cmd:", "~/", "session:a; ", "r@abcdef1:/abs"] {
        let o = with(("--proof", bad));
        assert_eq!(code(&o), 1, "{bad}: {}", err(&o));
        assert!(err(&o).contains("pointer") || err(&o).contains("proof"), "{bad}: {}", err(&o));
    }
    let o = with(("--proof", &"p".repeat(2001)));
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("proof is over"), "{}", err(&o));
}

#[test]
fn unfolded_home_path_in_proof_is_refused_with_class_home_path() {
    let t = Tmp::new();
    let home = planted_home();
    for proof in [format!("{home}/a.md"), format!("cmd:cat {home}/a.md")] {
        let o = add(&t, &[("--id", "a-1"), ("--date", "2026-11-01"), ("--ticket", "t-1"), ("--where-it-lives", "memory"), ("--proof", &proof), ("--text", "x")]);
        assert_eq!(code(&o), 1, "{proof}: {}", err(&o));
        assert!(out(&o).is_empty());
        let e = err(&o);
        // an absolute path is not a pointer shape; the cmd form reaches the privacy engine
        if proof.starts_with("cmd:") {
            assert!(e.contains("field proof has a privacy hit (home-path)"), "{e}");
        }
        assert!(!e.contains("zztester"), "{e}");
    }
    assert!(!t.inbox().exists() || fs::read(t.inbox()).unwrap().is_empty());
}

#[test]
fn folded_path_and_session_id_proofs_are_stored() {
    let t = Tmp::new();
    let o = add(
        &t,
        &[
            ("--id", "a-1"),
            ("--date", "2026-11-01"),
            ("--ticket", "t-1"),
            ("--where-it-lives", "memory"),
            ("--at", "2026-11-01T08:09:10Z"),
            ("--impact", "high"),
            ("--why", "suspected: a stale lock"),
            ("--proof", "~/notes/a.md; session:3f2c9a10-1111-2222-3333-444455556666"),
            ("--text", "x"),
        ],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    let body = fs::read_to_string(t.inbox()).unwrap();
    assert!(body.contains("\"proof\":\"~/notes/a.md; session:3f2c9a10-1111-2222-3333-444455556666\""), "{body}");
    let cfg = t.config();
    let o = run(&["friction", "lint", "--config", p(&cfg)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=1\n");
}

// ---------------------------------------------------------------- the mapper

const TS: u64 = 1793577600000 + 3_723_000; // 2026-11-02T01:02:03Z

#[test]
fn mapper_fills_at_and_a_fixed_impact_and_leaves_why_proof_empty() {
    let gate = format!(
        "{{\"v\":2,\"kind\":\"gate_fail\",\"ts_ms\":{TS},\"ticket\":\"199\",\"repo\":\"bridlework-harness\",\"gate_id\":\"rust-test\",\"failure_class\":\"suite-fail\",\"head_sha\":\"aa11bb22cc33dd44ee55ff6600112233445566aa\",\"result\":\"fail\"}}"
    );
    let r = map_line(&gate).unwrap().unwrap();
    assert_eq!(r.impact, "high");
    assert_eq!(r.at, "2026-11-02T01:02:03Z");
    assert_eq!(&r.at[0..10], r.date);
    assert!(r.why.is_empty() && r.proof.is_empty());
    let replan = format!("{{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":{TS},\"run_id\":\"run-1\",\"ticket\":\"193\",\"repo\":\"ai-brain\",\"reason\":\"x\",\"role\":\"executor\",\"model\":\"worker\"}}");
    assert_eq!(map_line(&replan).unwrap().unwrap().impact, "medium");
    let launch = format!("{{\"v\":2,\"kind\":\"launch_refused\",\"ts_ms\":{TS},\"launch_role\":\"worker\",\"brief_sha256\":\"{}\",\"matched_entry\":\"e\"}}", "a".repeat(64));
    assert_eq!(map_line(&launch).unwrap().unwrap().impact, "medium");
    // the mapped row is a valid row of the contract
    assert!(encode_row(&r).is_ok());
}
