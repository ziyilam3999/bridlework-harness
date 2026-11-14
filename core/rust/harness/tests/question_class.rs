//! G8.4 operator-question classifier contract tests (docs/question-class.md, ticket 191). The
//! classifier is pure: every input is planted here, every outcome is observed from the returned
//! record and advisor answer only. The fail-closed fixture under `fixtures/` carries its bad
//! conditions in its rows (no column is trimmed, so a padded or cased value stays as planted);
//! the row-label set is asserted so a fixture that drops a condition fails.

use harness::events::{Field, Kind, Record};
use harness::question_class::{classify, Advisor, QuestionInput};

const CASES: &str = include_str!("fixtures/g84-cases.txt");
/// A synthetic stamp: fixed, so the encoded bytes are deterministic.
const TS: u64 = 1731000000123;

fn input<'a>(
    request: Option<&'a str>,
    ticket_size: Option<&'a str>,
    plan_change: Option<&'a str>,
    section7: Option<&'a str>,
    risk_signal: Option<&'a str>,
) -> QuestionInput<'a> {
    QuestionInput { ts_ms: TS, request, ticket_size, plan_change, section7, risk_signal }
}

/// A plain question input with the four classifying facts free.
fn question<'a>(ticket_size: &'a str, plan_change: &'a str, section7: &'a str, risk_signal: &'a str) -> QuestionInput<'a> {
    input(Some("question"), Some(ticket_size), Some(plan_change), Some(section7), Some(risk_signal))
}

fn str_field(rec: &Record, key: &str) -> String {
    match rec.field(key) {
        Some(Field::Str(s)) => s.clone(),
        other => panic!("no string field {key}: {other:?}"),
    }
}

// ---- AC 2 (G8.4.1): L + high calls the advisor ----

#[test]
fn l_and_high_calls_the_advisor() {
    let out = classify(&question("L", "no", "no", "release"));
    assert_eq!(out.record.kind, Kind::QuestionClass);
    assert_eq!(out.record.ts_ms, TS);
    assert_eq!(str_field(&out.record, "size"), "L");
    assert_eq!(str_field(&out.record, "risk"), "high");
    assert_eq!(str_field(&out.record, "rule_hit"), "advisor");
    assert_eq!(out.advisor, Advisor::Consult { trigger: "operator-question" });
}

// ---- AC 3 (G8.4.1): every other combination is T2 only ----

#[test]
fn only_l_and_high_call_the_advisor() {
    // (label, ticket_size, risk_signal): every other fact clean; each row crosses exactly one arm
    // of the AND, so an OR mutant (AC 8) fires here and a missing-arm mutant fires here too.
    let rows = [("L+low", "L", "none"), ("M+high", "M", "release"), ("S+high", "S", "egress"), ("no-ticket+high", "none", "destructive")];
    for (label, size, signal) in rows {
        let out = classify(&question(size, "no", "no", signal));
        assert_eq!(str_field(&out.record, "rule_hit"), "t2_only", "{label}: only L AND high may consult");
        assert_eq!(out.advisor, Advisor::None, "{label}: no advisor without both L and high");
    }
}

// ---- AC 4: a status request never reaches the advisor ----

#[test]
fn status_never_reaches_the_advisor() {
    // The other facts say L + high (a plan change, a section 7 decision, a destructive signal):
    // a status request is still logged as status, unclassified, no advisor.
    let out = classify(&input(Some("status"), Some("L"), Some("yes"), Some("yes"), Some("destructive")));
    assert_eq!(str_field(&out.record, "rule_hit"), "status");
    assert_eq!(str_field(&out.record, "size"), "unclassified");
    assert_eq!(str_field(&out.record, "risk"), "unclassified");
    assert_eq!(out.advisor, Advisor::None);
    // The other facts are IGNORED for a status request, even ones that would fail a question closed.
    let out = classify(&input(Some("status"), Some("garbage"), None, Some("maybe"), Some("medium")));
    assert_eq!(str_field(&out.record, "rule_hit"), "status", "a status request never falls to fail_closed");
    assert_eq!(out.advisor, Advisor::None);
}

