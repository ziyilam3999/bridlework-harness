//! G8.3 proven-tier dispatch contract tests (docs/fix-tier.md, ticket 190). The decision function is
//! pure: every input is planted here, every outcome is observed from the returned decision only.
//! The red and green fixtures under `fixtures/` carry their conditions in their rows; the row-label
//! set is asserted so a fixture that drops a condition fails.

use harness::config::Config;
use harness::events::{Kind, Record};
use harness::fix_tier::{decide, Decision, DispatchInput, Lesson, RunIdentity, Tier};

const RED: &str = include_str!("fixtures/g83-red.txt");
const GREEN: &str = include_str!("fixtures/g83-green.txt");

/// A synthetic run identity: plain tokens only (the escalation event must pass `Event::validate`).
fn identity() -> RunIdentity<'static> {
    RunIdentity { ts_ms: 42, run_id: "run-1", ticket: "190", repo: "bridlework-harness", role: "executor", model: "model-b" }
}

fn input<'a>(
    lesson: Option<Lesson<'a>>,
    requested: Option<&'a str>,
    reason: Option<&'a str>,
    default_main: &'a str,
) -> DispatchInput<'a> {
    DispatchInput { lesson, default_main, requested, escalation_reason: reason, identity: identity() }
}

fn lesson<'a>(id: &'a str, status: &'a str, tier: &'a str) -> Lesson<'a> {
    Lesson { id, status, minimum_proven_tier: tier }
}

fn dispatch(d: Decision) -> (Tier, Option<String>, Option<harness::events::Event>) {
    match d {
        Decision::Dispatch { tier, lesson_id, escalation } => (tier, lesson_id, escalation),
        Decision::Refused(r) => panic!("expected a dispatch, got a refusal: {}", r.message()),
    }
}

fn refused(d: Decision, needle: &str) {
    match d {
        Decision::Refused(r) => assert!(r.message().contains(needle), "refusal {:?} should name {needle:?}", r.message()),
        Decision::Dispatch { .. } => panic!("expected a refusal naming {needle:?}, got a dispatch"),
    }
}

// ---- AC 1: a validated lesson dispatches at its proven tier, carrying the lesson id ----

#[test]
fn g83_validated_t2_dispatches_at_t2_with_lesson_id() {
    let d = decide(&input(Some(lesson("LESSON-77", "validated", "T2")), None, None, "T3"));
    let (tier, lesson_id, escalation) = dispatch(d);
    assert_eq!(tier, Tier::T2, "dispatch at the lesson's proven tier, not the default");
    assert_eq!(lesson_id.as_deref(), Some("LESSON-77"), "the matched lesson id is carried in the dispatch input");
    assert!(escalation.is_none(), "no escalation is needed at the proven tier");
}

// ---- AC 2 (G8.3.1 FAILS): above the proven tier with no reason refuses, no dispatch ----

#[test]
fn g83_t3_without_escalation_fails() {
    let d = decide(&input(Some(lesson("LESSON-77", "validated", "T2")), Some("T3"), None, "T2"));
    refused(d, "reason");
}

// ---- AC 3: above the proven tier with a reason dispatches with exactly one escalation event ----

#[test]
fn g83_t3_with_reason_writes_one_escalation() {
    let d = decide(&input(Some(lesson("LESSON-77", "validated", "T2")), Some("T3"), Some("needs the wider context"), "T2"));
    let (tier, lesson_id, escalation) = dispatch(d);
    assert_eq!(tier, Tier::T3);
    assert_eq!(lesson_id.as_deref(), Some("LESSON-77"));
    // exactly one escalation event, base kind, valid, naming the over-proven-tier cause
    let ev = escalation.expect("exactly one escalation event");
    assert_eq!(ev.kind, Kind::Escalation);
    ev.validate().expect("the escalation event passes Event::validate");
    assert_eq!(ev.reason, "above-proven-tier");
    // round-trips through the event decoder
    let line = ev.encode().expect("the escalation event encodes");
    let rec = Record::decode(line.trim_end_matches('\n')).expect("the escalation line decodes");
    assert_eq!(rec.kind, Kind::Escalation);
    assert_eq!(rec.run_id, "run-1");
    assert_eq!(rec.ticket, "190");
    assert_eq!(rec.repo, "bridlework-harness");
    assert_eq!(rec.role, "executor");
    assert_eq!(rec.model, "model-b");
    assert_eq!(rec.ts_ms, 42);
    assert_eq!(rec.reason, "above-proven-tier");
}

// ---- AC 4: everything without a proven tier dispatches at default_main ----

#[test]
fn g83_unproven_dispatches_default_main() {
    let d = decide(&input(Some(lesson("LESSON-77", "validated", "unproven")), None, None, "T3"));
    let (tier, lesson_id, escalation) = dispatch(d);
    assert_eq!(tier, Tier::T3);
    assert_eq!(lesson_id.as_deref(), Some("LESSON-77"), "the matched lesson id is still carried");
    assert!(escalation.is_none());
}

