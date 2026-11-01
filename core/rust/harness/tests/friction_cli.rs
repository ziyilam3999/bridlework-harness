//! Process-level tests for `harness friction add | lint | close-check` (K-1 first slice, ticket 154). Every check
//! runs the real binary as a child process and reads the real inbox file, so the exit-code contract (0 ok, 1 a
//! refusal — a refused write leaves the inbox byte-identical, 2 a usage or setup error) and "nothing on stdout
//! when erroring" are exercised end to end. Every row, marker and deny word is synthetic, built at run time; the
//! privacy list is injected on the child only, and no test reads an environment variable.
#![cfg(unix)]
use harness::friction;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
const PLACEHOLDER: &str = "/placeholder/friction-inbox.jsonl";
/// A synthetic deny word: the ONLY entry of the injected list. No clean fixture may contain it.
const DENY: &str = "zzdenyzz";
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// The synthetic test-mode marker, assembled at run time so this file never spells it out.
fn marker() -> String {
    ["Synthetic", "-Test", "-Marker"].concat()
}

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("fric-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// A scratch config: the public example with the placeholder inbox path swapped for this scratch inbox.
    fn config(&self) -> PathBuf {
        let text = fs::read_to_string(EXAMPLE).unwrap();
        let text = text.replace(PLACEHOLDER, self.inbox().to_str().unwrap());
        assert!(text.contains(self.inbox().to_str().unwrap()), "the example must carry the placeholder inbox path");
        self.file("config.yaml", text)
    }

    fn inbox(&self) -> PathBuf {
        self.0.join("inbox.jsonl")
    }

    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let f = self.0.join(name);
        fs::write(&f, body).unwrap();
        f
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The child with no config or list variable inherited: every input arrives as a flag or an explicit `.env`.
fn base() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET");
    c
}

/// The usual child: the synthetic deny list injected as the secret.
fn run(args: &[String]) -> Output {
    let mut c = base();
    c.args(args).env("HARNESS_PRIVACY_LIST_SECRET", DENY).stdin(Stdio::null());
    c.output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn s(v: &str) -> String {
    v.to_string()
}

/// `friction add` with every field spelled out. An empty `id`/`date`/`ticket`/`breaks` leaves that flag out
/// entirely; the tag and the text are always passed (an empty value is a refusal, not an omission).
fn add(t: &Tmp, id: &str, date: &str, ticket: &str, tag: &str, breaks: &str, text: &str) -> Output {
    let mut a: Vec<String> = vec![s("friction"), s("add"), s("--config"), t.config().to_str().unwrap().to_string()];
    for (flag, v) in [("--id", id), ("--date", date), ("--ticket", ticket), ("--breaks", breaks)] {
        if !v.is_empty() {
            a.push(s(flag));
            a.push(s(v));
        }
    }
    a.push(s("--where-it-lives"));
    a.push(s(tag));
    a.push(s("--text"));
    a.push(s(text));
    run(&a)
}

/// `friction add` with the tag flag omitted entirely (K-1.1 "no tag").
fn add_no_tag(t: &Tmp, id: &str, ticket: &str, text: &str) -> Output {
    let a: Vec<String> = vec![
        s("friction"),
        s("add"),
        s("--config"),
        t.config().to_str().unwrap().to_string(),
        s("--id"),
        s(id),
        s("--date"),
        s("2026-11-01"),
        s("--ticket"),
        s(ticket),
        s("--text"),
        s(text),
    ];
    run(&a)
}

fn seed(t: &Tmp, id: &str, ticket: &str, tag: &str, breaks: &str) -> Output {
    add(t, id, "2026-11-01", ticket, tag, breaks, "write the row before the code that reads it")
}

fn lint(t: &Tmp) -> Output {
    run(&[s("friction"), s("lint"), s("--config"), t.config().to_str().unwrap().to_string()])
}

fn close_check(t: &Tmp, note: &Path, ticket: &str) -> Output {
    run(&[
        s("friction"),
        s("close-check"),
        note.to_str().unwrap().to_string(),
        s("--ticket"),
        s(ticket),
        s("--config"),
        t.config().to_str().unwrap().to_string(),
    ])
}

/// A refusal: the given exit code, nothing on stdout, and the inbox byte-identical (or still absent).
fn refused_untouched(t: &Tmp, o: &Output, want: i32, before: Option<Vec<u8>>) {
    assert_eq!(code(o), want, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty on a refusal: {}", out(o));
    let after = fs::read(t.inbox()).ok();
    assert_eq!(after, before, "a refused write changed the inbox");
}

// ---------------------------------------------------------------- K-1.1: the tag (AC 1-3)

#[test]
fn k1_1_refuses_row_with_no_tag() {
    let t = Tmp::new();
    // No inbox yet: the refusal must not even create the file.
    let o = add_no_tag(&t, "l-1", "154", "a lesson with no tag");
    refused_untouched(&t, &o, 1, None);
    assert!(err(&o).contains("where-it-lives"), "stderr must name the field: {}", err(&o));
    // With a seeded inbox: byte-identical.
    assert_eq!(code(&seed(&t, "l-0", "154", "memory", "")), 0);
    let before = fs::read(t.inbox()).ok();
    let o = add_no_tag(&t, "l-1", "154", "a lesson with no tag");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("where-it-lives"), "{}", err(&o));
}

#[test]
fn k1_1_refuses_unknown_tag() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-0", "154", "memory", "")), 0);
    let before = fs::read(t.inbox()).ok();
    for bad in ["hook", "Skill", "controls", " "] {
        let o = add(&t, "l-1", "2026-11-01", "154", bad, "", "a lesson with a made-up tag");
        refused_untouched(&t, &o, 1, before.clone());
        assert!(err(&o).contains("where-it-lives"), "{bad}: {}", err(&o));
    }
}

