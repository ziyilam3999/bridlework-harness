//! `harness lesson replay` with `--config`/`--controls`/`--rows` (the answer key): real binary, synthetic
//! fixtures only (no real ids, names, paths or text). The config, the inbox and the privacy list come from
//! `friction_common`; the lessons, tickets and scopes are built beside them in the same scratch folder.
mod friction_common;
use friction_common::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

fn fm(body: &str) -> String {
    format!("---\n{}\n---\n", body)
}

fn ticket_json(repo: &str, title: &str, criteria: &[&str]) -> String {
    let crit: Vec<String> = criteria.iter().map(|c| format!("\"{}\"", c)).collect();
    format!(
        "{{\"id\":\"T-1\",\"repo_id\":\"{}\",\"title\":\"{}\",\"group\":\"grp\",\"size\":\"S\",\"objective\":\"obj\",\"acceptance_criteria\":[{}],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}",
        repo,
        title,
        crit.join(",")
    )
}

/// A ticket that hits the page token `example`.
fn hello() -> String {
    ticket_json("example", "Say hello", &["checks/hello.sh passes"])
}

/// A ticket that hits no synthetic page.
fn other() -> String {
    ticket_json("other-repo", "Nothing here", &["zzz"])
}

/// A friction row on the given ticket (the shared builder pins `t-1`, so the ticket field is swapped).
fn row_on(id: &str, date: &str, breaks: &str, ticket: &str) -> String {
    row(id, date, breaks, "lesson", &[]).replace("\"ticket\":\"t-1\"", &format!("\"ticket\":\"{ticket}\""))
}

/// A whole replay fixture in one scratch folder.
struct Fix {
    t: Tmp,
    lessons: PathBuf,
    tickets: PathBuf,
    scopes: PathBuf,
    controls: PathBuf,
    cfg: PathBuf,
}

fn fix(pages: &[(&str, String)], tickets: &[(&str, String)], scopes: &[(&str, String)], inbox: &[String], controls: &str) -> Fix {
    let t = Tmp::new();
    let lessons = t.0.join("lessons");
    fs::create_dir_all(&lessons).unwrap();
    for (n, b) in pages {
        fs::write(lessons.join(n), b).unwrap();
    }
    let tdir = t.0.join("tickets");
    fs::create_dir_all(&tdir).unwrap();
    for (n, b) in tickets {
        fs::write(tdir.join(n), b).unwrap();
    }
    let sdir = t.0.join("scopes");
    fs::create_dir_all(&sdir).unwrap();
    for (n, b) in scopes {
        fs::write(sdir.join(n), b).unwrap();
    }
    t.write_inbox(inbox);
    let controls = t.file("controls.txt", controls);
    let cfg = t.config();
    Fix { t, lessons, tickets: tdir, scopes: sdir, controls, cfg }
}

/// The standard fixture: g-1 offered on aa-1, g-1b replayed but not offered, g-1c on a ticket with no ticket
/// file, g-2 with no controls line.
fn standard() -> Fix {
    fix(
        &[("ISSUE-101-a.md", fm("match: [example]")), ("ISSUE-102-b.md", fm("match: [nosuchword]"))],
        &[("aa-1.json", hello()), ("bb-2.json", other())],
        &[("aa-1.paths", "src/\n".to_string()), ("bb-2.paths", "docs/\n".to_string())],
        &[
            row_on("g-1", "2026-11-02", "", "aa-1"),
            row_on("g-1b", "2026-11-09", "g-1", "bb-2"),
            row_on("g-1c", "2026-11-10", "g-1", "cc-3"),
            row_on("g-2", "2026-11-02", "", "aa-1"),
        ],
        "g-1 | control: a guard | lesson: ISSUE-101\n",
    )
}

fn replay_args(f: &Fix, extra: &[&str]) -> Output {
    let mut a: Vec<&str> = vec!["lesson", "replay", p(&f.lessons), "--tickets", p(&f.tickets), "--scopes", p(&f.scopes)];
    a.extend_from_slice(extra);
    run(&a)
}