// ---- AC 5: bad input fails closed (the planted fixture) ----

/// One fixture row: its label and its pipe-separated columns, NOT trimmed (a padded or cased value
/// must reach the classifier exactly as planted).
struct Row {
    id: String,
    cols: Vec<String>,
}

fn rows(text: &str) -> Vec<Row> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| {
            let mut parts = l.split('|');
            Row { id: parts.next().unwrap_or_default().to_string(), cols: parts.map(|p| p.to_string()).collect() }
        })
        .collect()
}

fn fact(col: &str) -> Option<&str> {
    if col == "-" { None } else { Some(col) }
}

#[test]
fn bad_input_fails_closed() {
    let rs = rows(CASES);
    let mut labels: Vec<&str> = rs.iter().map(|r| r.id.as_str()).collect();
    labels.sort();
    let mut want = vec!["b1", "b2", "b3", "b4", "b5", "b6", "b7", "b8"];
    want.sort();
    assert_eq!(labels, want, "the fixture must carry exactly the eight labelled bad conditions");
    // The valid set of each column, to tell the planted bad value from the clean columns beside it.
    let sets: [&[&str]; 5] = [
        &["status", "question"],
        &["S", "M", "L", "none"],
        &["yes", "no"],
        &["yes", "no"],
        &["none", "risk-path", "release", "privacy", "egress", "destructive"],
    ];
    for r in &rs {
        let out = classify(&input(fact(&r.cols[0]), fact(&r.cols[1]), fact(&r.cols[2]), fact(&r.cols[3]), fact(&r.cols[4])));
        assert_eq!(str_field(&out.record, "rule_hit"), "fail_closed", "{}", r.id);
        assert_eq!(str_field(&out.record, "size"), "unclassified", "{}: a fail-closed row never guesses a size", r.id);
        assert_eq!(str_field(&out.record, "risk"), "unclassified", "{}: a fail-closed row never guesses a risk", r.id);
        assert_eq!(out.advisor, Advisor::None, "{}: a fail-closed row never consults the advisor", r.id);
        // The encoded event carries none of the planted bad values. A bad value never appears as a
        // JSON value in the line (checked in its quoted form, so a one-letter plant like `l` cannot
        // "match" the plain letters inside the fixed tokens).
        let line = out.record.encode().unwrap_or_else(|e| panic!("{}: a fail-closed record encodes: {e}", r.id));
        for (i, col) in r.cols.iter().enumerate() {
            let planted = fact(col);
            let bad = match planted {
                Some(v) if !sets[i].contains(&v) => v,
                _ => continue,
            };
            assert!(!line.contains(&format!("\"{bad}\"")), "{}: the event echoed the bad value {bad:?}: {line}", r.id);
        }
    }
}

// ---- AC 6: the rubric sets size and risk ----