#[test]
fn g83_unvalidated_dispatches_default_main() {
    let d = decide(&input(Some(lesson("LESSON-77", "unvalidated", "T1")), Some("T1"), None, "T3"));
    let (tier, lesson_id, escalation) = dispatch(d);
    assert_eq!(tier, Tier::T3, "the unvalidated lesson's tier is not honoured");
    assert_eq!(lesson_id.as_deref(), Some("LESSON-77"));
    assert!(escalation.is_none());
}

#[test]
fn g83_retired_dispatches_default_main() {
    let d = decide(&input(Some(lesson("LESSON-77", "retired", "T1")), None, None, "T2"));
    let (tier, lesson_id, escalation) = dispatch(d);
    assert_eq!(tier, Tier::T2);
    assert_eq!(lesson_id.as_deref(), Some("LESSON-77"));
    assert!(escalation.is_none());
}

#[test]
fn g83_no_match_dispatches_default_main() {
    let d = decide(&input(None, Some("T1"), Some("a reason is present but no lesson matched"), "T3"));
    let (tier, lesson_id, escalation) = dispatch(d);
    assert_eq!(tier, Tier::T3);
    assert_eq!(lesson_id, None, "no lesson matched, so no lesson id is carried");
    assert!(escalation.is_none(), "above-default_main escalation is G8.2's job, not this function's");
}

// ---- AC 5: below the proven tier, and any text outside the contract, refuse ----

#[test]
fn g83_below_proven_refused() {
    let d = decide(&input(Some(lesson("LESSON-77", "validated", "T3")), Some("T2"), Some("cheaper is enough"), "T2"));
    refused(d, "below");
}

#[test]
fn g83_bad_tier_text_refused() {
    // (label, planted input, the planted texts a refusal message must never echo)
    let bads: Vec<(&str, DispatchInput, Vec<&str>)> = vec![
        (
            "requested is a model name",
            input(Some(lesson("LESSON-77", "validated", "T2")), Some("synth-model-4x"), None, "T2"),
            vec!["synth-model-4x"],
        ),
        (
            "requested is lowercase",
            input(Some(lesson("LESSON-77", "validated", "T2")), Some("t3"), Some("a reason"), "T2"),
            vec!["t3"],
        ),
        (
            "requested is T0",
            input(Some(lesson("LESSON-77", "validated", "T2")), Some("T0"), Some("a reason"), "T2"),
            vec!["T0"],
        ),
        (
            "requested is T5",
            input(Some(lesson("LESSON-77", "validated", "T2")), Some("T5"), Some("a reason"), "T2"),
            vec!["T5"],
        ),
        (
            "lesson tier is a model name",
            input(Some(lesson("LESSON-77", "validated", "nova-max-9")), None, None, "T2"),
            vec!["nova-max-9"],
        ),
        (
            "default_main is lowercase",
            input(Some(lesson("LESSON-77", "validated", "T2")), None, None, "t2"),
            vec!["t2"],
        ),
        (
            "default_main is a word",
            input(None, None, None, "cheap"),
            vec!["cheap"],
        ),
        (
            "status is not a W3 value",
            input(Some(lesson("LESSON-77", "VALIDATED", "T2")), None, None, "T2"),
            vec!["VALIDATED"],
        ),
        (
            "status is unknown",
            input(Some(lesson("LESSON-77", "matured", "T2")), None, None, "T2"),
            vec!["matured"],
        ),
        (
            "lesson id has a space",
            input(Some(lesson("LES SON", "validated", "T2")), None, None, "T2"),
            vec!["LES SON"],
        ),
        (
            "lesson id is empty",
            input(Some(lesson("", "validated", "T2")), None, None, "T2"),
            vec![],
        ),
        (
            "run id is not a token",
            DispatchInput {
                lesson: Some(lesson("LESSON-77", "validated", "T2")),
                default_main: "T2",
                requested: None,
                escalation_reason: None,
                identity: RunIdentity { ts_ms: 42, run_id: "Run 1", ticket: "190", repo: "bridlework-harness", role: "executor", model: "model-b" },
            },
            vec!["Run 1"],
        ),
    ];
    for (label, inp, secrets) in &bads {
        let d = decide(inp);
        match d {
            Decision::Refused(r) => {
                for s in secrets {
                    assert!(!r.message().contains(s), "{label}: the refusal echoed the input value {s:?}: {}", r.message());
                }
            }
            Decision::Dispatch { .. } => panic!("{label}: must be refused"),
        }
    }
}

// ---- AC 6: the red and green fixtures ----