fn plain(f: &Fix) -> Output {
    replay_args(f, &[])
}

fn with_key(f: &Fix) -> Output {
    replay_args(f, &["--config", p(&f.cfg), "--controls", p(&f.controls)])
}

fn with_rows(f: &Fix, rows: &Path) -> Output {
    replay_args(f, &["--config", p(&f.cfg), "--controls", p(&f.controls), "--rows", p(rows)])
}

/// The `key ok` line of an output.
fn key_ok(o: &Output) -> String {
    out(o).lines().find(|l| l.starts_with("key ok ")).unwrap().to_string()
}

// ---------------------------------------------------------------- the counts

/// The fixture prints the exact key lines, and rows + no_ticket + no_line is the number of inbox rows.
#[test]
fn the_key_prints_the_exact_lines() {
    let f = standard();
    let o = with_key(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "replay: ticket=aa-1 offered=1 pages=ISSUE-101\n\
         replay: ticket=bb-2 offered=0 pages=none\n\
         page: ISSUE-101 hits=1\n\
         page: ISSUE-102 hits=0\n\
         key: ISSUE-101 rows=2 offered=1\n\
         key ok rows=2 offered=1 no_ticket=1 no_line=1\n\
         replay ok tickets=2 skipped_no_scope=0 pages=2 offered_total=1 offered_max=1 zero=1\n"
    );
    let n = |k: &str| key_ok(&o).split(&format!(" {k}=")).nth(1).unwrap().split([' ', '\n']).next().unwrap().parse::<usize>().unwrap();
    assert_eq!(n("rows") + n("no_ticket") + n("no_line"), 4, "the four inbox rows");
}

/// A replay without the three flags prints no key line.
#[test]
fn a_replay_without_the_three_flags_prints_no_key_line() {
    let f = standard();
    let o = plain(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!out(&o).contains("key"), "{}", out(&o));
}

/// A row on a ticket with no ticket file and a row on a ticket skipped for no scope file both count in
/// no_ticket (their group has a line).
#[test]
fn rows_on_tickets_not_replayed_count_in_no_ticket() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", hello()), ("dd-4.json", other())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[row_on("g-1", "2026-11-02", "", "dd-4"), row_on("g-1b", "2026-11-09", "g-1", "ee-5")],
        "g-1 | control: a guard | lesson: ISSUE-101\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(key_ok(&o), "key ok rows=0 offered=0 no_ticket=2 no_line=0");
    assert!(out(&o).contains("skipped_no_scope=1"), "{}", out(&o));
}

/// A line that names a retired page is read: its rows count in rows= and are not offered.
#[test]
fn a_retired_page_line_counts_its_rows_not_offered() {
    let f = fix(
        &[("ISSUE-103-r.md", fm("status: retired\nmatch: [example]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[row_on("g-1", "2026-11-02", "", "aa-1")],
        "g-1 | control: a guard | lesson: ISSUE-103\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "replay: ticket=aa-1 offered=0 pages=none\n\
         page: ISSUE-103 hits=0\n\
         key: ISSUE-103 rows=1 offered=0\n\
         key ok rows=1 offered=0 no_ticket=0 no_line=0\n\
         replay ok tickets=1 skipped_no_scope=0 pages=1 offered_total=0 offered_max=0 zero=1 retired=1\n"
    );
}

/// A breaks chain three deep whose root has the controls line is counted under the root's page for all
/// three rows.
#[test]
fn a_breaks_chain_three_deep_counts_under_the_root() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[
            row_on("c-1", "2026-11-02", "", "aa-1"),
            row_on("c-2", "2026-11-03", "c-1", "aa-1"),
            row_on("c-3", "2026-11-04", "c-2", "aa-1"),
        ],
        "c-1 | control: a guard | lesson: ISSUE-101\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(key_ok(&o), "key ok rows=3 offered=3 no_ticket=0 no_line=0");
    assert!(out(&o).contains("key: ISSUE-101 rows=3 offered=3\n"), "{}", out(&o));
}

/// The key lines come in byte order of the page id, not in the order the inbox meets the pages.
#[test]
fn key_lines_are_in_byte_order_of_the_page_id() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]")), ("ISSUE-102-b.md", fm("match: [nosuchword]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[row_on("g-2", "2026-11-02", "", "aa-1"), row_on("g-1", "2026-11-03", "", "aa-1")],
        "g-2 | control: a guard | lesson: ISSUE-102\ng-1 | control: a guard | lesson: ISSUE-101\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "replay: ticket=aa-1 offered=1 pages=ISSUE-101\n\
         page: ISSUE-101 hits=1\n\
         page: ISSUE-102 hits=0\n\
         key: ISSUE-101 rows=1 offered=1\n\
         key: ISSUE-102 rows=1 offered=0\n\
         key ok rows=2 offered=1 no_ticket=0 no_line=0\n\
         replay ok tickets=1 skipped_no_scope=0 pages=2 offered_total=1 offered_max=1 zero=0\n"
    );
}

