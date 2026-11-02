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
/// Every test that spawns more than 8 writers, or builds an inbox of 5000 rows or more, takes this lock by name
/// first, so the heavy many-writer tests run one at a time inside the binary: on a small CI runner they would
/// otherwise queue behind each other past the verb's fixed 10 s busy wait (a product behaviour that stays).
static HEAVY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn heavy() -> std::sync::MutexGuard<'static, ()> {
    HEAVY.lock().unwrap_or_else(|e| e.into_inner())
}

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

    /// A scratch config: the public example with the placeholder inbox path swapped for the given inbox.
    fn config_for(&self, inbox: &Path) -> PathBuf {
        let text = fs::read_to_string(EXAMPLE).unwrap();
        let text = text.replace(PLACEHOLDER, inbox.to_str().unwrap());
        assert!(text.contains(inbox.to_str().unwrap()), "the example must carry the placeholder inbox path");
        self.file("config.yaml", text)
    }

    /// The usual scratch config: this scratch inbox.
    fn config(&self) -> PathBuf {
        self.config_for(&self.inbox())
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

#[test]
fn refuses_a_privacy_hit_in_every_field_even_a_valid_token() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    let before = fs::read(t.inbox()).ok();
    // The deny word IS a plain token, so only the privacy scan can refuse it (M9: text-only scanning goes RED).
    // It also names no row, so the breaks leg proves the scan runs BEFORE any cross-check echo (F1).
    let cfg = t.config().to_str().unwrap().to_string();
    for (i, flag) in ["--id", "--ticket", "--breaks"].iter().enumerate() {
        let mut a: Vec<String> = vec![
            s("friction"), s("add"), s("--config"), cfg.clone(),
            s("--id"), s("l-2"), s("--date"), s("2026-11-01"), s("--ticket"), s("154"),
            s("--where-it-lives"), s("memory"), s("--text"), s("clean text, poisoned field"),
        ];
        if i == 0 {
            a[5] = s(DENY); // swap the poisoned value in; --id twice would be a usage error
        } else if i == 1 {
            a[9] = s(DENY);
        } else {
            a.push(s(flag));
            a.push(s(DENY));
        }
        let field = ["id", "ticket", "breaks"][i];
        let o = run(&a);
        refused_untouched(&t, &o, 1, before.clone());
        assert!(err(&o).contains(&format!("field {field}")), "{field}: {}", err(&o));
        assert!(!err(&o).contains(DENY), "{field}: the deny word must not be echoed: {}", err(&o));
    }
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
    // an id from another ticket, named beside ALL of this ticket's rows, so the OTHER-TICKET rule refuses it
    let o = close_check(&t, &t.file("c.md", "lesson: l-1, l-2, l-9\n"), "154");
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("l-9"), "the foreign id is named: {}", err(&o));
    assert!(err(&o).contains("200") && err(&o).contains("not this one"), "the other-ticket rule refused it: {}", err(&o));
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
    // Empty and whitespace-only text are row refusals too (M32): a lesson row carries its lesson.
    for empty in ["", "   \t "] {
        let o = add(&t, "l-1", "2026-11-01", "154", "memory", "", empty);
        assert_eq!(code(&o), 1, "{empty:?}: {}", err(&o));
        assert!(err(&o).contains("text"), "{empty:?}: {}", err(&o));
    }
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

// ---------------------------------------------------------------- fix round 1: no unscanned byte ever echoed (F1)

/// Three private-shaped values, each assembled at run time so this file never spells one out.
fn shapes() -> [String; 3] {
    [
        DENY.to_string(),
        ["/Us", "ers/", "zztester", "/proj"].concat(),
        ["zz", "@", "example", ".", "invalid"].concat(),
    ]
}

/// One hand-written row line (no trailing newline) with the six contract fields and clean values.
fn hand_written(id: &str, ticket: &str, tag: &str, breaks: &str, text: &str) -> String {
    format!("{{\"id\":\"{id}\",\"date\":\"2026-11-01\",\"ticket\":\"{ticket}\",\"where-it-lives\":\"{tag}\",\"breaks\":\"{breaks}\",\"text\":\"{text}\"}}")
}

