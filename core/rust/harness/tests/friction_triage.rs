//! Ticket 3601, A3/A3b: `harness friction triage` and `harness friction bind`, process-level on scratch files.
//! One cycle: propose -> bind -> resolve (by the BOUND task id only) -> regress -> escalate, plus the refusal
//! arms. Every id, week and date is synthetic; task ids (4242, 4243) never equal a hand-off id.
#![cfg(unix)]
mod friction_common;
use friction_common::*;
use harness::ticket::Ticket;
use std::fs;
use std::path::Path;
use std::process::Output;

const W45: &str = "2026-w45";
const W46: &str = "2026-w46";
const W47: &str = "2026-w47";

fn triage(t: &Tmp, week: &str, extra: &[&str]) -> Output {
    let cfg = t.config();
    let handoff = t.handoff();
    let state = t.state();
    let mut args: Vec<&str> = vec!["friction", "triage", "--week", week, "--state", p(&state), "--handoff-dir", p(&handoff)];
    args.extend_from_slice(extra);
    args.push("--config");
    args.push(p(&cfg));
    run(&args)
}

fn bind(t: &Tmp, group: &str, id: &str, session: &str) -> Output {
    let state = t.state();
    let cfg = t.config();
    run(&["friction", "bind", "--state", p(&state), "--group", group, "--task-id", id, "--task-session", session, "--config", p(&cfg)])
}

/// A closed feed whose one row carries the given extra JSON members (`"pr":"7"`, `"head":"abc1234"`).
fn closed_with(t: &Tmp, id: &str, at: &str, extra: &str) -> std::path::PathBuf {
    t.file("cw.json", format!("{{\"closed_with_evidence\":[{{\"id\":\"{id}\",\"repo\":\"owner/repo\",{extra}\"at\":\"{at}\",\"session\":\"s-fake\"}}]}}"))
}

fn closed_file(t: &Tmp, name: &str, id: &str, session: Option<&str>, at: &str) -> std::path::PathBuf {
    let sess = session.map(|s| format!(",\"session\":\"{s}\"")).unwrap_or_default();
    t.file(name, format!("{{\"in_progress\":0,\"closed_with_evidence\":[{{\"id\":\"{id}\",\"repo\":\"owner/repo\",\"pr\":\"77\",\"at\":\"{at}\",\"subject\":\"s\"{sess}}}],\"close_candidates\":[]}}"))
}

fn state_rows(t: &Tmp) -> Vec<String> {
    fs::read_to_string(t.state()).unwrap_or_default().lines().map(|l| l.to_string()).collect()
}