/// One fixture row: its label and its pipe-separated columns.
struct Row {
    id: String,
    cols: Vec<String>,
}

fn rows(text: &str) -> Vec<Row> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| {
            let mut parts = l.split('|').map(|p| p.trim().to_string());
            Row { id: parts.next().unwrap_or_default(), cols: parts.collect() }
        })
        .collect()
}

/// Build a full config around one `factory.design_tier` value; `-` leaves the key out entirely.
fn config_text(design_tier: &str) -> String {
    let factory = if design_tier == "-" {
        String::new()
    } else {
        format!("factory:\n  design_tier: {design_tier}\n")
    };
    format!(
        "repos:\n\nbudgets:\n  s_wall_seconds: 1\n  s_tokens: 1\n  m_wall_seconds: 1\n  m_tokens: 1\n  l_wall_seconds: 1\n  l_tokens: 1\n\nshadow:\n  p1a_cap: 5\n  p4a_cap: 10\n  d2a_cap: 20\n  p7_cap: 10\n  p1a_token_budget: 1\n  p4a_token_budget: 1\n  d2a_token_budget: 1\n  p7_token_budget: 1\n\nlanes:\n  count: 4\n\nfriction:\n  inbox: /synthetic-inbox.jsonl\n\n{factory}"
    )
}

fn row_input<'a>(cols: &'a [String]) -> (Option<Lesson<'a>>, Tier) {
    // columns: lesson|status|proven|requested|reason|default
    let lesson = if cols[0] == "-" { None } else { Some(lesson(&cols[0], &cols[1], &cols[2])) };
    let default = Tier::parse(&cols[5]).expect("the fixture's default_main is a tier");
    (lesson, default)
}

#[test]
fn g83_red_fixture_every_row_refused() {
    let rs = rows(RED);
    let mut labels: Vec<&str> = rs.iter().map(|r| r.id.as_str()).collect();
    labels.sort();
    let mut want = vec!["r1", "r2", "r3", "r4", "r5", "r6", "r7"];
    want.sort();
    assert_eq!(labels, want, "the red fixture must carry exactly the seven labelled conditions");
    for r in &rs {
        if r.cols[0] == "cfg" {
            // r6/r7: the config row is refused naming the key, never the value
            let value = r.cols[1].as_str();
            let e = Config::parse(&config_text(value)).expect_err("a config row must be refused");
            assert!(e.contains("factory") && e.contains("design_tier"), "{}: {}", r.id, e);
            if value != "-" {
                assert!(!e.contains(value), "{}: the refusal echoed the value: {e}", r.id);
            }
            continue;
        }
        // columns: lesson|status|proven|requested|reason|default
        let (l, default) = row_input(&r.cols);
        let requested = if r.cols[3] == "-" { None } else { Some(r.cols[3].as_str()) };
        let reason = if r.cols[4] == "-" { None } else { Some(r.cols[4].as_str()) };
        let inp = DispatchInput { lesson: l, default_main: r.cols[5].as_str(), requested, escalation_reason: reason, identity: identity() };
        match decide(&inp) {
            Decision::Refused(_) => {}
            Decision::Dispatch { tier, escalation, .. } => {
                // r2/r3 only: the lesson's tier claim is refused; the sole dispatch allowed is
                // default_main, with no escalation and no lesson-honoured tier.
                assert!(r.id == "r2" || r.id == "r3", "{} dispatched but its condition is bad", r.id);
                assert_eq!(tier, default, "{}: the dispatch must be at default_main", r.id);
                assert!(escalation.is_none(), "{}: no escalation may come from a refused condition", r.id);
            }
        }
    }
}

#[test]
fn g83_green_fixture_every_row_dispatches() {
    let rs = rows(GREEN);
    assert!(rs.len() >= 8, "the green fixture carries every dispatching condition");
    for r in &rs {
        // columns: lesson|status|proven|requested|reason|default|want|escalation
        let (l, _) = row_input(&r.cols);
        let requested = if r.cols[3] == "-" { None } else { Some(r.cols[3].as_str()) };
        let reason = if r.cols[4] == "-" { None } else { Some(r.cols[4].as_str()) };
        let inp = DispatchInput { lesson: l, default_main: r.cols[5].as_str(), requested, escalation_reason: reason, identity: identity() };
        let (tier, lesson_id, escalation) = dispatch(decide(&inp));
        assert_eq!(tier, Tier::parse(&r.cols[6]).unwrap(), "{}: wrong tier", r.id);
        assert_eq!(lesson_id.is_some(), r.cols[0] != "-", "{}: the lesson id is carried exactly when a lesson matched", r.id);
        assert_eq!(escalation.is_some(), r.cols[7] == "yes", "{}: the escalation expectation", r.id);
        if let Some(ev) = escalation {
            ev.validate().expect("a fixture escalation event validates");
        }
    }
}