#[test]
fn rubric_sets_size_and_risk() {
    // plan_change=yes lifts a ticket-S question with a privacy signal to L/high/advisor.
    let out = classify(&question("S", "yes", "no", "privacy"));
    assert_eq!(str_field(&out.record, "size"), "L", "a plan-change question is L whatever the ticket says");
    assert_eq!(str_field(&out.record, "risk"), "high", "privacy is a high signal");
    assert_eq!(str_field(&out.record, "rule_hit"), "advisor");
    assert_eq!(out.advisor, Advisor::Consult { trigger: "operator-question" });

    // section7=yes lifts a ticket-M question with no signal to L/high/advisor.
    let out = classify(&question("M", "no", "yes", "none"));
    assert_eq!(str_field(&out.record, "size"), "L", "a section-7 question is L whatever the ticket says");
    assert_eq!(str_field(&out.record, "risk"), "high", "a section-7 decision is high on its own");
    assert_eq!(str_field(&out.record, "rule_hit"), "advisor");
    assert_eq!(out.advisor, Advisor::Consult { trigger: "operator-question" });

    // No ticket named, nothing flagged: S/low/t2_only.
    let out = classify(&question("none", "no", "no", "none"));
    assert_eq!(str_field(&out.record, "size"), "S", "a question naming no ticket is S");
    assert_eq!(str_field(&out.record, "risk"), "low");
    assert_eq!(str_field(&out.record, "rule_hit"), "t2_only");
    assert_eq!(out.advisor, Advisor::None);

    // A plain ticket-M question passes the ticket's own size through.
    let out = classify(&question("M", "no", "no", "none"));
    assert_eq!(str_field(&out.record, "size"), "M");
    assert_eq!(str_field(&out.record, "risk"), "low");
    assert_eq!(str_field(&out.record, "rule_hit"), "t2_only");
}

// ---- AC 7: the record round-trips, and the reader takes the kind ----

#[test]
fn question_class_event_round_trips() {
    // One outcome per rule_hit token, each observed only through encode/decode.
    let outs = [
        classify(&input(Some("status"), None, None, None, None)),
        classify(&input(None, Some("L"), Some("no"), Some("no"), Some("none"))),
        classify(&question("L", "no", "no", "release")),
        classify(&question("S", "no", "no", "none")),
        classify(&question("M", "no", "no", "egress")),
    ];
    let mut hits: Vec<String> = outs.iter().map(|o| str_field(&o.record, "rule_hit")).collect();
    hits.sort();
    hits.dedup();
    assert_eq!(hits, vec!["advisor", "fail_closed", "status", "t2_only"], "every rule_hit token is covered");
    for out in &outs {
        let line = out.record.encode().expect("a question_class record encodes");
        let dec = Record::decode(line.trim_end_matches('\n')).expect("the encoded line decodes");
        assert_eq!(dec, out.record, "decode returns an equal record");
        assert_eq!(dec.kind, Kind::QuestionClass);
    }
}

#[cfg(unix)]
#[test]
fn question_class_line_verifies_and_a_bad_risk_is_refused() {
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    const BIN: &str = env!("CARGO_BIN_EXE_harness");
    let dir: PathBuf = std::env::temp_dir().join(format!("g84-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let log = dir.join("events.jsonl");
    let run = |args: &[&str]| {
        let mut c = Command::new(BIN);
        c.args(args).stdin(Stdio::null());
        for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL"] {
            c.env_remove(k);
        }
        c.output().expect("the harness binary runs")
    };
    // One valid question_class line: verify passes.
    let out = classify(&question("L", "no", "no", "release"));
    let line = out.record.encode().unwrap();
    fs::write(&log, &line).unwrap();
    let v = run(&["events", "verify", "--log", log.to_str().unwrap()]);
    assert_eq!(v.status.code(), Some(0), "a valid question_class line verifies: {}", String::from_utf8_lossy(&v.stderr));
    // A planted line with risk normal (the ticket vocabulary, not the classifier's): refused naming risk.
    let planted = "{\"v\":2,\"kind\":\"question_class\",\"ts_ms\":1731000000123,\"size\":\"L\",\"risk\":\"normal\",\"rule_hit\":\"advisor\"}";
    fs::write(&log, format!("{planted}\n")).unwrap();
    let v = run(&["events", "verify", "--log", log.to_str().unwrap()]);
    assert_eq!(v.status.code(), Some(1), "a planted risk token is corruption, exit 1: {}", String::from_utf8_lossy(&v.stderr));
    let text = format!("{}{}", String::from_utf8_lossy(&v.stdout), String::from_utf8_lossy(&v.stderr));
    assert!(text.contains("risk"), "the refusal names risk: {text}");
    let _ = fs::remove_dir_all(&dir);
}
