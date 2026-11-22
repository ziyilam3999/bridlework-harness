//! Ticket friction-row-status-fields: the two optional row keys `status` and `status_ref`, and the read-only verb
//! `harness friction open`. The row library is exercised directly (the pointer each status needs, the re-encode)
//! and the real binary as a child process on a scratch inbox (privacy, the open list, the neighbours). Every row,
//! pointer and deny word is synthetic.
#![cfg(unix)]
mod friction_common;
// Public so the set-status tests (`friction_status_write.rs`) reuse the helpers and the neighbour fixture of this file.
pub(crate) use friction_common::*;
use harness::friction::{encode_row, encode_row_status, parse_row, parse_row_status};
use std::fs;
use std::process::Output;

// ---- pointers, one good one per status
pub const FIXED: &str = "pr:owner/repo#101";
pub const GUARDED: &str = "bridlework-harness@abcdef1:wiki/friction/friction-controls.txt:12";
pub const TICKET: &str = "~/.bridlework/tickets/friction-row-status-fields.json";
pub const LESSON: &str = "bridlework-harness@abcdef1:.sources/bridlework/lessons/ISSUE-001-x.md";
pub const NOACT: &str = "cmd:one-off, the cause is gone";

fn st_row(id: &str, status: &str, status_ref: &str) -> String {
    row(id, "2026-11-02", "", "a lesson", &[("status", status), ("status_ref", status_ref)])
}

fn parse_err(line: &str) -> String {
    match parse_row_status(line) {
        Ok(_) => panic!("expected a refusal: {line}"),
        Err(e) => e,
    }
}

fn lint(t: &Tmp) -> Output {
    let cfg = t.config();
    run(&["friction", "lint", "--config", p(&cfg)])
}

fn open(t: &Tmp) -> Output {
    let cfg = t.config();
    run(&["friction", "open", "--config", p(&cfg)])
}

// ---------------------------------------------------------------- the row contract

#[test]
fn rows_with_only_the_old_fields_re_encode_byte_identical() {
    let six = "{\"id\":\"les-001\",\"date\":\"2026-11-01\",\"ticket\":\"154\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"the lesson\"}";
    let full = "{\"id\":\"les-002\",\"date\":\"2026-11-01\",\"ticket\":\"154\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"t\",\"source\":\"event\",\"at\":\"2026-11-01T10:20:30Z\",\"impact\":\"blocker\",\"why\":\"suspected: a stale lock\",\"proof\":\"bridlework-harness@abcdef1:docs/x.md:12; session:s-fake\"}";
    for line in [six, full] {
        assert_eq!(encode_row(&parse_row(line).unwrap()).unwrap(), line);
        let (r, st) = parse_row_status(line).unwrap();
        assert_eq!(st.word(), "open");
        assert_eq!(encode_row_status(&r, &st).unwrap(), line);
    }
}

#[test]
fn each_status_with_its_pointer_round_trips_after_the_old_keys() {
    for (status, ptr) in [("fixed", FIXED), ("guarded", GUARDED), ("ticket", TICKET), ("lesson", LESSON), ("no-action", NOACT)] {
        let line = format!("{{\"id\":\"s-1\",\"date\":\"2026-11-02\",\"ticket\":\"t-1\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"a lesson\",\"proof\":\"session:s-9\",\"status\":\"{status}\",\"status_ref\":\"{ptr}\"}}");
        let (r, st) = parse_row_status(&line).unwrap_or_else(|e| panic!("{status}: {e}"));
        assert_eq!(st.word(), status);
        assert_eq!(encode_row_status(&r, &st).unwrap(), line, "{status}");
    }
    // two pointers: one of the right kind is enough
    let line = st_row("s-2", "fixed", "session:s-9; pr:owner/repo#7");
    assert!(parse_row_status(&line).is_ok());
}

#[test]
fn an_explicit_status_open_or_empty_is_refused_naming_status() {
    for v in ["open", ""] {
        let line = row("s-1", "2026-11-02", "", "a lesson", &[("status", v)]);
        let e = parse_err(&line);
        assert!(e.contains("status"), "{v}: {e}");
        assert!(!e.contains("status_ref"), "{v}: {e}");
    }
    // an explicit open with a pointer is refused too
    let e = parse_err(&st_row("s-1", "open", FIXED));
    assert!(e.contains("status"), "{e}");
}