fn last_status(t: &Tmp, group: &str) -> String {
    let rows = state_rows(t);
    let line = rows.iter().rev().find(|l| l.contains(&format!("\"group\":\"{group}\""))).expect("a state row");
    let k = "\"status\":\"";
    let i = line.find(k).unwrap() + k.len();
    line[i..].split('"').next().unwrap().to_string()
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

fn five(t: &Tmp) {
    t.write_inbox(&[
        hand("g-1", "2026-11-02", "", "blocker"),
        hand("g-2", "2026-11-02", "", "high"),
        hand("g-3", "2026-11-02", "", "medium"),
        hand("g-4", "2026-11-02", "", "low"),
        hand("g-5", "2026-11-02", "", ""),
    ]);
}

#[test]
fn top_k_writes_k_tickets_that_parse() {
    let t = Tmp::new();
    five(&t);
    let o = triage(&t, W45, &["--top", "2"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("propose group=g-1 file=2026-w45-1.json reason=rank"), "{s}");
    assert!(s.contains("propose group=g-2 file=2026-w45-2.json reason=rank"), "{s}");
    assert!(s.contains("triage week=2026-w45 proposed=2 open=2 resolved=0 escalated=0"), "{s}");
    assert!(s.contains("open group=g-1 age_weeks=0 bound=no"), "{s}");
    let dir = t.handoff();
    assert_eq!(files(&dir), vec!["2026-w45-1.json", "2026-w45-2.json"]);
    for f in files(&dir) {
        let text = fs::read_to_string(dir.join(&f)).unwrap();
        let tk = Ticket::parse(&text).unwrap_or_else(|e| panic!("{f}: {e}"));
        assert_eq!(tk.owner_role, "friction_learning");
        assert!(tk.id.starts_with("friction-2026-w45-"));
    }
    // blocker: size L, risk high
    let one = Ticket::parse(&fs::read_to_string(dir.join("2026-w45-1.json")).unwrap()).unwrap();
    assert_eq!((one.size.as_str(), one.risk.as_str()), ("L", "high"));
    let two = Ticket::parse(&fs::read_to_string(dir.join("2026-w45-2.json")).unwrap()).unwrap();
    assert_eq!((two.size.as_str(), two.risk.as_str()), ("M", "normal"));
}

#[test]
fn twice_broken_is_proposed_even_below_top_k() {
    let t = Tmp::new();
    t.write_inbox(&[
        hand("big", "2026-11-02", "", "blocker"),
        hand("les-1", "2026-11-02", "", ""),
        hand("les-2", "2026-11-03", "les-1", ""),
        hand("les-3", "2026-11-04", "les-1", ""),
    ]);
    let o = triage(&t, W45, &["--top", "1"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("propose group=big file=2026-w45-1.json reason=rank"), "{s}");
    assert!(s.contains("propose group=les-1 file=2026-w45-2.json reason=twice-broken"), "{s}");
}

#[test]
fn chain_is_proposed_twice_broken_below_top_k() {
    let t = Tmp::new();
    t.write_inbox(&[
        hand("big", "2026-11-02", "", "blocker"),
        hand("les-001", "2026-11-02", "", ""),
        hand("les-002", "2026-11-03", "les-001", ""),
        hand("les-003", "2026-11-04", "les-002", ""),
    ]);
    let o = triage(&t, W45, &["--top", "1"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("propose group=les-001 file=2026-w45-2.json reason=twice-broken"), "{s}");
    assert!(!s.contains("group=les-002"), "{s}");
}

#[test]
fn same_week_rerun_proposes_nothing() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "2"])), 0);
    let state = fs::read(t.state()).unwrap();
    let dir = files(&t.handoff());
    let o = triage(&t, W45, &["--top", "2"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("triage week=2026-w45 proposed=0 open=2"), "{}", out(&o));
    assert!(!out(&o).contains("propose group"));
    assert_eq!(fs::read(t.state()).unwrap(), state);
    assert_eq!(files(&t.handoff()), dir);
    // a later week does not re-propose either, and the open list shows its age
    let o = triage(&t, W47, &["--top", "2"]);
    assert!(out(&o).contains("proposed=0") && out(&o).contains("open group=g-1 age_weeks=2 bound=no"), "{}", out(&o));
}