/// The same row with one extra `key: value` pair bolted on before the closing brace.
fn with_extra_key(line: &str, key: &str, value: &str) -> String {
    format!("{},\"{}\":\"{}\"}}", &line[..line.len() - 1], key, value)
}

#[test]
fn refusals_never_echo_unscanned_bytes() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    for shape in shapes() {
        let before = fs::read(t.inbox()).ok();
        // (a) as the tag of `add`: the field and the reason only, never the value.
        let o = add(&t, "l-2", "2026-11-01", "154", &shape, "", "clean text, poisoned tag");
        refused_untouched(&t, &o, 1, before.clone());
        assert!(err(&o).contains("where-it-lives"), "{}", err(&o));
        assert!(!err(&o).contains(&shape), "the tag value must not be echoed: {}", err(&o));
        // (b) as an unknown key of a hand-written row, read by lint.
        let row = with_extra_key(&hand_written("l-2", "154", "memory", "", "a clean lesson"), &shape, "y");
        fs::write(t.inbox(), format!("{row}\n")).unwrap();
        let o = lint(&t);
        assert_eq!(code(&o), 1, "{}", err(&o));
        assert!(err(&o).contains("line 1") && err(&o).contains("unknown field"), "{}", err(&o));
        assert!(!err(&o).contains(&shape), "the key must not be echoed: {}", err(&o));
        // (c) as a duplicate key of a hand-written row, read by lint.
        let row = format!("{{\"{}\":\"a\",\"{}\":\"b\",{}", &shape, &shape, &hand_written("l-2", "154", "memory", "", "a clean lesson")[1..]);
        fs::write(t.inbox(), format!("{row}\n")).unwrap();
        let o = lint(&t);
        assert_eq!(code(&o), 1, "{}", err(&o));
        assert!(err(&o).contains("line 1") && err(&o).contains("duplicate"), "{}", err(&o));
        assert!(!err(&o).contains(&shape), "the duplicate key must not be echoed: {}", err(&o));
        // (d) as the `lesson:` value of a close note.
        fs::write(t.inbox(), format!("{}\n", hand_written("l-2", "154", "memory", "", "a clean lesson"))).unwrap();
        let note = t.file("note.md", format!("lesson: {shape}\n"));
        let o = close_check(&t, &note, "154");
        assert_eq!(code(&o), 1, "{}", err(&o));
        assert!(out(&o).is_empty());
        assert!(err(&o).contains("lesson"), "{}", err(&o));
        assert!(!err(&o).contains(&shape), "the close-line value must not be echoed: {}", err(&o));
        // (d') an unknown key whose name is private-shaped, read by close-check: refused, no key bytes.
        let row = with_extra_key(&hand_written("l-2", "154", "memory", "", "a clean lesson"), &shape, "y");
        fs::write(t.inbox(), format!("{row}\n")).unwrap();
        let note = t.file("note.md", "lesson: none\n");
        let o = close_check(&t, &note, "999");
        assert_eq!(code(&o), 2, "{}", err(&o));
        assert!(out(&o).is_empty());
        assert!(err(&o).contains("line 1"), "{}", err(&o));
        assert!(!err(&o).contains(&shape), "the key must not be echoed: {}", err(&o));
    }
}

// ---------------------------------------------------------------- fix round 1: lint scans (F2)