#[test]
fn a_non_open_status_with_no_pointer_is_refused_naming_status_ref() {
    for status in ["fixed", "guarded", "ticket", "lesson", "no-action"] {
        let e = parse_err(&row("s-1", "2026-11-02", "", "a lesson", &[("status", status)]));
        assert!(e.contains("status_ref is missing"), "{status}: {e}");
        let e = parse_err(&row("s-1", "2026-11-02", "", "a lesson", &[("status", status), ("status_ref", "")]));
        assert!(e.contains("status_ref is missing"), "{status}: {e}");
    }
}

#[test]
fn a_status_ref_with_no_status_or_open_is_refused_naming_status_ref() {
    let e = parse_err(&row("s-1", "2026-11-02", "", "a lesson", &[("status_ref", FIXED)]));
    assert!(e.contains("status_ref is allowed only"), "{e}");
    assert!(parse_row_status(&row("s-1", "2026-11-02", "", "a lesson", &[("status_ref", "")])).is_ok());
}

#[test]
fn an_unknown_status_word_is_refused_naming_status() {
    for word in ["done", "Fixed", "FIXED", "wontfix", "no_action", "closed"] {
        let e = parse_err(&st_row("s-1", word, FIXED));
        assert!(e.starts_with("status must be"), "{word}: {e}");
    }
}

#[test]
fn a_pointer_of_the_wrong_kind_for_the_status_is_refused_naming_status_ref() {
    let bad: [(&str, &[&str]); 5] = [
        ("fixed", &["cmd:done", "session:s-1", "~/notes/x.md"]),
        ("guarded", &[FIXED, "bridlework-harness@abcdef1:wiki/friction/friction-controls.txt", "bridlework-harness@abcdef1:wiki/friction/other.txt:12", "bridlework-harness@abcdef1:wiki/friction-controls.txt.bak:12", "bridlework-harness@abcdef1:a/../friction-controls.txt:12"]),
        ("ticket", &["~/notes/x.md", "~/.bridlework/tickets/", "~/.bridlework/tickets/../x.json", "bridlework-harness@abcdef1:docs/x.md", "cmd:see the ticket"]),
        ("lesson", &["bridlework-harness@abcdef1:docs/x.md", "bridlework-harness@abcdef1:.sources/bridlework/lessons/", "bridlework-harness@abcdef1:.sources/bridlework/lessons/../x.md", FIXED]),
        ("no-action", &[FIXED, "session:s-1", "~/notes/x.md", "bridlework-harness@abcdef1:docs/x.md"]),
    ];
    for (status, ptrs) in bad {
        for ptr in ptrs {
            let e = parse_err(&st_row("s-1", status, ptr));
            assert!(e.contains("status_ref"), "{status} {ptr}: {e}");
        }
    }
    // a pointer that is no pointer at all, and the proof cap
    let e = parse_err(&st_row("s-1", "fixed", "nonsense"));
    assert!(e.contains("status_ref"), "{e}");
    let long = format!("{FIXED}; cmd:{}", "x".repeat(2000));
    let e = parse_err(&st_row("s-1", "fixed", &long));
    assert!(e.contains("status_ref is over its 2000-byte cap"), "{e}");
    let e = parse_err(&st_row("s-1", "no-action", "cmd:two\\nlines"));
    assert!(e.contains("status_ref"), "{e}");
}

#[test]
fn a_status_ref_at_the_cap_is_kept_and_one_over_is_refused() {
    let pad = "x".repeat(2000 - "cmd:".len());
    let at_cap = format!("cmd:{pad}");
    assert_eq!(at_cap.len(), 2000);
    assert!(parse_row_status(&st_row("s-1", "no-action", &at_cap)).is_ok());
    let over = format!("{at_cap}x");
    assert!(parse_err(&st_row("s-1", "no-action", &over)).contains("status_ref"));
}