#[test]
fn bind_then_closed_feed_resolves_by_task_id_only() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    let o = bind(&t, "g-1", "4242", "s-fake");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "bind group=g-1 task=4242\n");
    assert_eq!(out(&bind(&t, "g-1", "4242", "s-fake")), "bind already group=g-1\n");
    // negative: the hand-off id never matches
    let c = closed_file(&t, "c1.json", "friction-2026-w45-1", Some("s-fake"), "2026-11-10");
    let o = triage(&t, W46, &["--top", "1", "--closed", p(&c)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(last_status(&t, "g-1"), "proposed");
    assert!(out(&o).contains("open group=g-1 age_weeks=1 bound=yes"), "{}", out(&o));
    // negative: the right id under another session
    let c = closed_file(&t, "c2.json", "4242", Some("s-other"), "2026-11-10");
    assert_eq!(code(&triage(&t, W46, &["--top", "1", "--closed", p(&c)])), 0);
    assert_eq!(last_status(&t, "g-1"), "proposed");
    // positive
    let c = closed_file(&t, "c3.json", "4242", Some("s-fake"), "2026-11-10");
    let o = triage(&t, W46, &["--top", "1", "--closed", p(&c)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(last_status(&t, "g-1"), "resolved");
    assert!(out(&o).contains("resolved=1"), "{}", out(&o));
    assert!(fs::read_to_string(t.state()).unwrap().contains("\"resolved_pr\":\"owner/repo#77\""));
}

#[test]
fn unbound_group_never_resolves_and_shows_bound_no() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    for id in ["4242", "friction-2026-w45-1", "1"] {
        let c = closed_file(&t, "c.json", id, Some("s-fake"), "2026-11-10");
        let o = triage(&t, W46, &["--top", "1", "--closed", p(&c)]);
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert_eq!(last_status(&t, "g-1"), "proposed");
        assert!(out(&o).contains("open group=g-1 age_weeks=1 bound=no"), "{}", out(&o));
    }
}

#[test]
fn bind_refusals_leave_state_byte_identical() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    assert_eq!(code(&bind(&t, "g-1", "4242", "s-fake")), 0);
    let before = fs::read(t.state()).unwrap();
    // Changed by the word-id and no-state-row criteria: "42x" is a legal id now, so "4 2x" stands in; "g-2" has
    // inbox rows, so bind files it now, and "g-1b" (no inbox row) stands in as a refusal.
    for (g, id, s) in [("g-1", "4 2x", "s-fake"), ("g-1", "", "s-fake"), ("nope", "4242", "s-fake"), ("g-1", "4243", "s-fake"), ("g-1", "4242", "s-other"), ("g-1", "4242", "S Bad"), ("g-1b", "4242", "s-fake")] {
        let o = bind(&t, g, id, s);
        assert_eq!(code(&o), 1, "{g} {id} {s}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert_eq!(fs::read(t.state()).unwrap(), before, "{g} {id} {s}");
    }
    // a missing state file and a group with no inbox row: refused, and not created
    let t2 = Tmp::new();
    t2.write_inbox(&[]);
    assert_eq!(code(&bind(&t2, "g-1", "1", "s-x")), 1);
    assert!(!t2.state().exists());
}

fn cycle_to_resolved(t: &Tmp) {
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "high")]);
    assert_eq!(code(&triage(t, W45, &[])), 0);
    assert_eq!(code(&bind(t, "g-1", "4242", "s-fake")), 0);
    let c = closed_file(t, "c.json", "4242", Some("s-fake"), "2026-11-10");
    assert_eq!(code(&triage(t, W46, &["--closed", p(&c)])), 0);
    assert_eq!(last_status(t, "g-1"), "resolved");
}

#[test]
fn regression_reproposes_first_and_clears_the_binding_then_escalates() {
    let t = Tmp::new();
    cycle_to_resolved(&t);
    // a later row (dated after resolved_date) in the group
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "high"), hand("g-1b", "2026-11-12", "g-1", ""), hand("fresh", "2026-11-12", "", "blocker")]);
    let o = triage(&t, W47, &["--top", "1"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let s = out(&o);
    let i_reg = s.find("propose group=g-1 file=2026-w47-1.json reason=regressed").unwrap_or_else(|| panic!("{s}"));
    let i_new = s.find("propose group=fresh").unwrap_or_else(|| panic!("{s}"));
    assert!(i_reg < i_new, "regressed goes first: {s}");
    assert_eq!(last_status(&t, "g-1"), "proposed");
    let last = state_rows(&t).into_iter().rev().find(|l| l.contains("\"group\":\"g-1\"")).unwrap();
    assert!(last.contains("\"task_id\":\"\"") && last.contains("\"task_session\":\"\"") && last.contains("\"attempts\":\"1\""), "{last}");
    assert!(s.contains("open group=g-1 age_weeks=0 bound=no"), "{s}");
    // rebind, resolve again, regress again: escalate and no new file
    assert_eq!(code(&bind(&t, "g-1", "4243", "s-fake")), 0);
    let c = closed_file(&t, "c2.json", "4243", Some("s-fake"), "2026-11-13");
    let w48 = "2026-w48";
    assert_eq!(code(&triage(&t, w48, &["--top", "1", "--closed", p(&c)])), 0);
    assert_eq!(last_status(&t, "g-1"), "resolved");
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "high"), hand("g-1b", "2026-11-12", "g-1", ""), hand("g-1c", "2026-11-20", "g-1", ""), hand("fresh", "2026-11-12", "", "blocker")]);
    let before = files(&t.handoff());
    let o = triage(&t, "2026-w49", &["--top", "1"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("escalate group=g-1 attempts=2"), "{s}");
    assert!(!s.contains("propose group=g-1"), "{s}");
    assert_eq!(last_status(&t, "g-1"), "escalated");
    assert_eq!(files(&t.handoff()), before, "no file for an escalated group");
    // and it is never proposed again
    let o = triage(&t, "2026-w50", &["--top", "9"]);
    assert!(!out(&o).contains("propose group=g-1") && out(&o).contains("escalated=1"), "{}", out(&o));
}

