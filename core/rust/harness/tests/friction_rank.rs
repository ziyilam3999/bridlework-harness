//! Ticket 3601, A2: `harness friction rank` (and A4's `friction page` stdout/--out split, AC-6). The real binary
//! runs as a child on a scratch inbox; the grouping function is also exercised directly for the partition rule.
//! Every row and needle is synthetic.
#![cfg(unix)]
mod friction_common;
use friction_common::*;
use harness::friction::parse_row;
use harness::friction_loop::{groups, ranked};
use std::fs;

fn rank(t: &Tmp, extra: &[&str]) -> std::process::Output {
    let cfg = t.config();
    let mut args: Vec<&str> = vec!["friction", "rank"];
    args.extend_from_slice(extra);
    args.push("--config");
    args.push(p(&cfg));
    run(&args)
}

fn rank_lines(o: &std::process::Output) -> Vec<String> {
    out(o).lines().filter(|l| l.starts_with("rank group=")).map(|l| l.to_string()).collect()
}

#[test]
fn blocker_outranks_low_x5_and_medium_x2() {
    let t = Tmp::new();
    let mut rows = vec![hand("b-1", "2026-11-05", "", "blocker")];
    rows.push(hand("m-1", "2026-11-02", "", "medium"));
    rows.push(hand("m-2", "2026-11-03", "m-1", "medium"));
    rows.push(hand("l-1", "2026-11-01", "", "low"));
    for i in 2..=5 {
        rows.push(hand(&format!("l-{i}"), "2026-11-04", "l-1", "low"));
    }
    t.write_inbox(&rows);
    let o = rank(&t, &["--top", "9"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let l = rank_lines(&o);
    assert_eq!(l.len(), 3, "{l:?}");
    assert!(l[0].starts_with("rank group=b-1 score=27 impact=blocker occurrences=1"), "{l:?}");
    assert!(l[1].starts_with("rank group=m-1 score=6 impact=medium occurrences=2"), "{l:?}");
    assert!(l[2].starts_with("rank group=l-1 score=5 impact=low occurrences=5"), "{l:?}");
    assert!(out(&o).ends_with("rank ok groups=3 listed=3\n"), "{}", out(&o));
}

#[test]
fn event_rows_of_one_signature_are_one_group() {
    let t = Tmp::new();
    let sig = "evt.gate_fail.3_abc";
    t.write_inbox(&[event(sig, "2026-w44", "2026-10-28", "high"), event(sig, "2026-w45", "2026-11-04", "high"), event(sig, "2026-w46", "2026-11-11", "high")]);
    let o = rank(&t, &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let l = rank_lines(&o);
    assert_eq!(l.len(), 1);
    assert!(l[0].starts_with(&format!("rank group={sig} score=27 impact=high occurrences=3 first=2026-10-28 last=2026-11-11")), "{l:?}");
}

#[test]
fn chain_is_one_group_by_the_transitive_closure() {
    let t = Tmp::new();
    t.write_inbox(&[hand("les-001", "2026-11-01", "", ""), hand("les-002", "2026-11-02", "les-001", ""), hand("les-003", "2026-11-03", "les-002", "")]);
    let o = rank(&t, &["--top", "9"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let l = rank_lines(&o);
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(l[0].starts_with("rank group=les-001 score=3 impact=unrated occurrences=3"), "{l:?}");
    assert!(!out(&o).contains("group=les-002") && !out(&o).contains("group=les-003"));
}

#[test]
fn hand_row_breaking_an_event_row_joins_its_signature_group() {
    let t = Tmp::new();
    let sig = "evt.replan_required.2_ab";
    t.write_inbox(&[event(sig, "2026-w44", "2026-10-28", ""), event(sig, "2026-w45", "2026-11-04", ""), hand("h-1", "2026-11-05", &format!("{sig}.2026-w45"), "")]);
    let o = rank(&t, &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let l = rank_lines(&o);
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(l[0].starts_with(&format!("rank group={sig} score=3 impact=unrated occurrences=3")), "{l:?}");
}

#[test]
fn hand_row_breaking_a_pattern_row_ranks_the_pattern_group() {
    let t = Tmp::new();
    t.write_inbox(&[pattern("pat-1", "2026-11-01", ""), pattern("pat-0", "2026-11-01", ""), hand("h-1", "2026-11-05", "pat-1", "high")]);
    let o = rank(&t, &["--top", "9"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let l = rank_lines(&o);
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(l[0].starts_with("rank group=pat-1 score=9 impact=high occurrences=1"), "{l:?}");
    assert!(!out(&o).contains("pat-0"), "an unbroken pattern row is not ranked");
    assert!(out(&o).ends_with("rank ok groups=1 listed=1\n"), "{}", out(&o));
}

#[test]
fn groups_partition_the_inbox() {
    let sig = "evt.gate_fail.3_abc";
    let lines = [
        hand("a-1", "2026-11-01", "", "low"),
        hand("a-2", "2026-11-02", "a-1", ""),
        hand("a-3", "2026-11-03", "a-2", "high"),
        event(sig, "2026-w44", "2026-10-28", "high"),
        event(sig, "2026-w45", "2026-11-04", ""),
        hand("h-1", "2026-11-05", &format!("{sig}.2026-w45"), ""),
        pattern("pat-1", "2026-11-01", ""),
        pattern("pat-0", "2026-11-01", ""),
        hand("h-2", "2026-11-06", "pat-1", ""),
        hand("solo", "2026-11-06", "", ""),
    ];
    let rows: Vec<_> = lines.iter().map(|l| parse_row(l).unwrap()).collect();
    let all = groups(&rows).unwrap();
    // every row in exactly one group
    let mut seen = vec![0; rows.len()];
    for g in &all {
        for &m in &g.members {
            seen[m] += 1;
        }
    }
    assert!(seen.iter().all(|&c| c == 1), "{seen:?}");
    // the member counts of the ranked groups equal the rows minus the unranked pattern rows
    let unranked_rows: usize = all.iter().filter(|g| !g.ranked).map(|g| g.members.len()).sum();
    let ranked_rows: usize = all.iter().filter(|g| g.ranked).map(|g| g.members.len()).sum();
    assert_eq!(unranked_rows, 1, "only pat-0 is unranked");
    assert_eq!(ranked_rows, rows.len() - unranked_rows);
    assert_eq!(ranked(&all).len(), all.iter().filter(|g| g.ranked).count());
}

#[test]
fn a_breaks_cycle_refuses_with_the_lowest_id() {
    let t = Tmp::new();
    t.write_inbox(&[hand("b", "2026-11-01", "a", ""), hand("a", "2026-11-02", "b", "")]);
    let lint = run(&["friction", "lint", "--config", p(&t.config())]);
    assert_eq!(code(&lint), 0, "the 2-cycle is lint-legal: {}", err(&lint));
    let o = rank(&t, &[]);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("rank: breaks cycle through a"), "{}", err(&o));
    assert!(out(&o).is_empty());
    // a tail leading into a cycle names the cycle's lowest id, not the tail
    t.write_inbox(&[hand("z", "2026-11-01", "c", ""), hand("c", "2026-11-02", "d", ""), hand("d", "2026-11-03", "c", "")]);
    let o = rank(&t, &[]);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("rank: breaks cycle through c"), "{}", err(&o));
}

#[test]
fn needle_in_text_why_proof_is_never_printed() {
    let t = Tmp::new();
    let needle = "needlexq7";
    t.write_inbox(&[row("les-001", "2026-11-01", "", &format!("text {needle}"), &[("why", &format!("why {needle}")), ("proof", &format!("cmd:echo {needle}")), ("impact", "high")])]);
    let o = rank(&t, &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!out(&o).contains(needle) && !err(&o).contains(needle));
}

#[test]
fn two_runs_are_byte_identical_and_lint_failure_prints_nothing() {
    let t = Tmp::new();
    t.write_inbox(&[hand("a", "2026-11-01", "", "low"), hand("b", "2026-11-01", "", "low"), hand("c", "2026-11-01", "a", "")]);
    let (o1, o2) = (rank(&t, &[]), rank(&t, &[]));
    assert_eq!(o1.stdout, o2.stdout);
    assert_eq!(code(&o1), 0);
    t.write_inbox(&[hand("a", "2026-11-01", "ghost", "")]);
    let o = rank(&t, &[]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty(), "{}", out(&o));
    assert!(err(&o).contains("breaks names no lesson"), "{}", err(&o));
}

#[test]
fn top_comes_from_the_flag_then_the_config_then_three() {
    let t = Tmp::new();
    let rows: Vec<String> = (1..=5).map(|i| hand(&format!("g-{i}"), "2026-11-01", "", "")).collect();
    t.write_inbox(&rows);
    assert_eq!(rank_lines(&rank(&t, &[])).len(), 3);
    assert_eq!(rank_lines(&rank(&t, &["--top", "1"])).len(), 1);
    let cfg = t.config_with("  triage_top: 2");
    let o = run(&["friction", "rank", "--config", p(&cfg)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(rank_lines(&o).len(), 2);
    for bad in ["0", "x", "-1"] {
        let o = rank(&t, &["--top", bad]);
        assert_eq!(code(&o), 2, "{bad}");
        assert!(out(&o).is_empty());
    }
}

#[test]
fn record_appends_one_trial_line_before_printing() {
    let t = Tmp::new();
    t.write_inbox(&[hand("a", "2026-11-01", "", "high")]);
    let rec = t.0.join("trial.jsonl");
    let o = rank(&t, &["--record", p(&rec)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let body = fs::read_to_string(&rec).unwrap();
    assert_eq!(body.lines().count(), 1);
    assert!(body.contains("\"groups\":1") && body.contains("\"listed\":[\"a\"]"), "{body}");
    // the record may not name the inbox
    let o = rank(&t, &["--record", p(&t.inbox())]);
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
}

// ---- AC-6: friction page writes text only to --out

#[test]
fn page_stdout_is_one_line_and_the_text_goes_only_to_out() {
    let t = Tmp::new();
    let needle = "needlepg9";
    t.write_inbox(&[row("les-001", "2026-11-01", "", &format!("text {needle}"), &[("why", &format!("why {needle}")), ("proof", &format!("cmd:echo {needle}")), ("impact", "high"), ("at", "2026-11-01T01:02:03Z")])]);
    let cfg = t.config();
    let handoff = t.handoff();
    let o = run(&["friction", "triage", "--week", "2026-w45", "--state", p(&t.state()), "--handoff-dir", p(&handoff), "--config", p(&cfg)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let page = t.0.join("page.md");
    let o = run(&["friction", "page", "--week", "2026-w45", "--state", p(&t.state()), "--out", p(&page), "--config", p(&cfg)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "page written groups=1\n");
    assert!(!out(&o).contains(needle) && !err(&o).contains(needle));
    let body = fs::read_to_string(&page).unwrap();
    assert!(body.contains(needle));
    assert!(body.starts_with("---\nkind: note\n"), "{body}");
    assert!(body.contains("# Friction triage 2026-w45") && body.contains("## les-001"), "{body}");
    // --out may not clobber the inbox or the state
    for target in [t.inbox(), t.state()] {
        let before = fs::read(&target).unwrap();
        let o = run(&["friction", "page", "--week", "2026-w45", "--state", p(&t.state()), "--out", p(&target), "--config", p(&cfg)]);
        assert_eq!(code(&o), 2);
        assert_eq!(fs::read(&target).unwrap(), before);
    }
}

#[test]
fn unknown_verbs_and_options_are_usage_errors() {
    let o = run(&["friction", "pagez"]);
    assert_eq!(code(&o), 2);
    let t = Tmp::new();
    t.write_inbox(&[hand("a", "2026-11-01", "", "")]);
    let o = rank(&t, &["--bogus", "x"]);
    assert_eq!(code(&o), 2);
    assert!(!err(&o).contains("bogus"));
}
