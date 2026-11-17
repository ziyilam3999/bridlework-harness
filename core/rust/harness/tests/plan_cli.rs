//! Process-level tests for `harness plan check|status`. Most tests build their own folders with made-up ids, run the real binary and
//! assert the stdout, stderr and exit code (a few assert only part of the output). No test calls `std::env::var`.
#![cfg(unix)]
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const USAGE: &str = "usage: harness plan check|status --index <absolute-file> --tickets <absolute-dir> --done <absolute-dir> [--ticket <id>]";
static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct Out {
    code: i32,
    out: String,
    err: String,
}

struct Env {
    root: PathBuf,
}

impl Env {
    fn new() -> Env {
        Env::named("plan")
    }

    fn named(tag: &str) -> Env {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("{tag}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("tickets")).unwrap();
        fs::create_dir_all(p.join("done")).unwrap();
        Env { root: p.canonicalize().unwrap() }
    }

    fn index(&self, rows: &[String]) {
        fs::write(self.root.join("index.jsonl"), rows.join("\n") + "\n").unwrap();
    }

    fn ticket(&self, id: &str, size: &str, link: &str) {
        fs::write(self.root.join("tickets").join(format!("{id}.json")), ticket_json(id, size, link)).unwrap();
    }

    fn done(&self, id: &str) {
        fs::write(self.root.join("done").join(format!("{id}.done")), format!("ticket={id}\nat=1\n")).unwrap();
    }

    fn run(&self, sub: &str, extra: &[&str]) -> Out {
        let mut c = Command::new(BIN);
        c.arg("plan").arg(sub);
        c.arg("--index").arg(self.root.join("index.jsonl"));
        c.arg("--tickets").arg(self.root.join("tickets"));
        c.arg("--done").arg(self.root.join("done"));
        c.args(extra);
        let o = c.output().unwrap();
        Out { code: o.status.code().unwrap(), out: String::from_utf8(o.stdout).unwrap(), err: String::from_utf8(o.stderr).unwrap() }
    }

    fn check(&self) -> Out {
        self.run("check", &[])
    }

    fn status(&self) -> Out {
        self.run("status", &[])
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const DEV_OK: &str = r#"{"reason":"r","decided_by":"operator","date":"2026-01-01"}"#;

fn row(id: &str, state: &str, tickets: &[&str], dev: &str) -> String {
    let t: Vec<String> = tickets.iter().map(|t| format!("\"{t}\"")).collect();
    format!(r#"{{"id":"{id}","title":"t","plan_line":3,"state":"{state}","tickets":[{}],"deviation":{dev},"trigger":null,"code":[]}}"#, t.join(","))
}

fn r(id: &str, state: &str) -> String {
    row(id, state, &[], "null")
}

fn ticket_json(id: &str, size: &str, link: &str) -> String {
    format!(
        r#"{{"id":"{id}","repo_id":"scratch-1","title":"t","group":"g","size":"{size}","objective":"o","acceptance_criteria":["a"],"dependencies":[],"risk":"normal","owner_role":"execution_verification","evidence_links":[]{link}}}"#
    )
}

fn names(refs: &str) -> String {
    format!(r#","main_plan_ref":[{refs}]"#)
}

fn off(reason: &str) -> String {
    format!(r#","off_plan":"{reason}""#)
}

fn is(o: &Out, code: i32, out: &str) {
    assert_eq!(o.err, "", "stderr");
    assert_eq!(o.out, out, "stdout");
    assert_eq!(o.code, code, "exit code");
}

fn refused(o: &Out, err: &str) {
    assert_eq!(o.out, "", "stdout");
    assert_eq!(o.err, format!("{err}\n"), "stderr");
    assert_eq!(o.code, 2, "exit code");
}

// ---- check

#[test]
fn tickets_are_ordered_by_id_not_file_name() {
    // "a-b.json" sorts before "a.json" by name, but the id "a" sorts before "a-b".
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    for id in ["a", "a-b"] {
        e.ticket(id, "S", "");
        e.done(id);
    }
    is(&e.check(), 1, "red done-ticket-no-row a\nred done-ticket-no-row a-b\nplan check red findings=2\n");
}

#[test]
fn a_built_row_with_no_ticket_is_red() {
    let e = Env::new();
    e.index(&[r("R-1", "built")]);
    is(&e.check(), 1, "red built-no-done-ticket R-1\nplan check red findings=1\n");
}

#[test]
fn a_built_row_whose_ticket_has_no_done_record_is_red() {
    let e = Env::new();
    e.index(&[row("R-1", "built", &["T-1"], "null")]);
    e.ticket("T-1", "S", "");
    is(&e.check(), 1, "red built-no-done-ticket R-1\nplan check red findings=1\n");
}

#[test]
fn a_built_row_listing_a_missing_ticket_file_is_red() {
    let e = Env::new();
    e.index(&[row("R-1", "built", &["T-1"], "null")]);
    e.done("T-1");
    is(&e.check(), 1, "red built-no-done-ticket R-1\nred done-record-no-ticket T-1\nplan check red findings=2\n");
}

#[test]
fn a_built_row_with_a_done_listed_ticket_is_green() {
    let e = Env::new();
    e.index(&[row("R-1", "built", &["T-1"], "null")]);
    e.ticket("T-1", "S", "");
    e.done("T-1");
    is(&e.check(), 0, "plan check ok rows=1 tickets=1 done=1\n");
}

#[test]
fn a_built_row_named_by_a_done_ticket_is_green() {
    let e = Env::new();
    e.index(&[r("R-1", "built")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    e.done("T-1");
    is(&e.check(), 0, "plan check ok rows=1 tickets=1 done=1\n");
}

#[test]
fn a_done_ticket_with_no_row_and_no_reason_is_red() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", "");
    e.done("T-1");
    is(&e.check(), 1, "red done-ticket-no-row T-1\nplan check red findings=1\n");
}

#[test]
fn a_done_ticket_listed_by_a_row_is_linked() {
    let e = Env::new();
    e.index(&[row("R-1", "not-built", &["T-1"], "null")]);
    e.ticket("T-1", "S", "");
    e.done("T-1");
    is(&e.check(), 0, "plan check ok rows=1 tickets=1 done=1\n");
}

#[test]
fn a_done_ticket_naming_a_row_is_linked() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    e.done("T-1");
    is(&e.check(), 0, "plan check ok rows=1 tickets=1 done=1\n");
}

#[test]
fn a_done_off_plan_ticket_is_not_red() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", &off("why"));
    e.done("T-1");
    is(&e.check(), 0, "plan check ok rows=1 tickets=1 done=1\n");
}

#[test]
fn a_ticket_naming_an_id_not_in_the_index_is_red() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", &names(r#""X-9""#));
    is(&e.check(), 1, "red ticket-names-unknown-row T-1 X-9\nplan check red findings=1\n");
    e.done("T-1");
    is(&e.check(), 1, "red ticket-names-unknown-row T-1 X-9\nred done-ticket-no-row T-1\nplan check red findings=2\n");
}

fn deviation_case(dev: &str, want: &str) {
    let e = Env::new();
    e.index(&[row("R-1", "deviation", &[], dev)]);
    is(&e.check(), 1, want);
}

#[test]
fn a_deviation_row_with_no_decided_by_is_red() {
    deviation_case(r#"{"reason":"r","date":"d"}"#, "red deviation-no-decided-by R-1\nplan check red findings=1\n");
}

#[test]
fn a_deviation_row_with_no_reason_is_red() {
    deviation_case(r#"{"decided_by":"operator","date":"d"}"#, "red deviation-no-reason R-1\nplan check red findings=1\n");
}

#[test]
fn a_deviation_row_with_no_date_is_red() {
    deviation_case(r#"{"reason":"r","decided_by":"operator"}"#, "red deviation-no-date R-1\nplan check red findings=1\n");
}

#[test]
fn a_deviation_row_with_a_null_deviation_is_red() {
    deviation_case("null", "red deviation-no-reason R-1\nred deviation-no-decided-by R-1\nred deviation-no-date R-1\nplan check red findings=3\n");
}

#[test]
fn a_deviation_row_with_an_unknown_decided_by_is_red() {
    deviation_case(r#"{"reason":"r","decided_by":"me","date":"d"}"#, "red deviation-bad-decided-by R-1\nplan check red findings=1\n");
}

#[test]
fn a_complete_deviation_row_is_green() {
    let e = Env::new();
    e.index(&[row("R-1", "deviation", &[], DEV_OK), row("R-2", "deviation", &[], r#"{"reason":"r","decided_by":"orchestrator-reading","date":"d"}"#)]);
    is(&e.check(), 0, "plan check ok rows=2 tickets=0 done=0\n");
}

#[test]
fn two_rows_with_one_id_are_red() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built"), r("R-1", "not-built"), r("R-1", "not-built")]);
    is(&e.check(), 1, "red duplicate-id R-1\nplan check red findings=1\n");
}

#[test]
fn an_unknown_state_row_is_red() {
    let e = Env::new();
    e.index(&[r("R-1", "unknown")]);
    is(&e.check(), 1, "red unknown-state R-1\nplan check red findings=1\n");
}

#[test]
fn a_done_record_with_no_ticket_file_is_red() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.done("T-9");
    is(&e.check(), 1, "red done-record-no-ticket T-9\nplan check red findings=1\n");
}

#[test]
fn findings_come_in_the_stated_order() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built"), r("R-1", "unknown"), r("R-2", "built")]);
    e.ticket("B-1", "S", &names(r#""X-1""#));
    e.ticket("A-1", "S", &names(r#""Y-1""#));
    e.done("B-1");
    e.done("Z-9");
    is(
        &e.check(),
        1,
        "red duplicate-id R-1\nred unknown-state R-1\nred built-no-done-ticket R-2\nred ticket-names-unknown-row A-1 Y-1\nred ticket-names-unknown-row B-1 X-1\nred done-ticket-no-row B-1\nred done-record-no-ticket Z-9\nplan check red findings=7\n",
    );
}

#[test]
fn the_green_case_prints_the_ok_line() {
    let e = Env::new();
    e.index(&[r("R-1", "built"), r("R-2", "not-built")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    e.ticket("T-2", "M", &names(r#""R-2""#));
    e.done("T-1");
    is(&e.check(), 0, "plan check ok rows=2 tickets=2 done=1\n");
}

#[test]
fn a_not_done_ticket_with_no_link_is_not_red() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", "");
    is(&e.check(), 0, "plan check ok rows=1 tickets=1 done=0\n");
}

// ---- inputs: most refusal cases run for check and for status

fn both(e: &Env, err: &str) {
    for sub in ["check", "status"] {
        refused(&e.run(sub, &[]), err);
    }
}

#[test]
fn a_missing_index_exits_2() {
    let e = Env::new();
    both(&e, "harness: error: plan index: the file is missing or cannot be read");
}

#[test]
fn an_index_that_does_not_parse_exits_2() {
    let e = Env::new();
    e.index(&["nope".to_string()]);
    both(&e, "harness: error: plan index: line 1: does not parse as one JSON object");
}

#[test]
fn an_index_row_with_a_missing_field_exits_2() {
    let e = Env::new();
    e.index(&[r(r"R-1", "built").replace(r#","code":[]"#, "")]);
    both(&e, "harness: error: plan index: line 1: field `code`: missing");
}

#[test]
fn an_index_state_outside_the_list_exits_2() {
    let e = Env::new();
    e.index(&[r("R-1", "finished")]);
    both(&e, "harness: error: plan index: line 1: field `state`: wrong type or value");
}

#[test]
fn a_ticket_that_does_not_parse_exits_2() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::write(e.root.join("tickets/T-1.json"), "{").unwrap();
    both(&e, "harness: error: ticket T-1: does not parse");
}

#[test]
fn a_ticket_whose_id_differs_from_its_file_name_exits_2() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::write(e.root.join("tickets/T-2.json"), ticket_json("T-1", "S", "")).unwrap();
    both(&e, "harness: error: ticket T-2: its id differs from its file name");
}

#[test]
fn a_ticket_or_done_name_outside_the_id_rule_exits_2() {
    for name in ["zq b", "zq\nb", "..zq", "zq..b", ".zq"] {
        let e = Env::new();
        e.index(&[r("R-1", "not-built")]);
        fs::write(e.root.join("tickets").join(format!("{name}.json")), ticket_json("T-1", "S", "")).unwrap();
        for sub in ["check", "status"] {
            let o = e.run(sub, &[]);
            refused(&o, "harness: error: a ticket file name is outside the id rule");
            assert!(!o.out.contains("zq") && !o.err.contains("zq"));
        }
        let e = Env::new();
        e.index(&[r("R-1", "not-built")]);
        fs::write(e.root.join("done").join(format!("{name}.done")), format!("ticket={name}\n")).unwrap();
        for sub in ["check", "status"] {
            let o = e.run(sub, &[]);
            refused(&o, "harness: error: a done record file name is outside the id rule");
            assert!(!o.out.contains("zq") && !o.err.contains("zq"));
        }
    }
}

#[test]
fn a_folder_or_link_named_like_a_ticket_or_done_record_exits_2() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::create_dir(e.root.join("tickets/T-1.json")).unwrap();
    both(&e, "harness: error: ticket T-1: is not a regular file");
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::write(e.root.join("real.txt"), ticket_json("T-1", "S", "")).unwrap();
    std::os::unix::fs::symlink(e.root.join("real.txt"), e.root.join("tickets/T-1.json")).unwrap();
    both(&e, "harness: error: ticket T-1: is not a regular file");
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::create_dir(e.root.join("done/T-1.done")).unwrap();
    both(&e, "harness: error: done record T-1: is not a regular file");
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::write(e.root.join("real.txt"), "ticket=T-1\n").unwrap();
    std::os::unix::fs::symlink(e.root.join("real.txt"), e.root.join("done/T-1.done")).unwrap();
    both(&e, "harness: error: done record T-1: is not a regular file");
}

#[test]
fn a_done_record_with_a_wrong_first_line_exits_2() {
    for body in ["ticket=T-2\n", "", "\nticket=T-1\n", "ticket=T-1 \n", "x ticket=T-1\n"] {
        let e = Env::new();
        e.index(&[r("R-1", "not-built")]);
        fs::write(e.root.join("done/T-1.done"), body).unwrap();
        both(&e, "harness: error: done record T-1: its first line is not ticket=<id>");
    }
}

#[test]
fn a_missing_tickets_or_done_folder_exits_2() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::remove_dir(e.root.join("tickets")).unwrap();
    both(&e, "harness: error: the tickets folder is missing or cannot be read");
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::remove_dir(e.root.join("done")).unwrap();
    both(&e, "harness: error: the done folder is missing or cannot be read");
}

#[test]
fn a_part_file_in_the_tickets_folder_is_skipped() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    fs::write(e.root.join("tickets/T-1.json.part"), "garbage").unwrap();
    fs::create_dir(e.root.join("tickets/sub")).unwrap();
    is(&e.check(), 0, "plan check ok rows=1 tickets=0 done=0\n");
}

#[test]
fn input_refusals_print_no_folder_and_no_row_text() {
    let e = Env::named("zzmarkzz");
    let bad_row = r("R-1", "not-built").replace("\"t\"", "\"zzmarkzz\"").replace(r#""code":[]"#, r#""code":1"#);
    e.index(&[bad_row]);
    for sub in ["check", "status"] {
        let o = e.run(sub, &[]);
        assert_eq!((o.out.as_str(), o.code), ("", 2));
        assert!(!o.err.contains("zzmarkzz") && !o.err.contains(e.root.to_str().unwrap()), "{}", o.err);
    }
    e.index(&[r("R-1", "not-built").replace("\"t\"", "\"zzmarkzz\"")]);
    fs::write(e.root.join("tickets/T-1.json"), ticket_json("T-1", "S", "").replace("\"o\"", "\"zzmarkzz\"") + "x").unwrap();
    for sub in ["check", "status"] {
        let o = e.run(sub, &[]);
        assert_eq!((o.out.as_str(), o.code), ("", 2));
        assert!(!o.err.contains("zzmarkzz") && !o.err.contains(e.root.to_str().unwrap()), "{}", o.err);
    }
}

// ---- ticket mode

#[test]
fn ticket_mode_ignores_findings_about_other_rows() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built"), r("R-2", "built")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    e.ticket("T-2", "S", "");
    e.done("T-2");
    assert_eq!(e.check().code, 1);
    is(&e.run("check", &["--ticket", "T-1"]), 0, "plan check ok ticket=T-1\n");
}

#[test]
fn ticket_mode_is_red_for_a_ticket_with_no_link() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", "");
    is(&e.run("check", &["--ticket", "T-1"]), 1, "red no-plan-link T-1\nplan check red findings=1\n");
}

#[test]
fn ticket_mode_is_red_for_an_unknown_row_id() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", &names(r#""X-9""#));
    is(&e.run("check", &["--ticket", "T-1"]), 1, "red no-plan-link T-1\nred ticket-names-unknown-row T-1 X-9\nplan check red findings=2\n");
}

#[test]
fn ticket_mode_is_red_when_its_row_is_unknown() {
    let e = Env::new();
    e.index(&[r("R-1", "unknown")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    is(&e.run("check", &["--ticket", "T-1"]), 1, "red unknown-state R-1\nplan check red findings=1\n");
}

#[test]
fn ticket_mode_is_red_when_its_deviation_row_is_incomplete() {
    let e = Env::new();
    e.index(&[row("R-1", "deviation", &[], "null")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    is(
        &e.run("check", &["--ticket", "T-1"]),
        1,
        "red deviation-no-reason R-1\nred deviation-no-decided-by R-1\nred deviation-no-date R-1\nplan check red findings=3\n",
    );
}

#[test]
fn ticket_mode_is_red_for_a_repeated_row_id() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built"), r("R-1", "not-built")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    is(&e.run("check", &["--ticket", "T-1"]), 1, "red duplicate-id R-1\nplan check red findings=1\n");
}

#[test]
fn ticket_mode_needs_no_done_record() {
    let e = Env::new();
    e.index(&[r("R-1", "built")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    is(&e.run("check", &["--ticket", "T-1"]), 0, "plan check ok ticket=T-1\n");
}

#[test]
fn ticket_mode_passes_an_off_plan_ticket() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", &off("why"));
    is(&e.run("check", &["--ticket", "T-1"]), 0, "plan check ok ticket=T-1\n");
}

#[test]
fn ticket_mode_with_no_ticket_file_exits_2() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    refused(&e.run("check", &["--ticket", "T-1"]), "harness: error: the ticket file is missing");
}

#[test]
fn ticket_mode_refuses_a_bad_ticket_value() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    for v in ["../x", "a b"] {
        refused(&e.run("check", &["--ticket", v]), USAGE);
    }
}

// ---- status

#[test]
fn status_prints_rows_per_state() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built"), r("R-2", "built"), r("R-3", "built"), r("R-4", "later"), r("R-5", "unknown")]);
    is(
        &e.status(),
        0,
        "state not-built rows=1 ids=R-1\nstate in-build rows=0 ids=-\nstate built rows=2 ids=R-2,R-3\nstate deviation rows=0 ids=-\nstate later rows=1 ids=R-4\nstate unknown rows=1 ids=R-5\nprogress done=0 left=0 percent=none\non-plan done=0 all-done=0 percent=none\n",
    );
}

const NO_ROWS: &str = "state not-built rows=0 ids=-\nstate in-build rows=0 ids=-\nstate built rows=0 ids=-\nstate deviation rows=0 ids=-\nstate later rows=0 ids=-\nstate unknown rows=0 ids=-\n";

fn only_not_built(ids: &str, n: usize) -> String {
    NO_ROWS.replacen("state not-built rows=0 ids=-", &format!("state not-built rows={n} ids={ids}"), 1)
}

#[test]
fn status_lists_off_plan_tickets() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-2", "S", &off("second"));
    e.ticket("T-1", "S", &off("first"));
    e.done("T-1");
    let want = only_not_built("R-1", 1)
        + "off-plan ticket=T-1 done=yes reason=first\noff-plan ticket=T-2 done=no reason=second\nprogress done=0 left=0 percent=none\non-plan done=0 all-done=1 percent=0\n";
    is(&e.status(), 0, &want);
}

#[test]
fn status_lists_leaned_on_later_rows() {
    let e = Env::new();
    e.index(&[row("R-1", "later", &["T-2"], "null"), r("R-2", "later")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    e.ticket("T-2", "S", "");
    let mut want = NO_ROWS.replace("state later rows=0 ids=-", "state later rows=2 ids=R-1,R-2");
    want += "leaned-on row=R-1 tickets=T-1,T-2\nprogress done=0 left=2 percent=0\non-plan done=0 all-done=0 percent=none\n";
    is(&e.status(), 0, &want);
}

#[test]
fn status_progress_is_weighted() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    e.ticket("T-2", "M", &names(r#""R-1""#));
    e.ticket("T-3", "L", &names(r#""R-1""#));
    e.done("T-1");
    e.done("T-2");
    is(&e.status(), 0, &(only_not_built("R-1", 1) + "progress done=4 left=9 percent=30\non-plan done=4 all-done=4 percent=100\n"));
}

#[test]
fn status_progress_counts_a_ticket_once() {
    let e = Env::new();
    e.index(&[row("R-1", "not-built", &["T-1"], "null"), row("R-2", "not-built", &["T-1"], "null")]);
    e.ticket("T-1", "M", &names(r#""R-1""#));
    e.done("T-1");
    is(&e.status(), 0, &(only_not_built("R-1,R-2", 2) + "progress done=3 left=0 percent=100\non-plan done=3 all-done=3 percent=100\n"));
}

#[test]
fn an_off_plan_ticket_listed_by_a_row_is_linked() {
    let e = Env::new();
    e.index(&[row("R-1", "not-built", &["T-1"], "null")]);
    e.ticket("T-1", "S", &off("why"));
    e.done("T-1");
    is(&e.status(), 0, &(only_not_built("R-1", 1) + "progress done=1 left=0 percent=100\non-plan done=1 all-done=1 percent=100\n"));
}

#[test]
fn status_progress_with_no_linked_ticket_prints_none() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", "");
    is(&e.status(), 0, &(only_not_built("R-1", 1) + "progress done=0 left=0 percent=none\non-plan done=0 all-done=0 percent=none\n"));
}

#[test]
fn status_on_plan_share() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    e.ticket("T-1", "S", &names(r#""R-1""#));
    e.ticket("T-2", "M", &names(r#""R-1""#));
    e.ticket("T-3", "L", &off("why"));
    for t in ["T-1", "T-2", "T-3"] {
        e.done(t);
    }
    let want = only_not_built("R-1", 1) + "off-plan ticket=T-3 done=yes reason=why\nprogress done=4 left=0 percent=100\non-plan done=4 all-done=13 percent=30\n";
    is(&e.status(), 0, &want);
}

#[test]
fn status_ticket_prints_the_linked_rows() {
    let e = Env::new();
    let titled = r("R-2", "built").replace("\"t\"", r#""a\tb""#);
    e.index(&[r("R-1", "not-built"), titled]);
    e.ticket("T-1", "S", &names(r#""R-1","R-2""#));
    e.ticket("T-2", "S", &off("why"));
    e.ticket("T-3", "S", "");
    is(&e.run("status", &["--ticket", "T-1"]), 0, "row=R-1 state=not-built plan_line=3 title=t\nrow=R-2 state=built plan_line=3 title=a b\n");
    is(&e.run("status", &["--ticket", "T-2"]), 0, "off-plan reason=why\n");
    is(&e.run("status", &["--ticket", "T-3"]), 0, "no-plan-link ticket=T-3\n");
    refused(&e.run("status", &["--ticket", "T-9"]), "harness: error: the ticket file is missing");
}

#[test]
fn status_on_a_red_index_exits_0() {
    let e = Env::new();
    e.index(&[r("R-1", "built"), r("R-1", "unknown"), row("R-2", "deviation", &[], "null")]);
    let o = e.status();
    assert_eq!((o.code, o.err.as_str()), (0, ""));
    assert!(o.out.starts_with("state not-built rows=0 ids=-\n"));
    assert!(!o.out.contains("red "));
}

// ---- usage

#[test]
fn usage_errors_exit_2_with_the_plan_usage() {
    let e = Env::new();
    e.index(&[r("R-1", "not-built")]);
    let i = e.root.join("index.jsonl");
    let t = e.root.join("tickets");
    let d = e.root.join("done");
    let (i, t, d) = (i.to_str().unwrap(), t.to_str().unwrap(), d.to_str().unwrap());
    let cases: Vec<Vec<&str>> = vec![
        vec![],
        vec!["frob"],
        vec!["check"],
        vec!["check", "--index", i, "--tickets", t],
        vec!["check", "--index", i, "--tickets", t, "--done", d, "--bogus", "x"],
        vec!["check", "--index", i, "--tickets", t, "--done"],
        vec!["check", "--index", i, "--index", i, "--tickets", t, "--done", d],
        vec!["check", "--index", i, "--tickets", t, "--done", d, "--ticket", "T-1", "--ticket", "T-1"],
        vec!["check", "--index", "index.jsonl", "--tickets", t, "--done", d],
        vec!["check", "--index", i, "--tickets", "/a/../b", "--done", d],
        vec!["check", "--index", i, "--tickets", t, "--done", d, "--ticket", "../x"],
        vec!["status", "--index", i, "--tickets", t, "--done", d, "--ticket", "a b"],
        vec!["check", "--index=x", "--tickets", t, "--done", d],
        vec!["check", i, "--index", i, "--tickets", t, "--done", d],
        vec!["status", "extra"],
    ];
    for c in cases {
        let o = Command::new(BIN).arg("plan").args(&c).output().unwrap();
        assert_eq!(String::from_utf8(o.stdout).unwrap(), "", "{c:?}");
        assert_eq!(String::from_utf8(o.stderr).unwrap(), format!("{USAGE}\n"), "{c:?}");
        assert_eq!(o.status.code(), Some(2), "{c:?}");
    }
}

#[test]
fn the_top_level_usage_line_ends_with_the_plan_alternative() {
    let o = Command::new(BIN).output().unwrap();
    let err = String::from_utf8(o.stderr).unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(err.ends_with(" | plan check|status ...\n"), "{err}");
}