fn sha(path: &Path) -> String {
    // a cheap content fingerprint (length + bytes) is enough for byte-identity checks; compare the bytes themselves
    String::from_utf8_lossy(&fs::read(path).unwrap_or_default()).to_string()
}

#[test]
fn a_corrupt_state_or_feed_refuses_and_changes_nothing() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    let log = t.file("events.jsonl", format!("{}\n", "{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":1793577600000,\"run_id\":\"run-1\",\"ticket\":\"193\",\"repo\":\"ai-brain\",\"reason\":\"replan-threshold\",\"role\":\"executor\",\"model\":\"worker\"}"));
    let good = fs::read_to_string(t.state()).unwrap();
    for bad in [format!("{good}not json\n"), format!("{good}{{\"group\":\"g-1\"}}\n"), good.trim_end().to_string(), good.replace("proposed", "weird"), good.replace("\"task_id\":\"\"", "\"task_id\":\"-x\"")] {
        fs::write(t.state(), &bad).unwrap();
        let (s0, i0, d0) = (sha(&t.state()), sha(&t.inbox()), files(&t.handoff()));
        let o = triage(&t, W46, &["--top", "9", "--log", p(&log)]);
        assert_ne!(code(&o), 0, "{bad}");
        assert!(out(&o).is_empty());
        assert_eq!((sha(&t.state()), sha(&t.inbox()), files(&t.handoff())), (s0, i0, d0), "{bad}");
    }
    fs::write(t.state(), &good).unwrap();
    let c = t.file("bad.json", "{\"closed_with_evidence\":[{\"id\":\"1\"}]}");
    let (s0, i0, d0) = (sha(&t.state()), sha(&t.inbox()), files(&t.handoff()));
    let o = triage(&t, W46, &["--top", "9", "--log", p(&log), "--closed", p(&c)]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!((sha(&t.state()), sha(&t.inbox()), files(&t.handoff())), (s0, i0, d0));
    // an inbox that fails lint refuses with the state and the hand-off folder untouched
    t.write_inbox(&[hand("a", "2026-11-01", "ghost", "")]);
    let (s0, d0) = (sha(&t.state()), files(&t.handoff()));
    let o = triage(&t, W46, &["--top", "9"]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    assert_eq!((sha(&t.state()), files(&t.handoff())), (s0, d0));
}

#[test]
fn bind_takes_a_word_id_and_refuses_other_characters() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    let before = fs::read(t.state()).unwrap();
    let long = "a".repeat(65);
    for id in ["a b", "a/b", "-x", ".x", "a;b", "a\u{e9}", long.as_str()] {
        let o = bind(&t, "g-1", id, "s-fake");
        assert_eq!(code(&o), 1, "{id}: {}", err(&o));
        assert_eq!(fs::read(t.state()).unwrap(), before, "{id}");
    }
    // 64 bytes is the cap: accepted
    let o = bind(&t, "g-2", &"a".repeat(64), "s-fake");
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = bind(&t, "g-1", "admission-two-lanes", "s-fake");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(state_rows(&t).last().unwrap().contains("\"task_id\":\"admission-two-lanes\""));
}

#[test]
fn bind_refuses_a_task_id_on_the_privacy_list() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    let before = fs::read(t.state()).unwrap();
    let o = bind(&t, "g-1", DENY, "s-fake");
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("privacy hit"), "{}", err(&o));
    assert_eq!(fs::read(t.state()).unwrap(), before);
}

#[test]
fn bind_refuses_a_group_whose_week_the_state_reader_would_refuse() {
    let t = Tmp::new();
    t.write_inbox(&[hand("g-9", "9999-12-31", "", "high")]);
    let o = bind(&t, "g-9", "word-id", "s-fake");
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(!t.state().exists());
}