#[test]
fn k1_1_stores_tagged_row_with_all_fields() {
    let t = Tmp::new();
    for (i, tag) in friction::TAGS.iter().enumerate() {
        let id = format!("l-{i}");
        let text = format!("route a {tag} lesson to its home");
        let o = add(&t, &id, "2026-11-01", "154", tag, "", &text);
        assert_eq!(code(&o), 0, "{}: {}", tag, err(&o));
        assert_eq!(out(&o), format!("stored id={id}\n"));
    }
    let body = fs::read_to_string(t.inbox()).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(body.ends_with('\n'), "each row is one whole line ending in a newline");
    // The stored row decodes back with all five fields plus the text, exactly as given.
    let row = friction::parse_row(lines[0]).unwrap();
    assert_eq!((row.id.as_str(), row.date.as_str(), row.ticket.as_str(), row.tag.as_str(), row.breaks.as_str()), ("l-0", "2026-11-01", "154", "skill", ""));
    assert_eq!(row.text, "route a skill lesson to its home");
    // The same checker reads the whole result clean.
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=4\n");
}

// ---------------------------------------------------------------- the cross-row rules (AC 4-5)

#[test]
fn refuses_duplicate_id() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, "l-1", "2026-11-01", "154", "gate", "", "the second coming of the same id");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("duplicate"), "{}", err(&o));
    // The refusal never echoes the lesson text nor the inbox path.
    assert!(!err(&o).contains("the second coming"), "{}", err(&o));
    assert!(!err(&o).contains(t.inbox().to_str().unwrap()), "{}", err(&o));
}

#[test]
fn refuses_breaks_pointing_at_no_lesson() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, "l-2", "2026-11-01", "154", "memory", "l-404", "this broke again");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("breaks"), "{}", err(&o));
    // Sibling: a `breaks` that names an existing lesson id is stored, and the inbox stays lint-clean.
    let o = add(&t, "l-2", "2026-11-01", "154", "memory", "l-1", "this broke again");
    assert_eq!(code(&o), 0, "{}", err(&o));
    let row = friction::parse_row(fs::read_to_string(t.inbox()).unwrap().lines().nth(1).unwrap()).unwrap();
    assert_eq!(row.breaks, "l-1");
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=2\n");
}

// ---------------------------------------------------------------- privacy (AC 6)