#[test]
fn lint_scans_every_field_of_every_row_and_add_refuses_after_it() {
    let t = Tmp::new();
    // A home-path shape in `text` of a hand-written row: lint refuses naming the line and the field, no bytes.
    let row = hand_written("l-1", "154", "memory", "", &["see ", "/Us", "ers/", "zztester", "/proj"].concat());
    fs::write(t.inbox(), format!("{row}\n")).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 1") && err(&o).contains("field text") && err(&o).contains("privacy"), "{}", err(&o));
    assert!(!err(&o).contains("zztester"), "{}", err(&o));
    // `add` uses the same scanning path: it refuses to append after the hit.
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, "l-2", "2026-11-01", "154", "memory", "", "a clean lesson after a poisoned row");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("line 1") && err(&o).contains("field text"), "{}", err(&o));
    // The deny word (a valid token) in `id` and in `ticket` of hand-written rows: same refusal.
    for field in ["id", "ticket"] {
        let id = if field == "id" { DENY } else { "l-3" };
        let ticket = if field == "ticket" { DENY } else { "154" };
        fs::write(t.inbox(), format!("{}\n", hand_written(id, ticket, "memory", "", "a clean lesson"))).unwrap();
        let o = lint(&t);
        assert_eq!(code(&o), 1, "{field}: {}", err(&o));
        assert!(err(&o).contains("line 1") && err(&o).contains(&format!("field {field}")), "{field}: {}", err(&o));
        assert!(!err(&o).contains(DENY), "{field}: {}", err(&o));
    }
    // No privacy list configured: lint fails closed (2), never clean.
    let mut c = base();
    c.args(["friction", "lint", "--config", t.config().to_str().unwrap()]).stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    // close-check scans the same rows: a poisoned row fails the check closed (2), naming the line and field.
    fs::write(t.inbox(), format!("{}\n", hand_written("l-4", "154", "memory", "", &format!("saw {DENY} in the log")))).unwrap();
    let note = t.file("n.md", "lesson: none\n");
    let o = close_check(&t, &note, "154");
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 1") && err(&o).contains("field text") && err(&o).contains("privacy"), "{}", err(&o));
    assert!(!err(&o).contains(DENY), "{}", err(&o));
}

// ---------------------------------------------------------------- fix round 1: one writer at a time (F3)

/// Run N child adds in parallel and collect their outputs.
fn run_parallel(args: Vec<Vec<String>>) -> Vec<Output> {
    let handles: Vec<_> = args.into_iter().map(|a| std::thread::spawn(move || run(&a))).collect();
    handles.into_iter().map(|h| h.join().unwrap()).collect()
}

/// The `add` argv for this config, with the id spelled by the caller (built once, run N times).
fn add_args(cfg: &str, id: &str, text: &str) -> Vec<String> {
    vec![
        s("friction"), s("add"), s("--config"), s(cfg), s("--id"), s(id),
        s("--date"), s("2026-11-01"), s("--ticket"), s("154"),
        s("--where-it-lives"), s("memory"), s("--text"), s(text),
    ]
}

/// A seeded inbox big enough to widen the read-check-append window: many whole rows, all clean.
fn big_inbox(rows: usize) -> String {
    let text = "write the row before the code that reads it and lint the whole inbox before the first append ".repeat(24);
    let mut out = String::new();
    for i in 0..rows {
        out.push_str(&hand_written(&format!("l-b-{i}"), "154", "memory", "", &text));
        out.push('\n');
    }
    out
}

#[test]
fn concurrent_same_id_adds_store_exactly_one_row() {
    const N: usize = 12;
    const SEED: usize = 300;
    let _h = heavy();
    let t = Tmp::new();
    let cfg = t.config().to_str().unwrap().to_string();
    // Round 0: a fresh inbox (nothing to read at all).
    // Rounds 1-3: a big seeded inbox, so the read-check window is wide enough to race.
    for round in 0..4 {
        let seed_rows = if round == 0 { 0 } else { SEED };
        if seed_rows > 0 {
            fs::write(t.inbox(), big_inbox(seed_rows)).unwrap();
        } else {
            let _ = fs::remove_file(t.inbox());
        }
        let id = format!("same-{round}");
        let args: Vec<Vec<String>> = (0..N).map(|_| add_args(&cfg, &id, "one row per id, no matter how many writers")).collect();
        let outs = run_parallel(args);
        let stored: Vec<&Output> = outs.iter().filter(|o| code(o) == 0).collect();
        assert_eq!(stored.len(), 1, "round {round}: exactly one writer may store the id");
        for o in outs.iter().filter(|o| code(o) != 0) {
            assert_eq!(code(o), 1, "round {round}: {}", err(o));
            assert!(out(o).is_empty(), "round {round}: {}", out(o));
            assert!(err(o).contains("duplicate"), "round {round}: {}", err(o));
        }
        let lines = fs::read_to_string(t.inbox()).unwrap().lines().count();
        assert_eq!(lines, seed_rows + 1, "round {round}: one row stored, no more");
        let o = lint(&t);
        assert_eq!(code(&o), 0, "round {round}: {}", err(&o));
        assert_eq!(out(&o), format!("inbox ok rows={}\n", seed_rows + 1));
    }
}