#[test]
fn a_closed_at_with_a_broken_time_refuses_the_feed() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    assert_eq!(code(&bind(&t, "g-1", "4242", "s-fake")), 0);
    let before = fs::read(t.state()).unwrap();
    for at in ["2026-11-10T10:00:00z", "2026-11-10T10:00:00+08:00", "2026-11-10T10:00:00.5Z", "2026-11-10T25:00:00Z", "2026-11-10junk"] {
        let c = closed_with(&t, "4242", at, "\"pr\":\"7\",");
        let o = triage(&t, W46, &["--top", "1", "--closed", p(&c)]);
        assert_eq!(code(&o), 1, "{at}: {}", err(&o));
        assert_eq!(fs::read(t.state()).unwrap(), before, "{at}");
    }
}

#[test]
fn bind_files_a_group_that_has_no_state_row() {
    let t = Tmp::new();
    five(&t);
    let o = bind(&t, "g-2", "word.id_1", "s-fake");
    assert_eq!(code(&o), 0, "{}", err(&o));
    let rows = state_rows(&t);
    assert_eq!(rows.len(), 1, "{rows:?}");
    for want in ["\"group\":\"g-2\"", "\"status\":\"proposed\"", "\"proposed_week\":\"2026-w45\"", "\"task_id\":\"word.id_1\"", "\"ticket_file\":\"\""] {
        assert!(rows[0].contains(want), "{want}: {}", rows[0]);
    }
    // the filed group resolves by its word id, and a later triage does not propose it again
    let c = closed_with(&t, "word.id_1", "2026-11-10", "\"pr\":\"5\",");
    let o = triage(&t, W46, &["--top", "5", "--closed", p(&c)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(last_status(&t, "g-2"), "resolved");
    assert!(!out(&o).contains("propose group=g-2"), "{}", out(&o));
    // a group with no inbox row: refused, state byte-identical
    let before = fs::read(t.state()).unwrap();
    let o = bind(&t, "no-such-group", "word.id_2", "s-fake");
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(fs::read(t.state()).unwrap(), before);
}

#[test]
fn closed_row_with_head_resolves_and_neither_refuses() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    assert_eq!(code(&bind(&t, "g-1", "4242", "s-fake")), 0);
    let before = fs::read(t.state()).unwrap();
    for extra in ["", "\"head\":\"abc12\",", "\"head\":\"zzzzzzz\",", "\"head\":\"abc1234abc1234abc1234abc1234abc1234abc1234\",", "\"pr\":\"x\","] {
        let c = closed_with(&t, "4242", "2026-11-10", extra);
        let o = triage(&t, W46, &["--top", "1", "--closed", p(&c)]);
        assert_eq!(code(&o), 1, "{extra}: {}", err(&o));
        assert_eq!(fs::read(t.state()).unwrap(), before, "{extra}");
    }
    let c = closed_with(&t, "4242", "2026-11-10", "\"head\":\"abc1234\",");
    let o = triage(&t, W46, &["--top", "1", "--closed", p(&c)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(last_status(&t, "g-1"), "resolved");
    assert!(fs::read_to_string(t.state()).unwrap().contains("\"resolved_pr\":\"owner/repo@abc1234\""));
}

#[test]
fn a_closed_pr_that_is_not_a_string_or_a_number_refuses_the_feed() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    assert_eq!(code(&bind(&t, "g-1", "4242", "s-fake")), 0);
    let before = fs::read(t.state()).unwrap();
    for pr in ["true", "{}", "[\"7\"]", "null"] {
        let c = closed_with(&t, "4242", "2026-11-10", &format!("\"pr\":{pr},\"head\":\"abc1234\","));
        let o = triage(&t, W46, &["--top", "1", "--closed", p(&c)]);
        assert_eq!(code(&o), 1, "{pr}: {}", err(&o));
        assert_eq!(fs::read(t.state()).unwrap(), before, "{pr}");
    }
}

/// g-1 resolved at 2026-11-10T12:00:00Z; the inbox then gains one member row.
fn resolved_at_noon_then(t: &Tmp, member: String) -> Output {
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "high")]);
    assert_eq!(code(&triage(t, W45, &[])), 0);
    assert_eq!(code(&bind(t, "g-1", "4242", "s-fake")), 0);
    let c = closed_with(t, "4242", "2026-11-10T12:00:00Z", "\"pr\":\"77\",");
    assert_eq!(code(&triage(t, W46, &["--closed", p(&c)])), 0);
    assert!(fs::read_to_string(t.state()).unwrap().contains("\"resolved_date\":\"2026-11-10T12:00:00Z\""));
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "high"), member]);
    triage(t, W47, &[])
}

