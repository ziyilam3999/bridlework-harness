//! Ticket friction-cause-tag: a hand-written friction row (kind empty or `friction`, source empty) must hold
//! a cause tag in `why` at `add`: the first `cause:`, then one space, then `Orchestrator`, `builder` or
//! `system` byte for byte, then the line end or one byte that is not a word byte. Pattern rows, event rows and
//! rows already stored are not checked. Every check runs the real binary as a child process over a scratch
//! inbox; every value is synthetic. The two non-ASCII why cases are built from escapes, not typed.
#![cfg(unix)]
mod friction_common;
use friction_common::*;
use std::fs;
use std::process::Output;

/// The refusal, exactly: exit 1, stdout empty, and these two stderr lines naming the three tags and no byte
/// of the why.
const REFUSAL: &str = "harness: friction refused: why must hold a cause tag: cause: Orchestrator, cause: builder or cause: system\nharness: ROW NOT RECORDED\n";

/// `friction add` for one row: the four fixed fields, the why (`None` leaves the flag out, `Some("")` passes
/// an empty value), then the extra flags (the text among them).
fn add_row(t: &Tmp, id: &str, why: Option<&str>, extra: &[(&str, &str)]) -> Output {
    let mut kv: Vec<(&str, &str)> = vec![
        ("--id", id),
        ("--date", "2026-11-20"),
        ("--ticket", "t-1"),
        ("--where-it-lives", "memory"),
    ];
    if let Some(w) = why {
        kv.push(("--why", w));
    }
    kv.extend_from_slice(extra);
    add(t, &kv)
}

fn lint(t: &Tmp) -> Output {
    run(&["friction", "lint", "--config", p(&t.config())])
}

#[test]
fn a_why_with_a_tag_is_stored() {
    let t = Tmp::new();
    let whys = [
        "cause: Orchestrator",
        "cause: Orchestrator text",
        "cause: builder",
        "cause: system",
        "cause: system (judged)",
        "the brief was short. cause: builder.",
        "cause: builder.",
        "cause: system/x",
        "cause: builder's",
        "cause: builder; cause: ticket",
        "(cause: system)",
        "_cause: builder",
        "-cause: builder",
        "xcause: builder",
    ];
    for (i, w) in whys.iter().enumerate() {
        let id = format!("f-{i}");
        let o = add_row(&t, &id, Some(*w), &[("--text", "a lesson")]);
        assert_eq!(code(&o), 0, "{w}: {}", err(&o));
        assert_eq!(out(&o), format!("stored id={id}\n"), "{w}: {}", out(&o));
        assert!(err(&o).is_empty(), "{w}: {}", err(&o));
    }
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), format!("inbox ok rows={}\n", whys.len()));
}

#[test]
fn a_why_with_no_tag_is_refused_with_two_stderr_lines() {
    let nbsp = format!("cause:{}builder", "\u{a0}");
    let cyrillic = format!("cause: bu{}lder", "\u{456}");
    let whys = [
        "no tag here",
        "cause: orchestrator",
        "Cause: builder",
        "CAUSE: system",
        "cause: Builder",
        "cause: System",
        "cause:builder",
        "cause:  builder",
        "cause : builder",
        "cause: ticket",
        "cause: ticket; cause: system",
        "because: the seat; cause: builder",
        "cause: builders",
        "cause: systems",
        "cause: system-x",
        "cause: builder_x",
        "cause: Orchestrators",
        "cause:",
        "builder",
        nbsp.as_str(),
        cyrillic.as_str(),
    ];
    for w in whys {
        let t = Tmp::new();
        let o = add_row(&t, "f-1", Some(w), &[("--text", "a lesson")]);
        assert_eq!(code(&o), 1, "{w}: {}", err(&o));
        assert!(out(&o).is_empty(), "{w}: {}", out(&o));
        assert_eq!(err(&o), REFUSAL, "{w}: the two refusal lines byte for byte");
        assert!(!t.inbox().exists(), "{w}: a refused add creates no inbox");
    }
}

