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
    run(&["friction", "bind", "--state", p(&state), "--group", group, "--task-id", id, "--task-session", session])
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
    for (g, id, s) in [("g-1", "42x", "s-fake"), ("g-1", "", "s-fake"), ("nope", "4242", "s-fake"), ("g-1", "4243", "s-fake"), ("g-1", "4242", "s-other"), ("g-1", "4242", "S Bad"), ("g-2", "4242", "s-fake")] {
        let o = bind(&t, g, id, s);
        assert_eq!(code(&o), 1, "{g} {id} {s}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert_eq!(fs::read(t.state()).unwrap(), before, "{g} {id} {s}");
    }
    // a missing state file: refused, and not created
    let t2 = Tmp::new();
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
    for bad in [format!("{good}not json\n"), format!("{good}{{\"group\":\"g-1\"}}\n"), good.trim_end().to_string(), good.replace("proposed", "weird")] {
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