#[test]
fn closed_time_is_kept_and_orders_same_day_rows() {
    let day = "2026-11-10";
    let with_at = |at: &str| row("g-1b", day, "g-1", "lesson", &[("at", at)]);
    let attempts = |t: &Tmp| state_rows(t).last().unwrap().contains("\"attempts\":\"1\"");
    // later the same day: regressed (re-proposed with attempts 1)
    let t = Tmp::new();
    assert_eq!(code(&resolved_at_noon_then(&t, with_at("2026-11-10T13:00:00Z"))), 0);
    assert!(attempts(&t), "{:?}", state_rows(&t));
    // earlier the same day, and a row without at on the same day: not regressed
    for m in [with_at("2026-11-10T11:00:00Z"), row("g-1b", day, "g-1", "lesson", &[])] {
        let t = Tmp::new();
        assert_eq!(code(&resolved_at_noon_then(&t, m)), 0);
        assert_eq!(last_status(&t, "g-1"), "resolved");
    }
    // a date-only closed at keeps the date rule: a row with an at on the same day does not regress
    let t = Tmp::new();
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "high")]);
    assert_eq!(code(&triage(&t, W45, &[])), 0);
    assert_eq!(code(&bind(&t, "g-1", "4242", "s-fake")), 0);
    let c = closed_with(&t, "4242", "2026-11-10", "\"pr\":\"77\",");
    assert_eq!(code(&triage(&t, W46, &["--closed", p(&c)])), 0);
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "high"), with_at("2026-11-10T01:00:00Z")]);
    assert_eq!(code(&triage(&t, W47, &[])), 0);
    assert_eq!(last_status(&t, "g-1"), "resolved");
}

#[test]
fn a_state_file_of_the_older_binary_is_read() {
    let t = Tmp::new();
    five(&t);
    let line = "{\"group\":\"g-1\",\"status\":\"resolved\",\"ticket_file\":\"2026-w45-1.json\",\"proposed_week\":\"2026-w45\",\"task_id\":\"4242\",\"task_session\":\"s-fake\",\"resolved_pr\":\"owner/repo#77\",\"resolved_date\":\"2026-11-10\",\"attempts\":\"0\"}\n{\"group\":\"g-2\",\"status\":\"proposed\",\"ticket_file\":\"2026-w45-2.json\",\"proposed_week\":\"2026-w45\",\"task_id\":\"\",\"task_session\":\"\",\"resolved_pr\":\"\",\"resolved_date\":\"\",\"attempts\":\"0\"}\n";
    fs::write(t.state(), line).unwrap();
    let cfg = t.config();
    let page = t.0.join("page.md");
    let o = run(&["friction", "page", "--week", W46, "--state", p(&t.state()), "--out", p(&page), "--config", p(&cfg)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(fs::read_to_string(&page).unwrap().contains("## g-2"));
    assert_eq!(code(&triage(&t, W46, &["--top", "1"])), 0);
}

#[test]
fn log_runs_the_existing_sweep_first() {
    let t = Tmp::new();
    let log = t.file("events.jsonl", "{\"v\":2,\"kind\":\"gate_fail\",\"ts_ms\":1793577600000,\"ticket\":\"199\",\"repo\":\"bridlework-harness\",\"gate_id\":\"rust-test\",\"failure_class\":\"suite-fail\",\"head_sha\":\"aa11bb22cc33dd44ee55ff6600112233445566aa\",\"result\":\"fail\"}\n");
    fs::write(t.inbox(), "").unwrap();
    let o = triage(&t, W45, &["--log", p(&log)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let s = out(&o);
    assert!(s.contains("stored id=evt.gate_fail."), "{s}");
    assert!(s.contains("propose group=evt.gate_fail.") && s.contains("reason=rank"), "{s}");
    let tk = Ticket::parse(&fs::read_to_string(t.handoff().join("2026-w45-1.json")).unwrap()).unwrap();
    assert_eq!(tk.risk, "normal");
}

#[test]
fn usage_errors() {
    let t = Tmp::new();
    five(&t);
    for args in [vec!["--week", "w45"], vec!["--bogus", "x"], vec!["--week", W45]] {
        let o = run(&[&["friction", "triage"][..], &args[..]].concat());
        assert_eq!(code(&o), 2);
        assert!(out(&o).is_empty());
    }
}

// ---- --controls (FT-2): a valid controls line seeds a group as resolved ----

/// A lessons folder holding the regular file ISSUE-001-x.md, and a controls file with the given text.
fn controls(t: &Tmp, text: impl AsRef<[u8]>) -> (std::path::PathBuf, std::path::PathBuf) {
    let l = t.0.join("lessons");
    fs::create_dir_all(&l).unwrap();
    // ISSUE-01 and ISSUE-0001 have files too, so only the three-digit rule can refuse them.
    for f in ["ISSUE-001-x.md", "ISSUE-01-x.md", "ISSUE-0001-x.md"] {
        fs::write(l.join(f), "lesson").unwrap();
    }
    (t.file("controls.txt", text), l)
}

fn triage_c(t: &Tmp, week: &str, c: &Path, l: &Path) -> Output {
    triage(t, week, &["--top", "1", "--controls", p(c), "--lessons-dir", p(l)])
}

const LINE: &str = "g-1 | control: a guard pointer | lesson: ISSUE-001\n";

#[test]
fn a_controls_line_seeds_the_group_as_resolved() {
    let t = Tmp::new();
    five(&t);
    let (c, l) = controls(&t, format!("# a comment\n\n  {}", LINE.replace('\n', "\r\n")));
    let o = triage_c(&t, W45, &c, &l);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("seed group=g-1 line=3"), "{}", out(&o));
    assert!(!out(&o).contains("propose group=g-1") && !out(&o).contains("a guard pointer"), "{}", out(&o));
    assert_eq!(last_status(&t, "g-1"), "resolved");
    assert!(state_rows(&t)[0].contains("\"resolved_date\":\"2026-11-02\"") && state_rows(&t)[0].contains("\"task_id\":\"\""));
    // a group in the state is not seeded again
    let n = state_rows(&t).len();
    let o = triage_c(&t, W45, &c, &l);
    assert!(!out(&o).contains("seed group="), "{}", out(&o));
    assert_eq!(state_rows(&t).len(), n);
    // a later repeat regresses it (re-proposed with attempts 1)
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "blocker"), hand("g-1b", "2026-11-09", "g-1", "")]);
    let o = triage_c(&t, W46, &c, &l);
    assert!(out(&o).contains("propose group=g-1 ") && out(&o).contains("reason=regressed"), "{}", out(&o));
    assert!(state_rows(&t).last().unwrap().contains("\"attempts\":\"1\""));
}