#[test]
fn a_status_ref_that_pushes_the_line_over_the_16k_cap_is_refused() {
    // a text that alone fits: the plain row is stored
    let text = "t".repeat(14_500);
    let plain = row("s-1", "2026-11-02", "", &text, &[]);
    assert!(plain.len() < 16 * 1024);
    assert!(parse_row_status(&plain).is_ok());
    // the same row with a 2000-byte status_ref is over the line cap
    let ptr = format!("cmd:{}", "x".repeat(1996));
    let big = row("s-1", "2026-11-02", "", &text, &[("status", "no-action"), ("status_ref", &ptr)]);
    assert!(big.len() + 1 > 16 * 1024, "{}", big.len());
    let e = parse_err(&big);
    assert!(e.contains("cap"), "{e}");
    // and through the CLI: lint names the line
    let t = Tmp::new();
    t.write_inbox(&[big]);
    let o = lint(&t);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("line 1") && err(&o).contains("cap"), "{}", err(&o));
    // the library encoder refuses the same row
    let (r, st) = {
        let mut r = parse_row(&plain).unwrap();
        r.text = text.clone();
        (r, harness::friction::Status { status: "no-action".to_string(), status_ref: ptr })
    };
    assert!(encode_row_status(&r, &st).unwrap_err().contains("cap"));
}

#[test]
fn the_unknown_field_refusal_names_the_nine_optional_fields() {
    let t = Tmp::new();
    t.write_inbox(&[row("s-1", "2026-11-02", "", "a lesson", &[("extra", "y")])]);
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(e.contains("line 1") && e.contains("an unknown field") && e.contains("the nine optional ones"), "{e}");
    for k in ["kind", "repeats", "source", "at", "impact", "why", "proof", "status", "status_ref"] {
        assert!(e.contains(k), "{k}: {e}");
    }
}

#[test]
fn lint_takes_every_status_and_the_ok_line_is_the_old_one() {
    let t = Tmp::new();
    t.write_inbox(&[
        st_row("s-1", "fixed", FIXED),
        st_row("s-2", "guarded", GUARDED),
        st_row("s-3", "ticket", TICKET),
        st_row("s-4", "lesson", LESSON),
        st_row("s-5", "no-action", NOACT),
        hand("s-6", "2026-11-02", "", ""),
    ]);
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=6\n");
}

#[test]
fn a_bad_status_row_is_named_by_line_through_lint() {
    let t = Tmp::new();
    t.write_inbox(&[hand("s-1", "2026-11-02", "", ""), st_row("s-2", "fixed", "cmd:not a pr")]);
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("line 2") && err(&o).contains("status_ref"), "{}", err(&o));
    assert!(out(&o).is_empty());
}

// ---------------------------------------------------------------- privacy

fn hit_rows(field: &str) -> Vec<String> {
    let bad = format!("cmd:{DENY} is the reason");
    let second = if field == "status_ref" {
        st_row("s-2", "no-action", &bad)
    } else {
        row("s-2", "2026-11-02", "", "a lesson", &[("proof", &bad)])
    };
    vec![hand("s-1", "2026-11-02", "", ""), second]
}

#[test]
fn a_deny_token_in_status_ref_is_a_privacy_hit_on_lint_naming_line_and_field() {
    let t = Tmp::new();
    t.write_inbox(&hit_rows("status_ref"));
    let o = lint(&t);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 2") && err(&o).contains("field status_ref has a privacy hit"), "{}", err(&o));
    assert!(!err(&o).contains(DENY), "{}", err(&o));
}