#[test]
fn concurrent_distinct_id_adds_all_store() {
    let _h = heavy();
    let t = Tmp::new();
    let cfg = t.config().to_str().unwrap().to_string();
    let args: Vec<Vec<String>> = (0..40).map(|i| add_args(&cfg, &format!("l-{i}"), "one whole row per writer")).collect();
    let outs = run_parallel(args);
    for o in &outs {
        assert_eq!(code(o), 0, "{}", err(o));
    }
    let body = fs::read_to_string(t.inbox()).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 40);
    for l in &lines {
        assert!(friction::parse_row(l).is_ok(), "every stored row is whole and valid");
    }
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=40\n");
}

// ---------------------------------------------------------------- fix round 1: folder and text edges (F6)

#[test]
fn add_does_not_create_the_inbox_folder() {
    let t = Tmp::new();
    let missing_dir = t.0.join("no-such-dir");
    let cfg = t.config_for(&missing_dir.join("inbox.jsonl")).to_str().unwrap().to_string();
    let o = run(&add_args(&cfg, "l-1", "a lesson with nowhere to live"));
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(!missing_dir.exists(), "the inbox folder is never created by the verb");
    assert!(!missing_dir.join("inbox.jsonl").exists());
}

#[test]
fn a_clean_hand_written_row_lints_and_add_appends() {
    let t = Tmp::new();
    fs::write(t.inbox(), format!("{}\n", hand_written("l-1", "154", "memory", "", "a clean hand written lesson"))).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=1\n");
    let o = add(&t, "l-2", "2026-11-01", "154", "gate", "", "a clean lesson after a clean row");
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=2\n");
}

#[test]
fn a_dash_id_and_escaped_text_store_and_lint_clean() {
    let t = Tmp::new();
    let text = "quote \" backslash \\ newline\nand tab\tand unicode \u{f1} end";
    let o = add(&t, "-", "2026-11-01", "154", "memory", "", text);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let line = fs::read_to_string(t.inbox()).unwrap();
    assert_eq!(line.lines().count(), 1, "the stored row is one line");
    let row = friction::parse_row(line.trim_end_matches('\n')).unwrap();
    assert_eq!((row.id.as_str(), row.text.as_str()), ("-", text));
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=1\n");
}

// ---------------------------------------------------------------- fix round 2: usage paths never echo (F1, M18)

