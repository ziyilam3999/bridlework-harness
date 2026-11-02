//! Process-level tests for `kind: pattern` + `repeats`, the N-threshold listing and the trial record (ticket 192).
//! Every check runs the real binary as a child process and reads the real inbox file, so the exit-code contract
//! (0 ok; 1 a refusal — a refused write leaves the inbox byte-identical; 2 a usage or setup error) and "nothing
//! on stdout when erroring" are exercised end to end. Every row, marker and deny word is synthetic, built at run
//! time; the privacy list is injected on the child only, and no test reads an environment variable. Each refusal
//! test plants the bad condition in the fixture itself (a hand-written row for `lint`, a flag for `add`), so it
//! cannot pass for the wrong reason.
#![cfg(unix)]
use harness::friction;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
const SCHEMA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../schemas/friction-trial.schema.json");
const PLACEHOLDER: &str = "/placeholder/friction-inbox.jsonl";
/// A synthetic deny word: the ONLY entry of the injected list. No clean fixture may contain it.
const DENY: &str = "zzdenyzz";
/// A unique marker planted in row TEXT: `patterns` must never print it (privacy AC).
const TEXT_MARK: &str = "unique-row-text-marker-192";
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("fricpat-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// The public example with the placeholder inbox path swapped for this scratch inbox.
    fn example_text(&self) -> String {
        let text = fs::read_to_string(EXAMPLE).unwrap();
        let text = text.replace(PLACEHOLDER, self.inbox().to_str().unwrap());
        assert!(text.contains(self.inbox().to_str().unwrap()), "the example must carry the placeholder inbox path");
        text
    }

    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let f = self.0.join(name);
        fs::write(&f, body).unwrap();
        f
    }

    /// The example as shipped (threshold key present or absent, exactly as the example carries it).
    fn config(&self) -> PathBuf {
        self.file("config.yaml", self.example_text())
    }

    /// The example with NO `pattern_threshold` key at all: the absent case, default 3.
    fn config_no_threshold(&self) -> PathBuf {
        let text = self.example_text();
        let text = text.lines().filter(|l| !l.contains("pattern_threshold")).collect::<Vec<&str>>().join("\n");
        assert!(!text.contains("pattern_threshold"), "the filter must remove the key");
        self.file("config-no-threshold.yaml", format!("{text}\n"))
    }

    /// The example with `pattern_threshold` set to the given value.
    fn config_threshold(&self, v: &str) -> PathBuf {
        let text = self.example_text();
        let text = text.replace("pattern_threshold: 3", &format!("pattern_threshold: {v}"));
        assert!(text.contains(&format!("pattern_threshold: {v}")), "the example must carry the key to edit");
        self.file("config-threshold.yaml", text)
    }

    fn inbox(&self) -> PathBuf {
        self.0.join("inbox.jsonl")
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

/// One synthetic row line: the six required keys, plus `kind` and `repeats` when given (empty = omitted).
fn row_line(id: &str, kind: &str, repeats: &str) -> String {
    let mut extra = String::new();
    if !kind.is_empty() {
        extra.push_str(&format!(",\"kind\":\"{kind}\""));
    }
    if !repeats.is_empty() {
        extra.push_str(&format!(",\"repeats\":\"{repeats}\""));
    }
    format!("{{\"id\":\"{id}\",\"date\":\"2026-11-02\",\"ticket\":\"192\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"a synthetic row\"{extra}}}")
}

/// `friction add` for this ticket's fixtures: ticket 192, tag memory, a clean text; `kind`/`repeats` empty = flag omitted.
fn add(_t: &Tmp, cfg: &Path, id: &str, kind: &str, repeats: &str, text: &str) -> Output {
    let mut a: Vec<String> = vec![
        s("friction"),
        s("add"),
        s("--config"),
        cfg.to_str().unwrap().to_string(),
        s("--id"),
        s(id),
        s("--date"),
        s("2026-11-02"),
        s("--ticket"),
        s("192"),
        s("--where-it-lives"),
        s("memory"),
        s("--text"),
        s(text),
    ];
    if !kind.is_empty() {
        a.push(s("--kind"));
        a.push(s(kind));
    }
    if !repeats.is_empty() {
        a.push(s("--repeats"));
        a.push(s(repeats));
    }
    run(&a)
}

/// `friction lint` against the given config.
fn lint(cfg: &Path) -> Output {
    run(&[s("friction"), s("lint"), s("--config"), cfg.to_str().unwrap().to_string()])
}

/// `harness config check` against the given config.
fn config_check(cfg: &Path) -> Output {
    run(&[s("config"), s("check"), s("--config"), cfg.to_str().unwrap().to_string()])
}

/// `friction patterns [--record <file>]` against the given config.
fn patterns(cfg: &Path, record: Option<&Path>) -> Output {
    let mut a: Vec<String> = vec![s("friction"), s("patterns"), s("--config"), cfg.to_str().unwrap().to_string()];
    if let Some(r) = record {
        a.push(s("--record"));
        a.push(s(r.to_str().unwrap()));
    }
    run(&a)
}

/// `friction close-check` against the default scratch config.
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

/// A refusal: the given exit code, nothing on stdout, and the inbox byte-identical.
fn refused_untouched(t: &Tmp, o: &Output, want: i32, before: Option<Vec<u8>>) {
    assert_eq!(code(o), want, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty on a refusal: {}", out(o));
    assert_eq!(fs::read(t.inbox()).ok(), before, "a refused write changed the inbox");
}

/// A recursive listing of the whole scratch root: (relative path, bytes) for every file, sorted.
fn snap(p: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            } else {
                let rel = e.path().strip_prefix(p).unwrap().to_string_lossy().to_string();
                out.push((rel, fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// Seed one pattern chain root (kind pattern, no repeats) and assert it stored.
fn seed_root(t: &Tmp, cfg: &Path, id: &str) {
    let o = add(t, cfg, id, "pattern", "", "a procedure that worked");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), format!("stored id={id}\n"));
}

/// Seed one repeat of the given root and assert it stored.
fn seed_repeat(t: &Tmp, cfg: &Path, id: &str, root: &str) {
    let o = add(t, cfg, id, "pattern", root, "the same procedure worked again");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), format!("stored id={id}\n"));
}

// ---------------------------------------------------------------- AC 2: the pointer rules, one test each

/// `kind: lesson` — a value outside {friction, pattern} is refused by lint (line named, stdout empty) and by add.
#[test]
fn refuses_unknown_kind() {
    let t = Tmp::new();
    // add: the condition through the verb, inbox byte-identical (run first: the lint leg plants the bad row).
    let cfg = t.config();
    let o = add(&t, &cfg, "f-1", "", "", "a clean friction row first");
    assert_eq!(code(&o), 0, "{}", err(&o));
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, &cfg, "p-1", "lesson", "", "a row with a made-up kind");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("kind"), "{}", err(&o));
    assert!(!err(&o).contains("a row with a made-up kind"), "the text is never echoed: {}", err(&o));
    // lint: the fixture carries the bad row at line 2.
    fs::write(t.inbox(), format!("{}\n{}\n", row_line("f-1", "", ""), row_line("p-1", "lesson", ""))).unwrap();
    let o = lint(&cfg);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 2"), "the line must be named: {}", err(&o));
    assert!(err(&o).contains("kind"), "the field must be named: {}", err(&o));
}