/// A flag of the three with no value is a setup error: exit 2, stdout empty.
#[test]
fn a_key_flag_with_no_value_exits_2() {
    let f = standard();
    for flag in ["--config", "--controls", "--rows"] {
        let o = replay_args(&f, &[flag]);
        assert_eq!(code(&o), 2, "{flag}: {}", err(&o));
        assert_eq!(out(&o), "", "{flag}");
        assert!(err(&o).contains("needs a value"), "{flag}: {}", err(&o));
    }
}

/// A row whose own id (not its group id) is on a controls line is not counted through that line.
#[test]
fn a_line_naming_a_row_id_is_not_counted_through() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[row_on("g-1", "2026-11-02", "", "aa-1"), row_on("g-1b", "2026-11-09", "g-1", "aa-1")],
        "g-1b | control: a guard | lesson: ISSUE-101\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(key_ok(&o), "key ok rows=0 offered=0 no_ticket=0 no_line=2");
    assert!(!out(&o).contains("\nkey: "), "{}", out(&o));
}

/// No row text and no ticket text is printed (a planted unique word in each).
#[test]
fn no_row_text_and_no_ticket_text_is_printed() {
    const W: &str = "zqkeywordzz";
    let row = row("g-1", "2026-11-02", "", W, &[]).replace("\"ticket\":\"t-1\"", "\"ticket\":\"aa-1\"");
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", ticket_json("example", &format!("{W} title"), &["checks/x"]))],
        &[("aa-1.paths", "src/\n".to_string())],
        &[row],
        "g-1 | control: a guard | lesson: ISSUE-101\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!out(&o).contains(W) && !err(&o).contains(W), "{}{}", out(&o), err(&o));
}

// ---------------------------------------------------------------- --rows

/// With a 4-row inbox and a file naming 2 rows, rows= counts those 2 only; without --rows it counts 4.
#[test]
fn a_rows_file_limits_the_counted_rows() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[
            row_on("g-1", "2026-11-02", "", "aa-1"),
            row_on("g-1b", "2026-11-03", "g-1", "aa-1"),
            row_on("g-2", "2026-11-04", "", "aa-1"),
            row_on("g-2b", "2026-11-05", "g-2", "aa-1"),
        ],
        "g-1 | control: a guard | lesson: ISSUE-101\ng-2 | control: a guard | lesson: ISSUE-101\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(key_ok(&o), "key ok rows=4 offered=4 no_ticket=0 no_line=0");
    let rf = f.t.file("rows.txt", "g-1b\ng-2b\n");
    let o = with_rows(&f, &rf);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(key_ok(&o), "key ok rows=2 offered=2 no_ticket=0 no_line=0 listed=2");
    assert!(out(&o).contains("key: ISSUE-101 rows=2 offered=2\n"), "{}", out(&o));
}