#[test]
fn every_inbox_reader_reports_a_status_ref_hit_like_a_proof_hit() {
    let note_text = "lesson: s-1, s-2\n";
    let mut results: Vec<(&str, i32, String, String)> = Vec::new();
    for field in ["proof", "status_ref"] {
        let t = Tmp::new();
        t.write_inbox(&hit_rows(field));
        let cfg = t.config();
        let note = t.file("note.md", note_text);
        let verbs: Vec<(&str, Vec<&str>)> = vec![
            ("lint", vec!["friction", "lint"]),
            ("open", vec!["friction", "open"]),
            ("rank", vec!["friction", "rank"]),
            ("suggest", vec!["friction", "suggest", "--text", "a lesson"]),
            ("close-check", vec!["friction", "close-check", p(&note), "--ticket", "t-1"]),
            ("patterns", vec!["friction", "patterns"]),
        ];
        for (name, mut args) in verbs {
            args.push("--config");
            args.push(p(&cfg));
            let o = run(&args);
            results.push((name, code(&o), out(&o), err(&o)));
        }
        // add: a stored row with a hit blocks the append, naming line and field
        let o = add(&t, &[("--id", "s-3"), ("--date", "2026-11-03"), ("--ticket", "t-1"), ("--where-it-lives", "memory"), ("--why", "cause: system"), ("--text", "x")]);
        results.push(("add", code(&o), out(&o), err(&o)));
    }
    let (proof_runs, ref_runs) = results.split_at(results.len() / 2);
    for ((name, pc, pout, perr), (_, sc, sout, serr)) in proof_runs.iter().zip(ref_runs.iter()) {
        assert_ne!(*pc, 0, "{name} proof: {perr}");
        assert_eq!(pc, sc, "{name}: exit code of status_ref differs from proof");
        assert!(pout.is_empty() && sout.is_empty(), "{name}: stdout must stay empty");
        assert!(perr.contains("field proof has a privacy hit"), "{name}: {perr}");
        assert!(serr.contains("field status_ref has a privacy hit"), "{name}: {serr}");
        assert!(serr.contains("line 2"), "{name}: {serr}");
        assert!(!serr.contains(DENY), "{name}: {serr}");
    }
}

// ---------------------------------------------------------------- friction open

fn mixed(t: &Tmp) {
    t.write_inbox(&[
        row("o-1", "2026-11-01", "", "text-one-zzq", &[("impact", "high"), ("why", "why-one-zzq"), ("proof", "session:s-proof-zzq")]),
        row("f-1", "2026-11-02", "", "fixed text", &[("status", "fixed"), ("status_ref", FIXED)]),
        row("o-2", "2026-11-03", "", "text-two", &[]),
        row("g-1", "2026-11-04", "", "guarded text", &[("status", "guarded"), ("status_ref", GUARDED)]),
        row("t-1", "2026-11-05", "", "ticket text", &[("status", "ticket"), ("status_ref", TICKET)]),
        row("l-1", "2026-11-06", "", "lesson text", &[("status", "lesson"), ("status_ref", LESSON)]),
        row("n-1", "2026-11-07", "", "no-action text", &[("status", "no-action"), ("status_ref", NOACT)]),
        row("f-2", "2026-11-08", "", "fixed again", &[("status", "fixed"), ("status_ref", "bridlework-harness@abcdef1:docs/x.md")]),
    ]);
}

#[test]
fn open_lists_only_open_rows_then_the_per_row_counts() {
    let t = Tmp::new();
    mixed(&t);
    let o = open(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "open id=o-1 date=2026-11-01 impact=high ticket=t-1\n\
         open id=o-2 date=2026-11-03 impact=- ticket=t-1\n\
         rows open=2 fixed=2 guarded=1 ticket=1 lesson=1 no-action=1 total=8\n"
    );
    assert!(err(&o).is_empty(), "{}", err(&o));
}

#[test]
fn open_prints_no_text_why_proof_or_status_ref() {
    let t = Tmp::new();
    mixed(&t);
    let o = open(&t);
    assert_eq!(code(&o), 0);
    let s = out(&o);
    for needle in ["zzq", "text", "why", "proof", "status_ref", "pr:", "cmd:", "~/", "friction-controls", "lessons/"] {
        assert!(!s.contains(needle), "{needle} in {s}");
    }
}

#[test]
fn open_on_an_inbox_with_no_row_prints_only_the_count_line() {
    let t = Tmp::new();
    fs::write(t.inbox(), "").unwrap();
    let o = open(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "rows open=0 fixed=0 guarded=0 ticket=0 lesson=0 no-action=0 total=0\n");
}

#[test]
fn open_fails_closed_on_a_torn_tail_a_bad_row_and_a_privacy_hit() {
    // a torn tail
    let t = Tmp::new();
    fs::write(t.inbox(), format!("{}\n{}", hand("o-1", "2026-11-01", "", ""), hand("o-2", "2026-11-02", "", ""))).unwrap();
    let o = open(&t);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("torn tail"), "{}", err(&o));
    // a bad row among good ones
    t.write_inbox(&[hand("o-1", "2026-11-01", "", ""), st_row("o-2", "fixed", "cmd:not a pr"), hand("o-3", "2026-11-03", "", "")]);
    let o = open(&t);
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty(), "no list on a bad inbox: {}", out(&o));
    assert!(err(&o).contains("line 2") && err(&o).contains("status_ref"), "{}", err(&o));
    // a privacy hit in an open row
    t.write_inbox(&[hand("o-1", "2026-11-01", "", ""), row("o-2", "2026-11-02", "", &format!("has {DENY} inside"), &[])]);
    let o = open(&t);
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 2") && err(&o).contains("field text has a privacy hit"), "{}", err(&o));
}