#[test]
fn usage_errors_never_echo_argument_bytes() {
    let t = Tmp::new();
    let cfg = t.config().to_str().unwrap().to_string();
    for shape in shapes() {
        // every verb: an unknown --flag and a stray positional, each carrying a private shape
        let add_full = |flag: String| {
            vec![
                s("friction"), s("add"), s("--config"), cfg.clone(), s("--id"), s("l-1"),
                s("--date"), s("2026-11-01"), s("--ticket"), s("154"), s("--where-it-lives"), s("memory"),
                s("--text"), s("t"), flag,
            ]
        };
        let cases: Vec<Vec<String>> = vec![
            add_full(format!("--{shape}")),
            vec![s("friction"), s("add"), s("--config"), cfg.clone(), shape.clone()],
            vec![s("friction"), s("lint"), s("--config"), cfg.clone(), format!("--{shape}")],
            vec![s("friction"), s("lint"), s("--config"), cfg.clone(), shape.clone()],
            vec![s("friction"), s("close-check"), s("--config"), cfg.clone(), format!("--{shape}")],
            vec![s("friction"), s("close-check"), shape.clone(), s("--config"), cfg.clone(), s("--ticket"), s("154")],
        ];
        for args in cases {
            let o = run(&args);
            assert_eq!(code(&o), 2, "{:?}: {}", args, err(&o));
            assert!(out(&o).is_empty(), "{:?}: {}", args, out(&o));
            assert!(!err(&o).contains(&shape), "a usage error must not echo the argument bytes: {}", err(&o));
        }
    }
    // lint --config with no value, and twice: constants only, nothing typed is echoed (not even the file name)
    let o = run(&[s("friction"), s("lint"), s("--config")]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(!err(&o).contains("config.yaml"), "{}", err(&o));
    let o = run(&[s("friction"), s("lint"), s("--config"), cfg, s("--config"), t.config().to_str().unwrap().to_string()]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(!err(&o).contains("config.yaml"), "{}", err(&o));
}

// ---------------------------------------------------------------- fix round 2: one lock per inbox, one holder (F2/F3)

/// A clean seeded inbox of many short rows: `rows` rows, just under the 4 MiB read cap at 40000.
fn compact_inbox(rows: usize) -> String {
    let mut out = String::new();
    for i in 0..rows {
        out.push_str(&hand_written(&format!("x-{i}"), "154", "memory", "", "l"));
        out.push('\n');
    }
    out
}

/// Open the inbox file the way the verb locks it (read + write, no create), for a test that plays a holder.
fn open_inbox(inbox: &Path) -> fs::File {
    fs::File::options().read(true).write(true).open(inbox).unwrap()
}

/// A config naming `inbox` (any spelling), written next to the scratch inbox under `name`.
fn config_named(t: &Tmp, name: &str, inbox: &Path) -> String {
    let text = fs::read_to_string(EXAMPLE).unwrap().replace(PLACEHOLDER, inbox.to_str().unwrap());
    t.file(name, text).to_str().unwrap().to_string()
}

/// `writers` parallel same-id adds, split round-robin across `cfgs`; exactly one stores, every loser is a
/// duplicate refusal (never a busy exit).
fn assert_one_winner(cfgs: &[&str], writers: usize, id: &str, what: &str) {
    let args: Vec<Vec<String>> = (0..writers).map(|i| add_args(cfgs[i % cfgs.len()], id, "one lock for one inbox")).collect();
    let outs = run_parallel(args);
    let stored = outs.iter().filter(|o| code(o) == 0).count();
    assert_eq!(stored, 1, "{what}: exactly one writer may store the id: {}", outs.iter().map(err).collect::<Vec<_>>().join(" | "));
    for o in outs.iter().filter(|o| code(o) != 0) {
        assert_eq!(code(o), 1, "{what}: a loser is a duplicate refusal, not a busy exit: {}", err(o));
        assert!(err(o).contains("duplicate"), "{what}: {}", err(o));
    }
}

#[test]
fn stale_lock_artefacts_never_block_or_double_store() {
    // The lock is the kernel lock on the inbox file itself, so a file or a folder named `<inbox>.lock` is
    // not a lock at all: it never blocks, never causes a double store, and is never touched or deleted.
    const N: usize = 32;
    const ROUNDS: usize = 10;
    let _h = heavy();
    for dir in [false, true] {
        let t = Tmp::new();
        let cfg = t.config().to_str().unwrap().to_string();
        fs::write(t.inbox(), compact_inbox(200)).unwrap();
        let lock = t.0.join("inbox.jsonl.lock");
        if dir {
            fs::create_dir_all(lock.join("sub")).unwrap();
            fs::write(lock.join("keep.txt"), "keep me").unwrap();
            fs::write(lock.join("sub").join("deep.txt"), "keep me too").unwrap();
        } else {
            fs::write(&lock, "12345").unwrap();
            let past = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
            fs::File::options().write(true).open(&lock).unwrap().set_times(std::fs::FileTimes::new().set_modified(past)).unwrap();
        }
        for round in 0..ROUNDS {
            assert_one_winner(&[&cfg], N, &format!("same-{round}"), &format!("dir={dir} round {round}"));
            let lines = fs::read_to_string(t.inbox()).unwrap().lines().count();
            assert_eq!(lines, 200 + round + 1, "dir={dir} round {round}");
        }
        if dir {
            assert_eq!(fs::read(lock.join("keep.txt")).unwrap(), b"keep me");
            assert_eq!(fs::read(lock.join("sub").join("deep.txt")).unwrap(), b"keep me too");
            assert!(lock.is_dir());
        } else {
            assert_eq!(fs::read(&lock).unwrap(), b"12345", "a stale lock file is ignored, not rewritten");
        }
        let o = lint(&t);
        assert_eq!(code(&o), 0, "dir={dir}: {}", err(&o));
        assert_eq!(out(&o), format!("inbox ok rows={}\n", 200 + ROUNDS));
    }
}

/// Eight parallel same-id adds split across two spellings of one inbox, `rounds` times: exactly one row per round.
fn spelling_case(what: &str, rows: usize, rounds: usize, make: impl Fn(&Tmp) -> (String, String)) {
    let t = Tmp::new();
    fs::write(t.inbox(), compact_inbox(rows)).unwrap();
    let (a, b) = make(&t);
    for round in 0..rounds {
        assert_one_winner(&[&a, &b], 8, &format!("pair-{round}"), &format!("{what} round {round}"));
        let lines = fs::read_to_string(t.inbox()).unwrap().lines().count();
        assert_eq!(lines, rows + round + 1, "{what} round {round}: exactly one row stored");
    }
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{what}: {}", err(&o));
    assert_eq!(out(&o), format!("inbox ok rows={}\n", rows + rounds));
}

#[test]
fn two_spellings_of_one_inbox_take_one_lock() {
    let _h = heavy();
    // (1) a symlink to the inbox FOLDER
    spelling_case("folder symlink", 2000, 10, |t| {
        let alias = t.0.join("alias");
        std::os::unix::fs::symlink(&t.0, &alias).unwrap();
        (t.config().to_str().unwrap().to_string(), config_named(t, "c-alias.yaml", &alias.join("inbox.jsonl")))
    });
    // (2) a symlink to the inbox FILE itself
    spelling_case("file symlink", 2000, 10, |t| {
        let alias = t.0.join("inbox-link.jsonl");
        std::os::unix::fs::symlink(t.inbox(), &alias).unwrap();
        (t.config().to_str().unwrap().to_string(), config_named(t, "c-alias.yaml", &alias))
    });
    // (3) a `./` plus doubled-slash detour (the config contract itself refuses a `..` component, so the
    // `..` spelling can only arrive through a symlinked folder, which case (1) covers)
    spelling_case("dot detour", 2000, 10, |t| {
        let detour = PathBuf::from(format!("{}/.//inbox.jsonl", t.0.display()));
        (t.config().to_str().unwrap().to_string(), config_named(t, "c-alias.yaml", &detour))
    });
    // (4) a HARD link: one inode, two names
    spelling_case("hard link", 2000, 10, |t| {
        let alias = t.0.join("inbox-hard.jsonl");
        fs::hard_link(t.inbox(), &alias).unwrap();
        (t.config().to_str().unwrap().to_string(), config_named(t, "c-alias.yaml", &alias))
    });
}

#[test]
fn hard_linked_inboxes_take_one_lock_with_two_and_eight_writers() {
    let _h = heavy();
    let t = Tmp::new();
    let alias = t.0.join("inbox-hard.jsonl");
    fs::write(t.inbox(), compact_inbox(4000)).unwrap();
    fs::hard_link(t.inbox(), &alias).unwrap();
    let cfg_a = t.config().to_str().unwrap().to_string();
    let cfg_b = config_named(&t, "c-hard.yaml", &alias);
    for (writers, rounds) in [(2usize, 10usize), (8, 10)] {
        for round in 0..rounds {
            // rewrite in place (same inode, the link survives) so every round starts from 4000 rows
            fs::write(t.inbox(), compact_inbox(4000)).unwrap();
            assert_one_winner(&[&cfg_a, &cfg_b], writers, &format!("h{writers}-{round}"), &format!("{writers} writers round {round}"));
            let lines = fs::read_to_string(&alias).unwrap().lines().count();
            assert_eq!(lines, 4001, "{writers} writers round {round}: exactly one row stored");
        }
    }
}

#[test]
fn a_dangling_inbox_symlink_takes_one_lock_on_the_first_ever_append() {
    let _h = heavy();
    let t = Tmp::new();
    let target = t.0.join("real-inbox.jsonl");
    let link = t.0.join("inbox.jsonl"); // t.inbox(): the symlink; the target does not exist yet
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let cfg_link = t.config().to_str().unwrap().to_string();
    let cfg_target = config_named(&t, "c-target.yaml", &target);
    for round in 0..20 {
        let _ = fs::remove_file(&target);
        assert!(!target.exists());
        assert_one_winner(&[&cfg_link, &cfg_target], 8, &format!("first-{round}"), &format!("round {round}"));
        assert_eq!(fs::read_to_string(&target).unwrap().lines().count(), 1, "round {round}: one row in the real inbox");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link is still a link");
    }
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=1\n");
}

#[test]
fn a_killed_holder_never_blocks_the_next_add() {
    let _h = heavy();
    let t = Tmp::new();
    fs::write(t.inbox(), compact_inbox(9000)).unwrap();
    let cfg = t.config().to_str().unwrap().to_string();
    let mut child = {
        let mut c = base();
        c.args(add_args(&cfg, "killed-1", "held by a writer that dies"))
            .env("HARNESS_PRIVACY_LIST_SECRET", DENY)
            .stdin(Stdio::null());
        c.spawn().unwrap()
    };
    // watch the inbox file until the child provably HOLDS its lock (an exclusive lock we cannot take)
    let mut held = false;
    for _ in 0..2000 {
        let f = open_inbox(&t.inbox());
        if matches!(f.try_lock(), Err(std::fs::TryLockError::WouldBlock)) {
            held = true;
            break;
        }
        drop(f);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(held, "the child never held the writer lock");
    child.kill().unwrap(); // SIGKILL, mid critical section
    let _ = child.wait();
    // the kernel released the lock: the same id stores inside the busy window, with no steal wait involved
    let started = std::time::Instant::now();
    let o = run(&add_args(&cfg, "killed-1", "stored after the holder died"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "a dead holder must not cost a busy wait: {:?}", started.elapsed());
    assert_eq!(fs::read_to_string(t.inbox()).unwrap().lines().count(), 9001);
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=9001\n");
}

/// SLOW (waits the documented 10 s busy window once, by design): a LIVE holder fails `add` closed.
#[test]
fn a_live_holder_times_out_into_busy_exit_two() {
    let t = Tmp::new();
    assert_eq!(code(&seed(&t, "l-1", "154", "memory", "")), 0);
    let holder = open_inbox(&t.inbox());
    holder.lock().unwrap(); // this test process is the live holder
    let before = fs::read(t.inbox()).unwrap();
    let started = std::time::Instant::now();
    let o = run(&add_args(&t.config().to_str().unwrap(), "l-2", "waits, then fails closed"));
    drop(holder);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("the inbox is busy"), "{}", err(&o));
    assert!(!err(&o).contains(t.inbox().to_str().unwrap()), "no path on stderr: {}", err(&o));
    assert!(!err(&o).contains("lock"), "no lock path on stderr: {}", err(&o));
    assert!(started.elapsed().as_secs() >= 9, "the busy wait is the documented 10 s, not a fast bail");
    assert_eq!(fs::read(t.inbox()).unwrap(), before, "a busy refusal leaves the inbox untouched");
    // once the holder is gone the same id stores
    let o = run(&add_args(&t.config().to_str().unwrap(), "l-2", "stored once the holder is gone"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=2\n");
}

#[test]
fn a_refused_first_row_never_creates_the_inbox_and_a_first_row_creates_it() {
    let t = Tmp::new();
    // bad tag, and a `breaks` that can only name nothing in an empty inbox: both refused, nothing created
    let o = add(&t, "l-1", "2026-11-01", "154", "hook", "", "t");
    refused_untouched(&t, &o, 1, None);
    let o = add(&t, "l-1", "2026-11-01", "154", "memory", "l-0", "t");
    refused_untouched(&t, &o, 1, None);
    assert!(!t.inbox().exists());
    // a first-ever add into an existing empty folder creates the inbox
    let o = add(&t, "l-1", "2026-11-01", "154", "memory", "", "t");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(fs::read_to_string(t.inbox()).unwrap().lines().count(), 1);
}

// ---------------------------------------------------------------- fix round 2: the cross check is linear (F4)

#[test]
fn cross_check_over_40000_rows_is_linear() {
    let rows: Vec<(usize, friction::Row)> = (0..40000)
        .map(|i| {
            (
                i + 1,
                friction::Row {
                    id: format!("x-{i}"),
                    date: s("2026-11-01"),
                    ticket: s("154"),
                    tag: s("memory"),
                    breaks: if i > 0 { s(&format!("x-{}", i - 1)) } else { s("") },
                    text: s("l"),
                    kind: s(""),
                    repeats: s(""),
                },
            )
        })
        .collect();
    let started = std::time::Instant::now();
    assert!(friction::cross_check(&rows).is_empty());
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(4), "cross_check over 40000 rows took {took:?} (linear bound)");
}

/// The end-to-end shape of F4: a big inbox, 8 parallel same-id adds: a loser is always a duplicate refusal,
/// never a busy timeout. Sized for a 2-core CI runner in a debug build (one full scan of this inbox is well
/// under a second, so the serialized queue stays far inside the 10 s window). The at-cap time bound is carried
/// by `cross_check_over_40000_rows_is_linear`, not by this end-to-end test.
#[test]
fn concurrent_adds_on_a_big_inbox_never_exit_busy() {
    const N: usize = 8;
    let _h = heavy();
    let t = Tmp::new();
    let cfg = t.config().to_str().unwrap().to_string();
    for round in 0..3 {
        fs::write(t.inbox(), compact_inbox(4000)).unwrap();
        let id = format!("same-{round}");
        let args: Vec<Vec<String>> = (0..N).map(|_| add_args(&cfg, &id, "a loser is a duplicate, never a busy exit")).collect();
        let outs = run_parallel(args);
        assert_eq!(outs.iter().filter(|o| code(o) == 0).count(), 1, "round {round}");
        for o in outs.iter().filter(|o| code(o) != 0) {
            assert_eq!(code(o), 1, "round {round}: a loser must be a duplicate refusal, not a busy timeout: {}", err(o));
            assert!(err(o).contains("duplicate"), "round {round}: {}", err(o));
        }
        assert_eq!(fs::read_to_string(t.inbox()).unwrap().lines().count(), 4001, "round {round}");
        let o = lint(&t);
        assert_eq!(code(&o), 0, "round {round}: {}", err(&o));
        assert_eq!(out(&o), "inbox ok rows=4001\n");
    }
}
// ---------------------------------------------------------------- fix round 4: the close-check ticket is scanned (F2)

#[test]
fn close_check_never_prints_an_unscanned_ticket() {
    let t = Tmp::new();
    let note = t.file("note.md", "lesson: none\n");
    let clean_piece = ["x-example", "-qq-x"].concat();
    let mut tickets: Vec<String> = vec![DENY.to_string(), format!("t-{DENY}-1"), format!("t-{DENY}-9"), clean_piece.clone()];
    for shape in shapes() {
        tickets.push(shape);
    }
    for ticket in tickets {
        let o = close_check(&t, &note, &ticket);
        if ticket == clean_piece {
            // a clean token: printed only after a clean scan
            assert_eq!(code(&o), 0, "{}", err(&o));
            assert_eq!(out(&o), format!("close-check ok ticket={ticket} lessons=0\n"));
            continue;
        }
        let c = code(&o);
        assert!(c == 1 || c == 2, "exit {c}: {}", err(&o));
        assert!(out(&o).is_empty(), "stdout must be empty: {}", out(&o));
        assert!(!err(&o).contains(&ticket), "the ticket bytes must not be echoed: {}", err(&o));
        assert!(!out(&o).contains(DENY) && !err(&o).contains(DENY), "the deny word must not be echoed");
        if ticket.contains(DENY) {
            assert_eq!(c, 1, "{}", err(&o));
            assert!(err(&o).contains("field ticket has a privacy hit"), "{}", err(&o));
        }
    }
    let o = close_check(&t, &note, &format!("t-{DENY}-9"));
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("field ticket has a privacy hit (deny-token)"), "{}", err(&o));
    // a clean ticket with no rows still passes and is printed
    let o = close_check(&t, &note, "154");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "close-check ok ticket=154 lessons=0\n");
}

// ---------------------------------------------------------------- fix round 4: a forward `breaks` (N2)

#[test]
fn a_forward_breaks_in_a_hand_written_inbox_lints_clean_and_a_missing_one_does_not() {
    let t = Tmp::new();
    let body = format!(
        "{}\n{}\n",
        hand_written("l-1", "154", "memory", "l-2", "an earlier row naming a later one"),
        hand_written("l-2", "154", "memory", "", "the later row")
    );
    fs::write(t.inbox(), body).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=2\n");
    let body = format!(
        "{}\n{}\n",
        hand_written("l-1", "154", "memory", "l-9", "names nothing"),
        hand_written("l-2", "154", "memory", "", "the later row")
    );
    fs::write(t.inbox(), body).unwrap();
    let o = lint(&t);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("line 1") && err(&o).contains("breaks"), "{}", err(&o));
}