/// A listed row whose root row is not listed is counted under the root's controls line (the groups are
/// built from the whole inbox).
#[test]
fn rows_file_keeps_the_group() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[row_on("g-1", "2026-11-02", "", "aa-1"), row_on("g-1b", "2026-11-03", "g-1", "aa-1")],
        "g-1 | control: a guard | lesson: ISSUE-101\n",
    );
    let rf = f.t.file("rows.txt", "g-1b\n");
    let o = with_rows(&f, &rf);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(key_ok(&o), "key ok rows=1 offered=1 no_ticket=0 no_line=0 listed=1");
}

/// A 2-id file with and without the last line feed gives the same output.
#[test]
fn the_last_line_feed_of_a_rows_file_is_optional() {
    let f = standard();
    let with = f.t.file("rows-a.txt", "g-1\ng-1b\n");
    let without = f.t.file("rows-b.txt", "g-1\ng-1b");
    let a = with_rows(&f, &with);
    let b = with_rows(&f, &without);
    assert_eq!(code(&a), 0, "{}", err(&a));
    assert_eq!(code(&b), 0, "{}", err(&b));
    assert_eq!(out(&a), out(&b));
    assert_eq!(key_ok(&a), "key ok rows=2 offered=1 no_ticket=0 no_line=0 listed=2");
}

/// The worked example: 2 pages, 2 tickets, 2 friction rows, a rows file naming one.
#[test]
fn worked_example_two_pages_two_tickets_two_rows_and_a_rows_file() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]")), ("ISSUE-102-b.md", fm("match: [nosuchword]"))],
        &[("aa-1.json", hello()), ("bb-2.json", other())],
        &[("aa-1.paths", "src/\n".to_string()), ("bb-2.paths", "docs/\n".to_string())],
        &[row_on("g-1", "2026-11-02", "", "aa-1"), row_on("g-2", "2026-11-03", "", "bb-2")],
        "g-1 | control: a guard | lesson: ISSUE-101\ng-2 | control: a guard | lesson: ISSUE-102\n",
    );
    let rf = f.t.file("rows.txt", "g-2\n");
    let o = with_rows(&f, &rf);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "replay: ticket=aa-1 offered=1 pages=ISSUE-101\n\
         replay: ticket=bb-2 offered=0 pages=none\n\
         page: ISSUE-101 hits=1\n\
         page: ISSUE-102 hits=0\n\
         key: ISSUE-102 rows=1 offered=0\n\
         key ok rows=1 offered=0 no_ticket=0 no_line=0 listed=1\n\
         replay ok tickets=2 skipped_no_scope=0 pages=2 offered_total=1 offered_max=1 zero=1\n"
    );
}

// ---------------------------------------------------------------- usage (exit 2, the usage text)

/// Each answer-key usage error: exit 2, empty stdout, the usage text.
#[test]
fn key_flag_usage_errors_exit_2() {
    let f = standard();
    let rf = f.t.file("rows.txt", "g-1\n");
    let rel = "controls.txt".to_string();
    let dd = format!("{}/x/../controls.txt", f.t.0.to_str().unwrap());
    let cfg = p(&f.cfg);
    let ctl = p(&f.controls);
    let rows = p(&rf);
    let cases: Vec<Vec<&str>> = vec![
        vec!["--config", &cfg],
        vec!["--controls", &ctl],
        vec!["--rows", &rows],
        vec!["--config", &cfg, "--controls", &ctl, "--rows", &rows, "--rows", &rows],
        vec!["--config", &cfg, "--config", &cfg, "--controls", &ctl],
        vec!["--config", &cfg, "--controls", &ctl, "--controls", &ctl],
        vec!["--config", &cfg, "--controls", &rel],
        vec!["--config", &rel, "--controls", &ctl],
        vec!["--config", &cfg, "--controls", &ctl, "--rows", &rel],
        vec!["--config", &cfg, "--controls", &dd],
        vec!["--config", &dd, "--controls", &ctl],
        vec!["--config", &cfg, "--controls", &ctl, "--rows", &dd],
    ];
    for args in &cases {
        let o = replay_args(&f, args);
        assert_eq!(code(&o), 2, "{:?}", args);
        assert_eq!(out(&o), "", "{:?}", args);
        assert!(err(&o).starts_with("usage: harness lesson"), "{:?}: {}", args, err(&o));
    }
}