#[test]
fn open_setup_and_usage_errors_exit_two_with_stdout_empty() {
    let t = Tmp::new();
    // no inbox file
    let o = open(&t);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    let cfg = t.config();
    for args in [vec!["friction", "open", "extra"], vec!["friction", "open", "--bogus"], vec!["friction", "open", "--config"]] {
        let o = run(&args);
        assert_eq!(code(&o), 2, "{args:?}: {}", err(&o));
        assert!(out(&o).is_empty());
    }
    let o = run(&["friction", "open", "--config", p(&cfg), "--config", p(&cfg)]);
    assert_eq!(code(&o), 2);
    // no config at all
    let o = run(&["friction", "open"]);
    assert_eq!(code(&o), 2);
}

#[test]
fn the_usage_text_lists_the_open_verb() {
    let o = run(&["friction"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("friction open [--config <file>]"), "{}", err(&o));
    assert!(
        err(&o).contains("friction set-status --id <row id> --set <fixed|guarded|ticket|lesson|no-action|open> [--ref <pointers>] [--config <file>]"),
        "{}",
        err(&o)
    );
}

// ---------------------------------------------------------------- the neighbours do not move

pub fn plain_or_statused(with: bool) -> Vec<String> {
    let s = |status: &'static str, ptr: &'static str| -> Vec<(&'static str, &'static str)> {
        if with {
            vec![("status", status), ("status_ref", ptr)]
        } else {
            vec![]
        }
    };
    let mk = |id: &str, date: &str, breaks: &str, impact: &'static str, extra: Vec<(&'static str, &'static str)>| {
        let mut e: Vec<(&str, &str)> = Vec::new();
        if !impact.is_empty() {
            e.push(("impact", impact));
        }
        e.extend(extra);
        row(id, date, breaks, "lesson", &e)
    };
    vec![
        mk("g-1", "2026-11-02", "", "blocker", s("fixed", FIXED)),
        mk("g-1b", "2026-11-09", "g-1", "", s("guarded", GUARDED)),
        mk("g-2", "2026-11-02", "", "", s("ticket", TICKET)),
        mk("g-3", "2026-11-02", "", "high", s("lesson", LESSON)),
        mk("g-3b", "2026-11-09", "g-3", "", s("no-action", NOACT)),
        mk("g-4", "2026-11-03", "", "low", vec![]),
    ]
}

/// Every neighbour verb on one scratch inbox: (name, exit code, stdout, stderr), then the files triage and page write.
fn neighbours(with: bool) -> Vec<(String, String, String, String)> {
    let t = Tmp::new();
    t.write_inbox(&plain_or_statused(with));
    neighbours_in(&t)
}