/// `repeats` naming an id that is not in the inbox: refused by lint and by add.
#[test]
fn refuses_repeats_to_missing_id() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "a");
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, &cfg, "p-1", "pattern", "gone-404", "points at nothing");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("repeats"), "{}", err(&o));
    fs::write(t.inbox(), format!("{}\n{}\n", row_line("a", "pattern", ""), row_line("p-1", "pattern", "gone-404"))).unwrap();
    let o = lint(&cfg);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 2"), "{}", err(&o));
    assert!(err(&o).contains("repeats"), "{}", err(&o));
}

/// `repeats` naming its own row: refused by lint and by add.
#[test]
fn refuses_repeats_to_itself() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "a");
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, &cfg, "p-9", "pattern", "p-9", "points at itself");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("repeats"), "{}", err(&o));
    fs::write(t.inbox(), format!("{}\n{}\n", row_line("a", "pattern", ""), row_line("p-9", "pattern", "p-9"))).unwrap();
    let o = lint(&cfg);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 2"), "{}", err(&o));
    assert!(err(&o).contains("repeats"), "{}", err(&o));
}

/// `repeats` naming a row that exists but is NOT `kind: pattern`: refused by lint and by add (AC 11's mutant pins this check).
#[test]
fn refuses_repeats_to_a_friction_row() {
    let t = Tmp::new();
    let cfg = t.config();
    let o = add(&t, &cfg, "f-1", "", "", "a plain friction row");
    assert_eq!(code(&o), 0, "{}", err(&o));
    seed_root(&t, &cfg, "a");
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, &cfg, "p-1", "pattern", "f-1", "points at a friction row");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("repeats"), "{}", err(&o));
    fs::write(t.inbox(), format!("{}\n{}\n{}\n", row_line("f-1", "", ""), row_line("a", "pattern", ""), row_line("p-1", "pattern", "f-1"))).unwrap();
    let o = lint(&cfg);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 3"), "{}", err(&o));
    assert!(err(&o).contains("repeats"), "{}", err(&o));
}