// ---------------------------------------------------------------- setup errors (exit 2, as friction rank)

#[test]
fn a_missing_config_exits_2_as_rank() {
    let f = standard();
    let o = replay_args(&f, &["--config", p(&f.t.0.join("nope.yaml")), "--controls", p(&f.controls)]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).starts_with("harness: error: "), "{}", err(&o));
}

/// No privacy list configured (no list variable reaches the child).
#[test]
fn no_privacy_list_configured_exits_2() {
    let f = standard();
    let mut c = base();
    let args: Vec<&str> = vec![
        "lesson",
        "replay",
        p(&f.lessons),
        "--tickets",
        p(&f.tickets),
        "--scopes",
        p(&f.scopes),
        "--config",
        p(&f.cfg),
        "--controls",
        p(&f.controls),
    ];
    c.args(&args).stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_eq!(o.status.code(), Some(2), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(o.stdout.len(), 0);
    assert!(String::from_utf8_lossy(&o.stderr).contains("no privacy list configured"));
}

/// A missing inbox exits 2 and no inbox file is created by the read.
#[test]
fn a_missing_inbox_exits_2_and_creates_no_inbox() {
    let f = standard();
    fs::remove_file(f.t.inbox()).unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(!f.t.inbox().exists(), "an inbox file was created");
}

#[test]
fn an_inbox_that_is_a_folder_exits_2() {
    let f = standard();
    fs::remove_file(f.t.inbox()).unwrap();
    fs::create_dir_all(f.t.inbox()).unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("the inbox path is a folder"), "{}", err(&o));
}

#[test]
fn an_inbox_over_its_cap_exits_2() {
    let f = standard();
    fs::write(f.t.inbox(), vec![b'#'; 4 * 1024 * 1024 + 1]).unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("over"), "{}", err(&o));
}

#[test]
fn an_inbox_that_is_not_utf8_exits_2() {
    let f = standard();
    fs::write(f.t.inbox(), [0xffu8, 0xfe, b'\n']).unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("not utf-8"), "{}", err(&o));
}

// ---------------------------------------------------------------- inbox refusals (exit 1, as friction rank)

#[test]
fn a_bad_inbox_row_refuses_as_rank() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &["not a row".to_string()],
        "g-1 | control: a guard | lesson: ISSUE-101\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).starts_with("harness: inbox line 1: "), "{}", err(&o));
}

#[test]
fn a_torn_last_line_refuses_as_rank() {
    let f = standard();
    // the same inbox without the final line feed
    fs::write(f.t.inbox(), fs::read_to_string(f.t.inbox()).unwrap().trim_end_matches('\n')).unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("torn tail"), "{}", err(&o));
}

#[test]
fn a_privacy_hit_in_a_stored_row_refuses() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[row("g-1", "2026-11-02", "", DENY, &[])],
        "g-1 | control: a guard | lesson: ISSUE-101\n",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("privacy hit"), "{}", err(&o));
}

#[test]
fn a_breaks_cycle_refuses_as_rank() {
    let f = fix(
        &[("ISSUE-101-a.md", fm("match: [example]"))],
        &[("aa-1.json", hello())],
        &[("aa-1.paths", "src/\n".to_string())],
        &[row_on("g-1", "2026-11-02", "g-2", "aa-1"), row_on("g-2", "2026-11-03", "g-1", "aa-1")],
        "",
    );
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "rank: breaks cycle through g-1\n");
}

// ---------------------------------------------------------------- controls refusals