#[test]
fn refuses_row_with_privacy_hit() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    let before = fs::read(t.inbox()).ok();
    // A synthetic test-mode marker in the text (this run passes no --testmode, so it is a hit).
    let o = add(&t, "l-2", "2026-11-01", "154", "memory", "", &format!("the lesson mentions {}", marker()));
    refused_untouched(&t, &o, 1, before.clone());
    assert!(err(&o).contains("field text"), "{}", err(&o));
    assert!(!err(&o).contains(&marker()), "stderr must carry no matched bytes: {}", err(&o));
    assert!(!out(&o).contains(&marker()));
    // The same refusal for a deny word from the injected list, in any field.
    let o = add(&t, "l-2", "2026-11-01", "154", "memory", "", &format!("saw {} in the log", DENY));
    refused_untouched(&t, &o, 1, before.clone());
    assert!(!err(&o).contains(DENY), "stderr must carry no matched bytes: {}", err(&o));
    let o = add(&t, "l-2", "2026-11-01", "154", DENY, "", "clean text, poisoned tag");
    refused_untouched(&t, &o, 1, before);
}

// ---------------------------------------------------------------- whole-file lint (AC 7)

#[test]
fn lint_reports_bad_row_line() {
    let t = Tmp::new();
    let good = r#"{"id":"l-1","date":"2026-11-01","ticket":"154","where-it-lives":"memory","breaks":"","text":"a clean row"}"#;
    let bad = r#"{"id":"l-2","date":"2026-11-01","ticket":"154","where-it-lives":"hook","breaks":"","text":"a bad row"}"#;
    let good2 = r#"{"id":"l-3","date":"2026-11-01","ticket":"154","where-it-lives":"gate","breaks":"l-1","text":"another clean row"}"#;
    // A planted bad row in the middle: exit 1 naming ITS line.
    fs::write(t.inbox(), format!("{good}\n{bad}\n{good2}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 2"), "the line number must be named: {}", err(&o));
    assert!(err(&o).contains("where-it-lives"), "{}", err(&o));
    // A clean file of hand-written rows: exit 0.
    fs::write(t.inbox(), format!("{good}\n{good2}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=2\n");
}

// ---------------------------------------------------------------- K-1.5: the close line (AC 8-10)

#[test]
fn k1_5_refuses_close_note_without_lesson_line() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    for body in ["# close\n\ndid the thing, all green.\n", "", "# only a heading\n"] {
        let note = t.file("close.md", body);
        let o = close_check(&t, &note, "154");
        assert_eq!(code(&o), 1, "{body:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert!(err(&o).contains("lesson"), "{}", err(&o));
    }
}

#[test]
fn k1_5_accepts_lesson_none_and_lesson_ids() {
    let t = Tmp::new();
    // `lesson: none` passes when the inbox holds no row for that ticket (an empty inbox here).
    let note = t.file("close.md", "# close\n\nlesson: none\n");
    let o = close_check(&t, &note, "999");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "close-check ok ticket=999 lessons=0\n");
    // `lesson: <ids>` passes when the ids are exactly this ticket's rows, in any order.
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    assert_eq!(code(&seed(&t, "l-2", "154", "gate", "l-1")), 0);
    for value in ["lesson: l-1, l-2", "lesson: l-2,l-1", "lesson: l-1"] {
        let want_ok = value != "lesson: l-1";
        let note = t.file("close.md", format!("# close\n\n{value}\n"));
        let o = close_check(&t, &note, "154");
        assert_eq!(code(&o), if want_ok { 0 } else { 1 }, "{value}: {}", err(&o));
        if want_ok {
            assert_eq!(out(&o), "close-check ok ticket=154 lessons=2\n");
        }
    }
}

#[test]
fn k1_5_refuses_none_when_ticket_has_rows() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    assert_eq!(code(&seed(&t, "l-2", "154", "gate", "")), 0);
    assert_eq!(code(&seed(&t, "l-9", "200", "memory", "")), 0);
    // none while rows exist
    let o = close_check(&t, &t.file("a.md", "lesson: none\n"), "154");
    assert_eq!(code(&o), 1, "{}", err(&o));
    // an unknown id
    let o = close_check(&t, &t.file("b.md", "lesson: l-404\n"), "154");
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("l-404"), "{}", err(&o));
    // an id from another ticket
    let o = close_check(&t, &t.file("c.md", "lesson: l-9\n"), "154");
    assert_eq!(code(&o), 1, "{}", err(&o));
    // a captured row missing from the line
    let o = close_check(&t, &t.file("d.md", "lesson: l-1\n"), "154");
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("l-2"), "{}", err(&o));
    // two lesson lines
    let o = close_check(&t, &t.file("e.md", "lesson: l-1, l-2\nlesson: none\n"), "154");
    assert_eq!(code(&o), 1, "{}", err(&o));
}