#[test]
fn a_seed_keeps_the_at_of_the_latest_member_row() {
    let t = Tmp::new();
    t.write_inbox(&[row("g-1", "2026-11-02", "", "lesson", &[]), row("g-1b", "2026-11-09", "g-1", "lesson", &[("at", "2026-11-09T08:00:00Z")])]);
    let (c, l) = controls(&t, "g-1 | control: x | lesson: ISSUE-001\n");
    let o = triage_c(&t, W45, &c, &l);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(state_rows(&t)[0].contains("\"resolved_date\":\"2026-11-09T08:00:00Z\""), "{}", state_rows(&t)[0]);
}

#[test]
fn a_proposed_unbound_group_is_seeded_and_keeps_its_fields() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    let (c, l) = controls(&t, LINE);
    let o = triage_c(&t, W46, &c, &l);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("seed group=g-1 line=1"), "{}", out(&o));
    let last = state_rows(&t).into_iter().rev().find(|l| l.contains("\"group\":\"g-1\"")).unwrap();
    for want in ["\"status\":\"resolved\"", "\"ticket_file\":\"2026-w45-1.json\"", "\"proposed_week\":\"2026-w45\"", "\"attempts\":\"0\"", "\"task_id\":\"\"", "\"resolved_date\":\"2026-11-02\""] {
        assert!(last.contains(want), "{want}: {last}");
    }
    // a second run with the same file writes no seed row
    let n = state_rows(&t).len();
    let o = triage_c(&t, W46, &c, &l);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!out(&o).contains("seed group="), "{}", out(&o));
    assert_eq!(state_rows(&t).len(), n);
}