/// A row that CARRIES `repeats` but is not `kind: pattern` (no kind at all): refused by lint and by add.
#[test]
fn refuses_repeats_on_a_friction_row() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "a");
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, &cfg, "p-1", "", "a", "a friction row carrying repeats");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("repeats"), "{}", err(&o));
    fs::write(t.inbox(), format!("{}\n{}\n", row_line("a", "pattern", ""), row_line("p-1", "", "a"))).unwrap();
    let o = lint(&cfg);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 2"), "{}", err(&o));
    assert!(err(&o).contains("repeats"), "{}", err(&o));
}

/// `repeats` naming a row that itself carries `repeats` (not the chain root): refused by lint and by add.
#[test]
fn refuses_repeats_to_a_non_root() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "a");
    seed_repeat(&t, &cfg, "b", "a");
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, &cfg, "c", "pattern", "b", "points at a later repeat, not the root");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("repeats"), "{}", err(&o));
    fs::write(
        t.inbox(),
        format!("{}\n{}\n{}\n", row_line("a", "pattern", ""), row_line("b", "pattern", "a"), row_line("c", "pattern", "b")),
    )
    .unwrap();
    let o = lint(&cfg);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 3"), "{}", err(&o));
    assert!(err(&o).contains("repeats"), "{}", err(&o));
}

/// `repeats: "Pat 1"` — not a plain id token: refused by lint and by add.
#[test]
fn refuses_non_token_repeats() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "a");
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, &cfg, "p-1", "pattern", "Pat 1", "a non-token pointer");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("repeats"), "{}", err(&o));
    assert!(!err(&o).contains("Pat 1"), "the bad value is never echoed: {}", err(&o));
    fs::write(t.inbox(), format!("{}\n{}\n", row_line("a", "pattern", ""), row_line("p-1", "pattern", "Pat 1"))).unwrap();
    let o = lint(&cfg);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("line 2"), "{}", err(&o));
    assert!(err(&o).contains("repeats"), "{}", err(&o));
}

// ---------------------------------------------------------------- AC 3: the green chain

/// A root pattern row (no repeats) plus three repeats: every add stores with `stored id=<id>`, lint is clean.
#[test]
fn clean_pattern_chain_lints_and_stores() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "pat-a");
    for id in ["pat-a-r1", "pat-a-r2", "pat-a-r3"] {
        seed_repeat(&t, &cfg, id, "pat-a");
    }
    // the stored rows carry the two new keys, exactly as given (the root omits both at their defaults)
    let body = fs::read_to_string(t.inbox()).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(friction::parse_row(lines[0]).is_ok(), "every stored line parses");
    assert!(!lines[0].contains("repeats"), "the root carries no repeats: {}", lines[0]);
    assert!(lines[1].contains("\"kind\":\"pattern\"") && lines[1].contains("\"repeats\":\"pat-a\""), "the repeat carries both keys: {}", lines[1]);
    let o = lint(&cfg);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=4\n");
}

// ---------------------------------------------------------------- AC 4: the threshold