#[test]
fn a_row_with_no_why_is_refused() {
    // an empty --why, no --why at all, and the kind spelled out as friction: all three hold no tag
    let check = |why: Option<&str>, extra: &[(&str, &str)]| {
        let t = Tmp::new();
        let o = add_row(&t, "f-1", why, extra);
        assert_eq!(code(&o), 1, "{why:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert_eq!(err(&o), REFUSAL);
        assert!(!t.inbox().exists());
    };
    check(Some(""), &[("--text", "a lesson")]);
    check(None, &[("--text", "a lesson")]);
    check(None, &[("--kind", "friction"), ("--text", "a lesson")]);
}

#[test]
fn a_pattern_row_and_an_event_row_need_no_tag() {
    let t = Tmp::new();
    let o = add_row(&t, "p-1", None, &[("--kind", "pattern"), ("--text", "a procedure that worked")]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "stored id=p-1\n");
    assert!(err(&o).is_empty());
    let o = add_row(&t, "e-1", None, &[("--source", "event"), ("--text", "an event row")]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "stored id=e-1\n");
    assert!(err(&o).is_empty());
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=2\n");
}

#[test]
fn a_tag_outside_the_why_does_not_count() {
    let t = Tmp::new();
    // the tag only in the text, no --why
    let o = add_row(&t, "f-1", None, &[("--text", "a lesson with cause: builder inside it")]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(err(&o), REFUSAL);
    assert!(!t.inbox().exists());
    // the tag only in the proof
    let o = add_row(&t, "f-2", None, &[("--text", "a lesson"), ("--proof", "cmd:cause: builder")]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(err(&o), REFUSAL);
    assert!(!t.inbox().exists());
}

#[test]
fn a_breaks_row_is_checked_on_its_own_why() {
    let t = Tmp::new();
    let o = add_row(&t, "f-1", Some("cause: system"), &[("--text", "the first row")]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let first = fs::read(t.inbox()).unwrap();
    // a repeat filed with --breaks needs its own tagged why
    let o = add_row(&t, "f-2", None, &[("--breaks", "f-1"), ("--text", "it broke again")]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(err(&o), REFUSAL);
    assert_eq!(fs::read(t.inbox()).unwrap(), first, "the stored first row stays byte-identical");
    let o = add_row(&t, "f-2", Some("cause: builder"), &[("--breaks", "f-1"), ("--text", "it broke again")]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "stored id=f-2\n");
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=2\n");
}

#[test]
fn old_rows_with_no_tag_stay_and_lint_stays_green() {
    let t = Tmp::new();
    let a = r#"{"id":"o-1","date":"2026-11-01","ticket":"t-1","where-it-lives":"memory","breaks":"","text":"an old row with no why"}"#;
    let b = r#"{"id":"o-2","date":"2026-11-02","ticket":"t-1","where-it-lives":"gate","breaks":"","text":"an old row","why":"cause: ticket"}"#;
    fs::write(t.inbox(), format!("{a}\n{b}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=2\n");
    let o = add_row(&t, "f-1", Some("cause: builder"), &[("--text", "a new row")]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let body = fs::read_to_string(t.inbox()).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!((lines[0], lines[1]), (a, b), "the old rows stay byte for byte");
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=3\n");
}

#[test]
fn the_tag_check_comes_after_the_row_checks_and_before_the_privacy_scan() {
    let t = Tmp::new();
    // a row fault is refused first: a bad impact with no tag gives the impact refusal
    let o = add_row(&t, "f-1", None, &[("--text", "a lesson"), ("--impact", "urgent")]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("impact must be"), "{}", err(&o));
    assert!(!err(&o).contains("cause tag"), "{}", err(&o));
    assert!(!t.inbox().exists());
    // a deny word as the text with no tag: the tag refusal, and the word is not echoed
    let o = add_row(&t, "f-2", None, &[("--text", DENY)]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(err(&o), REFUSAL);
    assert!(!err(&o).contains(DENY), "{}", err(&o));
    assert!(!t.inbox().exists());
    // the same word with a tag: the privacy refusal, so the scan runs after the tag check
    let o = add_row(&t, "f-3", Some("cause: system"), &[("--text", DENY)]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("field text has a privacy hit"), "{}", err(&o));
    assert!(!t.inbox().exists());
}

#[test]
fn a_refused_add_leaves_the_inbox_byte_identical_and_the_refusal_echoes_no_why() {
    let t = Tmp::new();
    let o = add_row(&t, "f-1", Some("cause: system"), &[("--text", "the first row")]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let before = fs::read(t.inbox()).unwrap();
    let why = "the seat guessed a private cause qzwx";
    let o = add_row(&t, "f-2", Some(why), &[("--text", "a second row")]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), REFUSAL);
    assert!(!err(&o).contains(why), "the refusal holds no byte of the why");
    assert_eq!(fs::read(t.inbox()).unwrap(), before, "a refused add leaves the inbox byte-identical");
}