// ---------------------------------------------------------------- the rest of the rule set

#[test]
fn refuses_row_with_malformed_date() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    let before = fs::read(t.inbox()).ok();
    for bad in ["2026-02-30", "2026-13-01", "2026-1-1", "26-11-01", "2026-11-1", "today"] {
        let o = add(&t, "l-2", bad, "154", "memory", "", "a lesson with a bad date");
        refused_untouched(&t, &o, 1, before.clone());
        assert!(err(&o).contains("date"), "{bad}: {}", err(&o));
    }
    // Leap-day twins: 2024-02-29 is a real date, 2023-02-29 is not.
    assert_eq!(code(&add(&t, "l-3", "2024-02-29", "154", "memory", "", "leap day row")), 0);
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, "l-4", "2023-02-29", "154", "memory", "", "not a leap day");
    refused_untouched(&t, &o, 1, before);
}

#[test]
fn refuses_row_with_a_missing_or_bad_field() {
    let t = Tmp::new();
    // A missing --text, --id or --date is a refusal naming the field; no inbox file is even created.
    for args in [
        vec![s("friction"), s("add"), s("--config"), t.config().to_str().unwrap().to_string(), s("--id"), s("l-1"), s("--date"), s("2026-11-01"), s("--ticket"), s("154"), s("--where-it-lives"), s("memory")],
        vec![s("friction"), s("add"), s("--config"), t.config().to_str().unwrap().to_string(), s("--date"), s("2026-11-01"), s("--ticket"), s("154"), s("--where-it-lives"), s("memory"), s("--text"), s("t")],
        vec![s("friction"), s("add"), s("--config"), t.config().to_str().unwrap().to_string(), s("--id"), s("l-1"), s("--ticket"), s("154"), s("--where-it-lives"), s("memory"), s("--text"), s("t")],
        vec![s("friction"), s("add"), s("--config"), t.config().to_str().unwrap().to_string(), s("--id"), s("l-1"), s("--date"), s("2026-11-01"), s("--ticket"), s("154"), s("--text"), s("t")],
    ] {
        let o = run(&args);
        refused_untouched(&t, &o, 1, None);
    }
    // Empty and non-token ids are refused too (an empty tag value is passed, not omitted).
    assert_eq!(code(&add(&t, "l-1", "2026-11-01", "154", "", "", "t")), 1);
    assert_eq!(code(&add(&t, "Bad Id", "2026-11-01", "154", "memory", "", "t")), 1);
    assert_eq!(code(&add(&t, "l-1", "2026-11-01", "Ticket/9", "memory", "", "t")), 1);
    assert!(!t.inbox().exists(), "no refused row may create the inbox");
}