#[test]
fn a_bound_group_gets_no_row_from_the_controls_file() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    assert_eq!(code(&bind(&t, "g-1", "4242", "s-fake")), 0);
    let n = state_rows(&t).iter().filter(|l| l.contains("\"group\":\"g-1\"")).count();
    let (c, l) = controls(&t, LINE);
    let o = triage_c(&t, W46, &c, &l);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!out(&o).contains("seed group=g-1"), "{}", out(&o));
    assert_eq!(last_status(&t, "g-1"), "proposed");
    assert_eq!(state_rows(&t).iter().filter(|l| l.contains("\"group\":\"g-1\"")).count(), n);
}

#[test]
fn a_line_that_is_not_a_group_root_or_not_a_row_is_reported_and_not_seeded() {
    let t = Tmp::new();
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "blocker"), hand("g-1b", "2026-11-03", "g-1", "")]);
    let (c, l) = controls(&t, "g-1b | control: x | lesson: ISSUE-001\nzz-none | control: x | lesson: ISSUE-001\n");
    let o = triage_c(&t, W45, &c, &l);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("control-not-root line=1 group=g-1") && out(&o).contains("control-unknown line=2"), "{}", out(&o));
    assert!(!out(&o).contains("zz-none") && !out(&o).contains("seed group="), "{}", out(&o));
}

#[test]
fn a_bad_controls_file_refuses_with_nothing_changed() {
    let t = Tmp::new();
    five(&t);
    assert_eq!(code(&triage(&t, W45, &["--top", "1"])), 0);
    let before = fs::read(t.state()).unwrap();
    let bad: Vec<Vec<u8>> = [
        "g-1 | control: x\n",
        "g-1 | control:  | lesson: ISSUE-001\n",
        "g-1 | control:x | lesson: ISSUE-001\n",
        "g-1 | ctl: x | lesson: ISSUE-001\n",
        "g-1 | control: x | lesson: ISSUE-01\n",
        "g-1 | control: x | lesson: ISSUE-0001\n",
        "g-1 | control: x | lesson: ISSUE-002\n",
        "g 1 | control: x | lesson: ISSUE-001\n",
        "g-1 | control: x | y | lesson: ISSUE-001\n",
        "g-1 | control: a\u{1b}b | lesson: ISSUE-001\n",
        "g-1 | control: a\rb | lesson: ISSUE-001\n",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .chain([b"g-1 | control: \xff | lesson: ISSUE-001\n".to_vec()])
    .collect();
    for b in bad {
        let (c, l) = controls(&t, &b);
        let o = triage_c(&t, W46, &c, &l);
        assert_eq!(code(&o), 1, "{:?}: {}", String::from_utf8_lossy(&b), err(&o));
        assert!(out(&o).is_empty() && !err(&o).contains("control: x"), "{}", err(&o));
        assert_eq!(fs::read(t.state()).unwrap(), before);
    }
    // a lesson that is only a symbolic link or a folder does not count
    let (c, l) = controls(&t, "g-1 | control: x | lesson: ISSUE-003\n");
    std::os::unix::fs::symlink(l.join("ISSUE-001-x.md"), l.join("ISSUE-003-a.md")).unwrap();
    fs::create_dir_all(l.join("ISSUE-003-b.md")).unwrap();
    assert_eq!(code(&triage_c(&t, W46, &c, &l)), 1);
    assert_eq!(fs::read(t.state()).unwrap(), before);
}

#[test]
fn a_missing_controls_input_is_a_setup_error() {
    let t = Tmp::new();
    five(&t);
    let (c, l) = controls(&t, LINE);
    let big = t.file("big.txt", vec![b'#'; 1024 * 1024 + 1]);
    for (cf, lf) in [(t.0.join("none.txt"), l.clone()), (l.clone(), l.clone()), (big, l.clone()), (c.clone(), t.0.join("no-dir"))] {
        let o = triage_c(&t, W45, &cf, &lf);
        assert_eq!(code(&o), 2, "{}", err(&o));
        assert!(!t.state().exists());
    }
    for half in [["--controls", p(&c)], ["--lessons-dir", p(&l)]] {
        assert_eq!(code(&triage(&t, W45, &half)), 2);
    }
    // an empty controls file is valid and seeds nothing
    let (c, l) = controls(&t, "");
    assert_eq!(code(&triage_c(&t, W45, &c, &l)), 0);
}