/// With no `pattern_threshold` key (default 3): a pattern with 3 repeats is listed, one with 2 is not.
#[test]
fn threshold_lists_n_not_n_minus_1() {
    let t = Tmp::new();
    let cfg = t.config_no_threshold();
    seed_root(&t, &cfg, "a");
    for id in ["a-r1", "a-r2", "a-r3"] {
        seed_repeat(&t, &cfg, id, "a");
    }
    seed_root(&t, &cfg, "b");
    for id in ["b-r1", "b-r2"] {
        seed_repeat(&t, &cfg, id, "b");
    }
    let o = patterns(&cfg, None);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "pattern id=a repeats=3\npatterns ok threshold=3 listed=1\n");
}

/// The same inbox with `pattern_threshold: 2`: both patterns are listed, and the threshold is printed from the config.
#[test]
fn threshold_comes_from_config() {
    let t = Tmp::new();
    let cfg = t.config_threshold("2");
    seed_root(&t, &cfg, "a");
    for id in ["a-r1", "a-r2", "a-r3"] {
        seed_repeat(&t, &cfg, id, "a");
    }
    seed_root(&t, &cfg, "b");
    for id in ["b-r1", "b-r2"] {
        seed_repeat(&t, &cfg, id, "b");
    }
    let o = patterns(&cfg, None);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "pattern id=a repeats=3\npattern id=b repeats=2\npatterns ok threshold=2 listed=2\n");
}

// ---------------------------------------------------------------- AC 5: a listing writes nothing

/// No `--record`: the whole scratch tree is byte-identical after the run. With `--record <f>`: exactly one new
/// file appears (the record), holding exactly one line whose keys are the schema's four.
#[test]
fn patterns_creates_no_files() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "a");
    for id in ["a-r1", "a-r2", "a-r3"] {
        seed_repeat(&t, &cfg, id, "a");
    }
    let before = snap(&t.0);
    let o = patterns(&cfg, None);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(snap(&t.0), before, "patterns with no --record changed the tree");
    // with --record: one new file, one new line, and nothing else changed
    let rec = t.0.join("trial.jsonl");
    let o = patterns(&cfg, Some(&rec));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let after = snap(&t.0);
    assert_eq!(after.len(), before.len() + 1, "exactly one new file: {:?}", after.iter().map(|(r, _)| r).collect::<Vec<_>>());
    for (rel, body) in &after {
        if rel != "trial.jsonl" {
            assert!(before.contains(&(rel.clone(), body.clone())), "an untouched file changed: {rel}");
        }
    }
    let line = fs::read_to_string(&rec).unwrap();
    assert_eq!(line.lines().count(), 1, "exactly one trial line");
    assert!(line.ends_with('\n'));
    assert!(line.contains("\"date\":\""), "the line carries date: {line}");
    let date = line.split("\"date\":\"").nth(1).unwrap().split('"').next().unwrap();
    assert!(friction::date_ok(date), "a real calendar date: {date}");
    assert!(line.contains("\"threshold\":3"), "the line carries threshold: {line}");
    assert!(line.contains("\"rows\":4"), "the line carries the inbox row count: {line}");
    assert!(line.contains("\"listed\":[\"a\"]"), "the line carries the listed ids: {line}");
    // the committed schema pins exactly these four keys
    let schema = fs::read_to_string(SCHEMA).expect("the trial schema is committed and utf-8");
    for key in ["date", "threshold", "rows", "listed"] {
        assert!(schema.contains(&format!("\"{key}\"")), "the schema names key {key}");
    }
}

// ---------------------------------------------------------------- AC 6: privacy

/// A unique marker in a stored row's text never reaches `patterns` stdout or stderr: the listing prints ids and counts only.
#[test]
fn patterns_never_prints_text() {
    let t = Tmp::new();
    let cfg = t.config();
    let o = add(&t, &cfg, "a", "pattern", "", &format!("the root text carries {TEXT_MARK}"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    for id in ["a-r1", "a-r2", "a-r3"] {
        let o = add(&t, &cfg, id, "pattern", "a", &format!("repeat {id} text carries {TEXT_MARK}"));
        assert_eq!(code(&o), 0, "{}", err(&o));
    }
    let o = patterns(&cfg, None);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).contains("pattern id=a repeats=3"));
    assert!(!out(&o).contains(TEXT_MARK), "stdout must carry no row text: {}", out(&o));
    assert!(!err(&o).contains(TEXT_MARK), "stderr must carry no row text: {}", err(&o));
}