#[test]
fn lint_refuses_missing_field_unknown_field_self_break_and_duplicate() {
    let t = Tmp::new();
    let good = r#"{"id":"l-1","date":"2026-11-01","ticket":"154","where-it-lives":"memory","breaks":"","text":"a clean row"}"#;
    // a hand-written row missing a field
    fs::write(t.inbox(), format!("{good}\n{{\"id\":\"l-2\",\"date\":\"2026-11-01\",\"ticket\":\"154\",\"where-it-lives\":\"memory\",\"breaks\":\"\"}}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("line 2") && err(&o).contains("text"), "{}", err(&o));
    // an unknown field
    fs::write(t.inbox(), format!("{{\"id\":\"l-2\",\"date\":\"2026-11-01\",\"ticket\":\"154\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"x\",\"extra\":\"y\"}}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("line 1") && err(&o).contains("unknown field"), "{}", err(&o));
    // a `breaks` naming its own row
    fs::write(t.inbox(), format!("{good}\n{{\"id\":\"l-2\",\"date\":\"2026-11-01\",\"ticket\":\"154\",\"where-it-lives\":\"memory\",\"breaks\":\"l-2\",\"text\":\"myself\"}}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("line 2") && err(&o).contains("itself"), "{}", err(&o));
    // a duplicate id across lines
    fs::write(t.inbox(), format!("{good}\n{good}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("line 2") && err(&o).contains("duplicate"), "{}", err(&o));
    // a forward `breaks` reference that no later row satisfies
    fs::write(t.inbox(), format!("{good}\n{{\"id\":\"l-2\",\"date\":\"2026-11-01\",\"ticket\":\"154\",\"where-it-lives\":\"memory\",\"breaks\":\"l-9\",\"text\":\"forward\"}}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("line 2") && err(&o).contains("no lesson"), "{}", err(&o));
}

#[test]
fn a_torn_tail_blocks_lint_and_add() {
    let t = Tmp::new();
    let good = r#"{"id":"l-1","date":"2026-11-01","ticket":"154","where-it-lives":"memory","breaks":"","text":"a clean row"}"#;
    fs::write(t.inbox(), format!("{good}\n{{\"id\":\"l-2\"")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("torn tail") && err(&o).contains("line 2"), "{}", err(&o));
    // An append onto a torn inbox is refused and leaves it byte-identical.
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, "l-3", "2026-11-01", "154", "memory", "", "must not land on a torn tail");
    refused_untouched(&t, &o, 1, before);
    // A close check over a torn inbox fails closed: it cannot be compared against.
    let o = close_check(&t, &t.file("n.md", "lesson: none\n"), "154");
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
}

#[test]
fn add_refuses_when_the_inbox_already_has_a_bad_row() {
    let t = Tmp::new();
    let bad = r#"{"id":"l-1","date":"2026-11-01","ticket":"154","where-it-lives":"hook","breaks":"","text":"a bad row"}"#;
    fs::write(t.inbox(), format!("{bad}\n")).unwrap();
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, "l-2", "2026-11-01", "154", "memory", "", "a good row after a bad one");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("line 1"), "the damaged line is named: {}", err(&o));
}

#[test]
fn no_privacy_list_refuses_without_storing() {
    let t = Tmp::new();
    let mut c = base();
    c.args([
        "friction",
        "add",
        "--config",
        t.config().to_str().unwrap(),
        "--id",
        "l-1",
        "--date",
        "2026-11-01",
        "--ticket",
        "154",
        "--where-it-lives",
        "memory",
        "--text",
        "unscanned text",
    ])
    .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_eq!(code(&o), 2, "no list configured is a setup error: {}", err(&o));
    assert!(out(&o).is_empty());
    assert!(!t.inbox().exists(), "nothing is stored unscanned");
}

#[test]
fn an_oversize_row_is_refused_and_nothing_is_written() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    let before = fs::read(t.inbox()).ok();
    let big = format!("{} {}", "word".repeat(6000), "tail");
    let o = add(&t, "l-2", "2026-11-01", "154", "memory", "", &big);
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("cap"), "{}", err(&o));
}

#[test]
fn usage_errors_exit_two_with_stdout_empty() {
    let t = Tmp::new();
    let cfg = t.config().to_str().unwrap().to_string();
    for args in [
        vec![s("friction")],
        vec![s("friction"), s("nope")],
        vec![s("friction"), s("add"), s("--config"), cfg.clone(), s("--wat")],
        vec![s("friction"), s("add"), s("--config"), cfg.clone(), s("--id")],
        vec![s("friction"), s("add"), s("stray"), s("--config"), cfg.clone()],
        vec![s("friction"), s("lint"), s("--config"), cfg.clone(), s("--config"), cfg.clone()],
        vec![s("friction"), s("close-check"), s("--config"), cfg.clone()],
        vec![s("friction"), s("close-check"), s("note.md"), s("--config"), cfg.clone(), s("--ticket"), s("Bad Ticket")],
        vec![s("friction"), s("lint")],
    ] {
        let o = run(&args);
        assert_eq!(code(&o), 2, "{:?}: {}", args, err(&o));
        assert!(out(&o).is_empty(), "{:?}: {}", args, out(&o));
    }
}