#[test]
fn a_controls_file_missing_a_folder_or_over_the_cap_exits_2() {
    let f = standard();
    // missing
    let o = replay_args(&f, &["--config", p(&f.cfg), "--controls", p(&f.t.0.join("nope.txt"))]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    // a folder
    let d = f.t.0.join("cdir");
    fs::create_dir_all(&d).unwrap();
    let o = replay_args(&f, &["--config", p(&f.cfg), "--controls", p(&d)]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    // over the cap
    let big = f.t.file("big.txt", vec![b'#'; 1024 * 1024 + 1]);
    let o = replay_args(&f, &["--config", p(&f.cfg), "--controls", p(&big)]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
}

#[test]
fn a_malformed_controls_line_refuses() {
    let f = standard();
    fs::write(&f.controls, "not a line\n").unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: lesson replay refused: controls line 1 is malformed; the shape is <group id> | control: <pointer> | lesson: ISSUE-NNN or <group id> | not machine-checkable: <reason> | lesson: ISSUE-NNN\n");
}

#[test]
fn a_controls_line_that_is_not_utf8_refuses() {
    let f = standard();
    fs::write(&f.controls, [0xffu8, 0xfe, b'\n']).unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("controls line 1 is not utf-8"), "{}", err(&o));
}

#[test]
fn a_controls_line_with_a_control_character_refuses() {
    let f = standard();
    fs::write(&f.controls, "g-1 | control: x\u{1b}y | lesson: ISSUE-101\n").unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("has a control character"), "{}", err(&o));
}

#[test]
fn a_controls_line_with_no_lesson_page_refuses() {
    let f = standard();
    fs::write(&f.controls, "g-1 | control: a guard | lesson: ISSUE-777\n").unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("no regular file"), "{}", err(&o));
}

/// Two lines that name one group id refuse the file with the later line number.
#[test]
fn two_controls_lines_naming_one_group_refuse() {
    let f = standard();
    fs::write(&f.controls, "g-1 | control: a guard | lesson: ISSUE-101\n# a comment\ng-1 | control: another | lesson: ISSUE-101\n").unwrap();
    let o = with_key(&f);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: lesson replay refused: controls line 3 names the group of an earlier line\n");
}

// ---------------------------------------------------------------- rows-file refusals (exit 2)

#[test]
fn a_rows_file_missing_a_folder_a_link_over_the_cap_or_not_utf8_exits_2() {
    let f = standard();
    // missing
    let o = with_rows(&f, &f.t.0.join("nope.txt"));
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    // a folder
    let d = f.t.0.join("rdir");
    fs::create_dir_all(&d).unwrap();
    let o = with_rows(&f, &d);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    // a symbolic link (the config, the inbox and the controls file follow one; a rows file does not)
    let real = f.t.file("real.txt", "g-1\n");
    let link = f.t.0.join("rows-link.txt");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let o = with_rows(&f, &link);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    // over the cap
    let big = f.t.file("rows-big.txt", vec![b'x'; 1024 * 1024 + 1]);
    let o = with_rows(&f, &big);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
    // not UTF-8
    let bin = f.t.file("rows-bin.txt", [0xffu8, b'\n']);
    let o = with_rows(&f, &bin);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(out(&o), "");
}

// ---------------------------------------------------------------- rows-line refusals (exit 1, no id)

#[test]
fn a_rows_line_that_is_not_an_inbox_id_refuses() {
    let f = standard();
    for (name, body) in [("unknown", "zz-9\n".to_string()), ("cr", "g-1\r\n".to_string()), ("blank-around", " g-1 \n".to_string())] {
        let rf = f.t.file(&format!("rows-{name}.txt"), body);
        let o = with_rows(&f, &rf);
        assert_eq!(code(&o), 1, "{}: {}", name, err(&o));
        assert_eq!(out(&o), "", "{}: {}", name, out(&o));
        assert_eq!(err(&o), "harness: lesson replay refused: rows line 1 is not the id of an inbox row\n", "{}: {}", name, err(&o));
    }
}

#[test]
fn a_rows_id_given_twice_refuses() {
    let f = standard();
    let rf = f.t.file("rows.txt", "g-1\ng-1\n");
    let o = with_rows(&f, &rf);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: lesson replay refused: rows line 2 names a row twice\n");
}

#[test]
fn a_blank_rows_line_refuses() {
    let f = standard();
    let rf = f.t.file("rows.txt", "g-1\n\ng-1b\n");
    let o = with_rows(&f, &rf);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: lesson replay refused: rows line 2 is blank\n");
}