/// A privacy-list hit planted in `repeats` (a plain token, so only the scan can refuse it) is refused by `add`
/// and by `lint` naming the field and the class, never the bytes.
#[test]
fn kind_and_repeats_are_privacy_scanned() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "a");
    let before = fs::read(t.inbox()).ok();
    let o = add(&t, &cfg, "p-1", "pattern", DENY, "clean text, poisoned pointer");
    refused_untouched(&t, &o, 1, before);
    assert!(err(&o).contains("field repeats"), "{}", err(&o));
    assert!(err(&o).contains("privacy hit"), "{}", err(&o));
    assert!(!err(&o).contains(DENY), "the deny word must not be echoed: {}", err(&o));
    // the same hit in a stored row, through the whole-file lint
    fs::write(t.inbox(), format!("{}\n{}\n", row_line("a", "pattern", ""), row_line("p-1", "pattern", DENY))).unwrap();
    let o = lint(&cfg);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("field repeats"), "{}", err(&o));
    assert!(err(&o).contains("privacy hit"), "{}", err(&o));
    assert!(!err(&o).contains(DENY), "{}", err(&o));
}

// ---------------------------------------------------------------- AC 7: backward compatible

/// A six-key row (no kind, no repeats) parses and re-encodes to exactly its input bytes.
#[test]
fn old_rows_reencode_byte_identical() {
    for line in [
        r#"{"id":"les-001","date":"2026-11-01","ticket":"154","where-it-lives":"memory","breaks":"","text":"the lesson"}"#,
        r#"{"id":"les-002","date":"2026-11-01","ticket":"154","where-it-lives":"gate","breaks":"les-001","text":"a \"quoted\" lesson"}"#,
        r#"{"id":"les-003","date":"2024-02-29","ticket":"154","where-it-lives":"wiki","breaks":"","text":"leap day, and a \\ backslash"}"#,
    ] {
        let row = friction::parse_row(line).unwrap();
        assert_eq!(friction::encode_row(&row).unwrap(), line, "a six-key row must re-encode byte-identical");
    }
}

// ---------------------------------------------------------------- AC 8: close-check unchanged in meaning

/// A ticket with one pattern row passes `lesson: <that id>` and refuses `lesson: none`: a pattern row is an
/// inbox row its ticket captured, exactly like a friction row.
#[test]
fn close_check_counts_pattern_rows() {
    let t = Tmp::new();
    let cfg = t.config();
    seed_root(&t, &cfg, "pat-a");
    seed_repeat(&t, &cfg, "pat-a-r1", "pat-a");
    let note = t.file("close-ok.md", "# close\n\nlesson: pat-a, pat-a-r1\n");
    let o = close_check(&t, &note, "192");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "close-check ok ticket=192 lessons=2\n");
    let o = close_check(&t, &t.file("close-none.md", "# close\n\nlesson: none\n"), "192");
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
}

// ---------------------------------------------------------------- AC 10: the config key

/// `harness config check` accepts a file with no `pattern_threshold` key, and refuses `0` and a non-number
/// naming the key and never the value (the config family's refusal code for a malformed file: 2).
#[test]
fn config_check_refuses_a_bad_pattern_threshold_and_accepts_absent() {
    let t = Tmp::new();
    // absent: accepted (the key is optional)
    let o = config_check(&t.config_no_threshold());
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).starts_with("config ok"), "{}", out(&o));
    // the shipped example (present, well-formed) is accepted too
    let o = config_check(&t.config());
    assert_eq!(code(&o), 0, "{}", err(&o));
    // 0 and x: refused naming the key, never the value
    for bad in ["0", "x"] {
        let o = config_check(&t.config_threshold(bad));
        assert_ne!(code(&o), 0, "{bad}: {}", err(&o));
        assert!(out(&o).is_empty(), "{bad}: {}", out(&o));
        assert!(err(&o).contains("pattern_threshold"), "{bad}: the key must be named: {}", err(&o));
        assert!(!err(&o).contains(&format!("pattern_threshold: {bad}")), "{bad}: the value must not be echoed: {}", err(&o));
    }
}