/// The same run over a scratch folder whose inbox is already written (the set-status tests change one status first).
pub fn neighbours_in(t: &Tmp) -> Vec<(String, String, String, String)> {
    let cfg = t.config();
    let note = t.file("note.md", "lesson: g-1, g-1b, g-2, g-3, g-3b, g-4\n");
    let controls = t.file("controls.txt", "g-1 | control: a guard pointer | lesson: ISSUE-001\ng-3 | control: a guard pointer | lesson: ISSUE-001\n");
    let lessons = t.0.join("lessons");
    fs::create_dir_all(&lessons).unwrap();
    fs::write(lessons.join("ISSUE-001-x.md"), "x").unwrap();
    let state = t.state();
    let handoff = t.handoff();
    let page = t.0.join("page.md");
    let calls: Vec<(&str, Vec<&str>)> = vec![
        ("lint", vec!["friction", "lint", "--config", p(&cfg)]),
        ("rank", vec!["friction", "rank", "--top", "100000", "--config", p(&cfg)]),
        ("patterns", vec!["friction", "patterns", "--config", p(&cfg)]),
        ("suggest", vec!["friction", "suggest", "--text", "lesson", "--top", "9", "--config", p(&cfg)]),
        ("close-check", vec!["friction", "close-check", p(&note), "--ticket", "t-1", "--config", p(&cfg)]),
        ("controls-check", vec!["friction", "controls-check", "--controls", p(&controls), "--lessons-dir", p(&lessons), "--config", p(&cfg)]),
        ("triage", vec!["friction", "triage", "--week", "2026-w46", "--state", p(&state), "--handoff-dir", p(&handoff), "--as-of", "2026-11-12T00:00:00Z", "--top", "9", "--config", p(&cfg)]),
        ("page", vec!["friction", "page", "--week", "2026-w46", "--state", p(&state), "--out", p(&page), "--config", p(&cfg)]),
    ];
    let mut res = Vec::new();
    for (name, args) in calls {
        let o = run(&args);
        assert!(matches!(code(&o), 0 | 1), "{name}: {}", err(&o));
        res.push((name.to_string(), format!("exit {}", code(&o)), out(&o), err(&o)));
    }
    // the files the two writers wrote
    res.push(("state".to_string(), String::new(), fs::read_to_string(&state).unwrap(), String::new()));
    res.push(("page-file".to_string(), String::new(), fs::read_to_string(&page).unwrap(), String::new()));
    let mut names: Vec<String> = fs::read_dir(&handoff).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    names.sort();
    for n in names {
        res.push((format!("handoff/{n}"), String::new(), fs::read_to_string(handoff.join(&n)).unwrap(), String::new()));
    }
    res
}

#[test]
fn neighbour_verbs_print_and_write_the_same_with_statuses_as_without() {
    let plain = neighbours(false);
    let statused = neighbours(true);
    assert_eq!(plain.len(), statused.len());
    for (a, b) in plain.iter().zip(statused.iter()) {
        assert_eq!(a.0, b.0);
        assert_eq!(a, b, "{} differs", a.0);
    }
    // the runs are real: the lint ok line is the old line, and rank lists every group (rows of any status)
    let lint = plain.iter().find(|r| r.0 == "lint").unwrap();
    assert_eq!(lint.2, "inbox ok rows=6\n");
    let rank = statused.iter().find(|r| r.0 == "rank").unwrap();
    assert!(rank.2.contains("rank group=g-1 ") && rank.2.contains("rank group=g-3 ") && rank.2.contains("rank group=g-2 "), "{}", rank.2);
    let suggest = statused.iter().find(|r| r.0 == "suggest").unwrap();
    assert!(suggest.2.contains("group=g-1 "), "{}", suggest.2);
    assert!(statused.iter().any(|r| r.0.starts_with("handoff/")), "triage proposed nothing");
}

#[test]
fn add_writes_no_status_and_leaves_stored_status_rows_alone() {
    let t = Tmp::new();
    let stored = [st_row("s-1", "fixed", FIXED), hand("s-2", "2026-11-02", "", "")];
    t.write_inbox(&stored);
    let before = fs::read_to_string(t.inbox()).unwrap();
    let o = add(&t, &[("--id", "s-3"), ("--date", "2026-11-03"), ("--ticket", "t-1"), ("--where-it-lives", "memory"), ("--why", "cause: system"), ("--text", "a new lesson")]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let after = fs::read_to_string(t.inbox()).unwrap();
    assert!(after.starts_with(&before), "the stored rows are byte-identical");
    let new_line = &after[before.len()..];
    assert!(new_line.contains("\"id\":\"s-3\"") && !new_line.contains("status"), "{new_line}");
    // a new row is open
    let o = open(&t);
    assert_eq!(code(&o), 0);
    assert!(out(&o).contains("open id=s-3 ") && out(&o).ends_with("rows open=2 fixed=1 guarded=0 ticket=0 lesson=0 no-action=0 total=3\n"), "{}", out(&o));
    // add has no status flags
    let o = add(&t, &[("--id", "s-4"), ("--date", "2026-11-03"), ("--ticket", "t-1"), ("--where-it-lives", "memory"), ("--status", "fixed"), ("--text", "x")]);
    assert_eq!(code(&o), 2, "{}", err(&o));
}