#[test]
fn an_empty_rows_file_refuses() {
    let f = standard();
    let rf = f.t.file("rows.txt", "");
    let o = with_rows(&f, &rf);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: lesson replay refused: rows line 1 the file holds no row id\n");
}

// ---------------------------------------------------------------- read only, same answer twice

/// One line per path under `p`: its type, and for a file its size and bytes, for a link its target.
fn listing(p: &Path, out: &mut Vec<String>) {
    let md = fs::symlink_metadata(p).unwrap();
    if md.file_type().is_symlink() {
        out.push(format!("{} link -> {:?}", p.display(), fs::read_link(p).unwrap()));
    } else if md.is_dir() {
        out.push(format!("{} dir", p.display()));
        let mut kids: Vec<PathBuf> = fs::read_dir(p).unwrap().map(|e| e.unwrap().path()).collect();
        kids.sort();
        for k in kids {
            listing(&k, out);
        }
    } else {
        out.push(format!("{} file {} {:?}", p.display(), md.len(), fs::read(p).unwrap()));
    }
}

fn snap(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    listing(root, &mut v);
    v
}

/// Run the replay in the scratch `cwd` folder: the whole scratch tree stays byte-identical and nothing is
/// created in `cwd`.
fn run_read_only(f: &Fix, extra: &[&str], want: i32) -> Output {
    let cwd = f.t.0.join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let before = snap(&f.t.0);
    let mut a: Vec<&str> = vec!["lesson", "replay", p(&f.lessons), "--tickets", p(&f.tickets), "--scopes", p(&f.scopes)];
    a.extend_from_slice(extra);
    let mut c = base();
    let o = c.args(&a).env("HARNESS_PRIVACY_LIST_SECRET", DENY).current_dir(&cwd).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(want), "{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert_eq!(before, snap(&f.t.0), "the scratch tree changed");
    assert_eq!(fs::read_dir(&cwd).unwrap().count(), 0, "the current folder gained a file");
    o
}

/// A passing key run and each refusal leave every input byte-identical, and the same input twice gives the
/// same stdout.
#[test]
fn the_key_writes_nothing_and_prints_the_same_twice() {
    // a passing run with --rows
    let f = standard();
    let rf = f.t.file("rows.txt", "g-1\n");
    let extra: Vec<&str> = vec!["--config", p(&f.cfg), "--controls", p(&f.controls), "--rows", p(&rf)];
    let first = run_read_only(&f, &extra, 0);
    let second = run_read_only(&f, &extra, 0);
    assert_eq!(first.stdout, second.stdout);

    // a rows-line refusal (1)
    let bad = f.t.file("rows-bad.txt", "zz-9\n");
    run_read_only(&f, &["--config", p(&f.cfg), "--controls", p(&f.controls), "--rows", p(&bad)], 1);
    // a controls file that is missing (2)
    run_read_only(&f, &["--config", p(&f.cfg), "--controls", p(&f.t.0.join("nope.txt"))], 2);
    // a config refusal (2)
    run_read_only(&f, &["--config", p(&f.t.0.join("nope.yaml")), "--controls", p(&f.controls)], 2);
    // a usage error (2): --rows without the other two
    run_read_only(&f, &["--rows", p(&rf)], 2);
    // an inbox refusal (1): a torn last line
    let inbox = fs::read_to_string(f.t.inbox()).unwrap();
    fs::write(f.t.inbox(), inbox.trim_end_matches('\n')).unwrap();
    run_read_only(&f, &extra, 1);
    fs::write(f.t.inbox(), inbox).unwrap();
    // a controls refusal (1): a malformed line
    let good = fs::read_to_string(&f.controls).unwrap();
    fs::write(&f.controls, "not a line\n").unwrap();
    run_read_only(&f, &extra, 1);
    fs::write(&f.controls, good).unwrap();
    // a rows-file refusal (2): a folder
    let d = f.t.0.join("rdir");
    fs::create_dir_all(&d).unwrap();
    run_read_only(&f, &["--config", p(&f.cfg), "--controls", p(&f.controls), "--rows", p(&d)], 2);
}
