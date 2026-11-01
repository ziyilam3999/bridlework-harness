//! Process-level tests for `harness config check` (PRE-5c). Every check runs the real binary as a child process, with scratch files
//! and scratch repos under the system temp folder. The config source is set only on the child (`.env`, `.env_remove`); this file never reads one.
//! Each refusal has a planted failing twin: the same run with one thing changed that a wrong implementation would get wrong.
#![cfg(unix)]
use harness::config::{Config, KEYS};
use harness::repo_id::RepoId;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
const MAX: usize = 1024 * 1024;
/// A recognizable synthetic string: no refusal may carry it.
const MARK: &str = "0xA1";
const FOLDER: &str = "/synthetic-folder-0xA1";
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let root = std::env::temp_dir();
        let p = root.join(format!("hcfg-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        let p = p.canonicalize().unwrap();
        // AC 9: scratch lives under the system temp folder, never beside the checkout.
        assert!(p.starts_with(root.canonicalize().unwrap()), "scratch must be under the temp folder");
        Tmp(p)
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

/// Run the binary. `env` is the value of the config variable on the child only (`None` = removed from the child).
fn run(args: &[&str], env: Option<&str>, cwd: Option<&Path>) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null()).env_remove("HARNESS_CONFIG");
    if let Some(v) = env {
        c.env("HARNESS_CONFIG", v);
    }
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().unwrap()
}

fn check(file: &Path, extra: &[&str]) -> Output {
    let f = file.to_str().unwrap();
    let mut a = vec!["config", "check", "--config", f];
    a.extend_from_slice(extra);
    run(&a, None, None)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

/// True when `needle` appears in stdout or stderr. The tests check this helper against a known-leaking string first.
fn leaks(o: &Output, needle: &str) -> bool {
    out(o).contains(needle) || err(o).contains(needle)
}

/// A full valid config. `repos` is the body of the `repos` section; `s_tokens` is the value planted on one budget key.
fn make(repos: &str, s_tokens: &str) -> String {
    format!(
        "repos:\n{repos}\nbudgets:\n  s_wall_seconds: 1\n  s_tokens: {s_tokens}\n  m_wall_seconds: 1\n  m_tokens: 1\n  l_wall_seconds: 1\n  l_tokens: 1\n\nshadow:\n  p1a_cap: 5\n  p4a_cap: 10\n  d2a_cap: 20\n  p7_cap: 10\n  p1a_token_budget: 1\n  p4a_token_budget: 1\n  d2a_token_budget: 1\n  p7_token_budget: 1\n\nlanes:\n  count: 4\n"
    )
}

fn valid() -> String {
    make("", "1")
}

/// A config with distinct planted budget pairs per size.
fn budgets(s: (u64, u64), m: (u64, u64), l: (u64, u64)) -> String {
    format!(
        "repos:\n\nbudgets:\n  s_wall_seconds: {}\n  s_tokens: {}\n  m_wall_seconds: {}\n  m_tokens: {}\n  l_wall_seconds: {}\n  l_tokens: {}\n\nshadow:\n  p1a_cap: 5\n  p4a_cap: 10\n  d2a_cap: 20\n  p7_cap: 10\n  p1a_token_budget: 1\n  p4a_token_budget: 1\n  d2a_token_budget: 1\n  p7_token_budget: 1\n\nlanes:\n  count: 4\n",
        s.0, s.1, m.0, m.1, l.0, l.1
    )
}

/// 1-based number of the first line containing `needle`.
fn line_of(text: &str, needle: &str) -> usize {
    text.split('\n').position(|l| l.contains(needle)).expect("planted line") + 1
}

fn refused_clean(o: &Output) {
    assert_eq!(code(o), 2, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty when erroring: {}", out(o));
}

// ---- AC 1: the public example loads, and only as placeholders ----

#[test]
fn the_public_example_loads_with_no_repos() {
    let o = run(&["config", "check", "--config", EXAMPLE], None, None);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "config ok repos=0 shadow=ok\n");
}

#[test]
fn a_copy_of_the_example_with_a_required_key_deleted_is_refused_by_name() {
    let t = Tmp::new();
    let text = fs::read_to_string(EXAMPLE).unwrap();
    let cut: String = text.lines().filter(|l| !l.contains("p7_cap")).map(|l| format!("{l}\n")).collect();
    let o = check(&t.file("cut.yaml", cut), &[]);
    refused_clean(&o);
    assert!(err(&o).contains("p7_cap"), "{}", err(&o));
}

// ---- AC 2: no config configured is a refusal, not a default ----

#[test]
fn no_flag_and_no_variable_is_a_refusal_and_a_valid_flag_is_not() {
    let t = Tmp::new();
    let good = t.file("ok.yaml", valid());
    let o = run(&["config", "check"], None, None);
    refused_clean(&o);
    assert!(err(&o).contains("no config configured"), "{}", err(&o));
    // Twin: the same command with a valid --config loads.
    let o = run(&["config", "check", "--config", good.to_str().unwrap()], None, None);
    assert_eq!(code(&o), 0);
}

#[test]
fn an_empty_flag_does_not_fall_back_to_the_variable() {
    let t = Tmp::new();
    let good = t.file("ok.yaml", valid());
    let o = run(&["config", "check", "--config", ""], Some(good.to_str().unwrap()), None);
    refused_clean(&o);
    // The empty-source guard is pinned by its own message: the loader would also refuse "", with a different one.
    assert!(err(&o).contains("config source is empty"), "{}", err(&o));
    // Twin: the variable alone loads, and the flag wins over a bad variable.
    assert_eq!(code(&run(&["config", "check"], Some(good.to_str().unwrap()), None)), 0);
    let o = run(&["config", "check", "--config", good.to_str().unwrap()], Some("/no/such/file"), None);
    assert_eq!(code(&o), 0, "the flag must win: {}", err(&o));
}

#[test]
fn an_empty_variable_does_not_fall_back_to_the_example() {
    // The example is reachable from the child's current folder; an empty variable must still refuse.
    let beside = Path::new(EXAMPLE).parent().unwrap();
    let o = run(&["config", "check"], Some(""), Some(beside));
    refused_clean(&o);
    assert!(err(&o).contains("config source is empty"), "{}", err(&o));
}

#[test]
fn a_bad_size_is_exit_two_even_when_the_repo_is_unmapped() {
    let t = Tmp::new();
    let f = t.file("ok.yaml", valid());
    // Bad size + unmapped repo: a command-line error, exit 2, nothing on stdout.
    refused_clean(&check(&f, &["--repo", "c", "--budget", "XL"]));
    refused_clean(&check(&f, &["--budget", "XL", "--repo", "c"]));
    // Twin: a good size + the same unmapped repo is the lookup refusal, exit 1.
    let o = check(&f, &["--repo", "c", "--budget", "S"]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
}

// ---- AC 3: bad files are refused ----

#[test]
fn bad_files_are_refused_and_a_file_of_exactly_one_mebibyte_loads() {
    let t = Tmp::new();
    refused_clean(&check(&t.0.join("missing.yaml"), &[]));
    refused_clean(&check(&t.0, &[]));
    refused_clean(&check(&t.file("empty.yaml", ""), &[]));
    refused_clean(&check(&t.file("bin.yaml", [0xffu8, 0xfe, 0x00, 0x80]), &[]));
    let base = valid();
    let pad = |total: usize| {
        let fill = total - base.len();
        format!("{base}#{}\n", "x".repeat(fill - 2))
    };
    let over = pad(MAX + 1);
    assert_eq!(over.len(), MAX + 1);
    refused_clean(&check(&t.file("over.yaml", over), &[]));
    // Twin: exactly 1 MiB of valid content loads (the cap is exact, not off by one).
    let exact = pad(MAX);
    assert_eq!(exact.len(), MAX);
    let o = check(&t.file("exact.yaml", exact), &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
}

// ---- AC 4 and 6: malformed content is refused, names the key, never echoes the value ----

#[test]
fn the_not_echoed_helper_catches_a_leak() {
    let t = Tmp::new();
    let o = check(&t.file("ok.yaml", valid()), &[]);
    assert!(!leaks(&o, MARK));
    let leaking = Command::new("/bin/sh").args(["-c", &format!("echo {MARK}")]).output().unwrap();
    assert!(leaks(&leaking, MARK));
}

#[test]
fn malformed_content_is_refused_names_the_key_and_the_line_and_never_echoes_the_value() {
    let t = Tmp::new();
    let ctl = format!("  a: {FOLDER}\u{1}x");
    // (label, config text, text that stderr must carry)
    let mut cases: Vec<(&str, String, &str)> = vec![
        ("dot-dot", make(&format!("  a: {FOLDER}/.."), "1"), "repo `a`"),
        ("comment", make(&format!("  a: {FOLDER} #y"), "1"), "key `a`"),
        ("trailing space", make(&format!("  a: {FOLDER} "), "1"), "key `a`"),
        ("colon space", make(&format!("  a: {FOLDER}: y"), "1"), "key `a`"),
        ("inner quote", make(&format!("  a: {FOLDER}\"y"), "1"), "key `a`"),
        ("relative", make("  a: synthetic-folder-0xA1", "1"), "repo `a`"),
        ("tilde", make(&format!("  a: ~{FOLDER}"), "1"), "repo `a`"),
        ("control byte", make(&ctl, "1"), "repo `a`"),
        ("trailing slash", make(&format!("  a: {FOLDER}/"), "1"), "repo `a`"),
        ("duplicate id", make(&format!("  a: {FOLDER}\n  a: {FOLDER}-2"), "1"), "repo `a`"),
        ("duplicate folder", make(&format!("  a: {FOLDER}\n  b: {FOLDER}"), "1"), "repo `b`"),
        ("upper-case id", make(&format!("  Repo: {FOLDER}"), "1"), "repo_id"),
        ("slash id", make(&format!("  a/b: {FOLDER}"), "1"), "repo_id"),
        ("dot-dot id", make(&format!("  ../x: {FOLDER}"), "1"), "repo_id"),
        ("empty id", make(&format!("  : {FOLDER}"), "1"), "repo_id"),
        ("not a number", make("", MARK), "key `s_tokens`"),
        ("zero", make("", "0"), "key `s_tokens`"),
        ("negative", make("", "-1"), "key `s_tokens`"),
        ("fraction", make("", "1.5"), "key `s_tokens`"),
        ("leading zero", make("", "007"), "key `s_tokens`"),
        ("sixteen digits", make("", "1234567890123456"), "key `s_tokens`"),
        ("empty value", make("", "").replace("s_tokens: \n", "s_tokens:\n"), "key `s_tokens`"),
        ("flow map", make("", "{a: 0xA1}"), "key `s_tokens`"),
        ("flow list", make("", "[0xA1]"), "key `s_tokens`"),
        ("anchor", make("", "&0xA1"), "key `s_tokens`"),
        ("alias", make("", "*0xA1"), "key `s_tokens`"),
        ("block", make("", "|0xA1"), "key `s_tokens`"),
        ("fold", make("", ">0xA1"), "key `s_tokens`"),
        ("trailing text", make("", "1 0xA1"), "key `s_tokens`"),
        ("tab", make("", "1").replace("  s_tokens: 1", "\ts_tokens: 1"), "section `budgets`"),
        ("unknown key", make("", "1").replace("  p7_cap: 10", "  p7_cap: 10\n  zz_unknown: 3"), "key `zz_unknown`"),
        ("duplicate key", make("", "1").replace("  s_tokens: 1", "  s_tokens: 1\n  s_tokens: 2"), "key `s_tokens`"),
        ("unknown section", format!("{}other:\n  q: 1\n", valid()), "section `other`"),
        ("duplicate section", format!("{}budgets:\n", valid()), "section `budgets`"),
        ("multi-document", format!("---\n{}", valid()), "section"),
    ];
    // A key name that is not a plain name is never echoed either.
    cases.push(("odd key name", make("", "1").replace("  p7_cap: 10", "  p7_cap: 10\n  Zz0xA1: 3"), "key `key`"));
    for (label, text, want) in &cases {
        let o = check(&t.file("bad.yaml", text), &[]);
        refused_clean(&o);
        let e = err(&o);
        assert!(e.contains(want), "{label}: stderr should name {want:?}, got {e:?}");
        assert!(e.contains("line "), "{label}: stderr should carry a line number: {e:?}");
        // The planted value (and the folder it sits in) must not appear in either stream.
        for secret in [MARK, "synthetic-folder", "1234567890123456", "007"] {
            if text.contains(secret) {
                assert!(!leaks(&o, secret), "{label}: leaked {secret:?}: {e:?}");
            }
        }
    }
    // The line number is the planted line.
    let text = make("", "0");
    let o = check(&t.file("bad.yaml", &text), &[]);
    assert!(err(&o).contains(&format!("line {}:", line_of(&text, "s_tokens"))), "{}", err(&o));
}

#[test]
fn a_missing_key_or_section_is_refused_by_name() {
    let t = Tmp::new();
    let no_key: String = valid().lines().filter(|l| !l.contains("m_tokens")).map(|l| format!("{l}\n")).collect();
    let o = check(&t.file("a.yaml", no_key), &[]);
    refused_clean(&o);
    assert!(err(&o).contains("m_tokens"), "{}", err(&o));
    let cut = valid();
    let no_section = &cut[..cut.find("shadow:").unwrap()];
    let o = check(&t.file("b.yaml", no_section), &[]);
    refused_clean(&o);
    assert!(err(&o).contains("shadow"), "{}", err(&o));
}

// ---- AC 9 (v2): the required `lanes` section and its `count` key ----

#[test]
fn the_lanes_section_is_required_and_count_is_a_positive_whole_number() {
    let t = Tmp::new();
    // no lanes section at all: refused naming the section
    let no_lanes: String = valid().lines().filter(|l| !l.starts_with("lanes") && *l != "  count: 4").map(|l| format!("{l}\n")).collect();
    let o = check(&t.file("no_lanes.yaml", no_lanes), &[]);
    refused_clean(&o);
    assert!(err(&o).contains("lanes"), "{}", err(&o));
    // count 0, a leading zero and a non-number: refused naming the section and the key
    for (label, bad) in [("zero", "0"), ("leading zero", "04"), ("word", "four")] {
        let o = check(&t.file("bad.yaml", valid().replace("count: 4", &format!("count: {bad}"))), &[]);
        refused_clean(&o);
        assert!(err(&o).contains("lanes") && err(&o).contains("count"), "{label}: {}", err(&o));
    }
    // the twin loads: another legal count, read from the file
    let o = check(&t.file("nine.yaml", valid().replace("count: 4", "count: 9")), &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let cfg = Config::parse(&valid().replace("count: 4", "count: 9")).unwrap();
    assert_eq!(cfg.lane_count(), 9);
    assert_eq!(Config::parse(&valid()).unwrap().lane_count(), 4);
    // an extra key under lanes is refused
    let o = check(&t.file("extra.yaml", valid().replace("  count: 4", "  count: 4\n  extra: 1")), &[]);
    refused_clean(&o);
    assert!(err(&o).contains("unknown key"), "{}", err(&o));
}

#[test]
fn two_different_absolute_folders_load_as_the_twin_of_the_map_rules() {
    let t = Tmp::new();
    let o = check(&t.file("ok.yaml", make("  a: /x\n  b: /y", "1")), &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "config ok repos=2 shadow=ok\n");
    // Twin of the plain-value rows: the same line without the comment loads.
    assert_eq!(code(&check(&t.file("ok2.yaml", make("  a: /x", "1")), &[])), 0);
}

// ---- AC 5: the map resolves each id to its own folder, from the config only ----

#[test]
fn the_map_resolves_each_id_to_its_own_folder_from_the_config_only() {
    let t = Tmp::new();
    let repo_a = t.0.join("scratch-a");
    let repo_b = t.0.join("scratch-b");
    let not_c = t.0.join("c");
    for d in [&repo_a, &repo_b, &not_c] {
        fs::create_dir_all(d).unwrap();
    }
    let (a, b) = (repo_a.to_str().unwrap(), repo_b.to_str().unwrap());
    let text = make(&format!("  a: {a}\n  b: {b}"), "1");
    // Library: each id gets its own planted path (not a constant, not the first entry).
    let cfg = Config::parse(&text).unwrap();
    assert_eq!(cfg.repo_dir(&RepoId::parse("a").unwrap()), Some(a));
    assert_eq!(cfg.repo_dir(&RepoId::parse("b").unwrap()), Some(b));
    assert_eq!(cfg.repo_dir(&RepoId::parse("c").unwrap()), None);
    // CLI.
    let f = t.file("map.yaml", &text);
    for (id, want) in [("a", "repo_id=a mapped"), ("b", "repo_id=b mapped")] {
        let o = check(&f, &["--repo", id]);
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert_eq!(out(&o), format!("config ok repos=2 shadow=ok\n{want}\n"));
        assert!(!leaks(&o, a) && !leaks(&o, b), "a folder was printed");
    }
    // An id that is not mapped: exit 1, stdout empty, stderr names the id, no folder.
    let o = check(&f, &["--repo", "c"]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    assert!(err(&o).contains('c') && err(&o).contains("repo_id"), "{}", err(&o));
    assert!(!leaks(&o, a) && !leaks(&o, b));
    // The current folder is named like the missing id: still exit 1 (the map is the only source).
    let o = run(&["config", "check", "--config", f.to_str().unwrap(), "--repo", "c"], None, Some(&not_c));
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    // Twin: the same id mapped in the file loads, whatever the current folder is.
    let o = check(&t.file("map2.yaml", make(&format!("  c: {a}"), "1")), &["--repo", "c"]);
    assert_eq!(code(&o), 0);
}

// ---- AC 7: budgets come from the file, read again on every run ----

#[test]
fn a_budget_edit_shows_on_the_next_run_with_no_code_change() {
    let t = Tmp::new();
    let f = t.0.join("b.yaml");
    fs::write(&f, budgets((111, 222), (555, 666), (777, 888))).unwrap();
    let want = |s: &str, w: u64, tk: u64| format!("config ok repos=0 shadow=ok\nbudget size={s} wall_seconds={w} tokens={tk}\n");
    let o = check(&f, &["--budget", "S"]);
    assert_eq!(out(&o), want("S", 111, 222), "{}", err(&o));
    // Edit the file; the same binary reads the new numbers (a compiled-in pair would print the old ones).
    fs::write(&f, budgets((333, 444), (555, 666), (777, 888))).unwrap();
    let o = check(&f, &["--budget", "S"]);
    assert_eq!(out(&o), want("S", 333, 444));
    // M and L read their own keys.
    assert_eq!(out(&check(&f, &["--budget", "M"])), want("M", 555, 666));
    assert_eq!(out(&check(&f, &["--budget", "L"])), want("L", 777, 888));
    // A bad size is a command-line error, with nothing on stdout.
    for bad in ["XL", "s", "", "SM"] {
        refused_clean(&check(&f, &["--budget", bad]));
    }
}

// ---- command line ----

#[test]
fn a_bad_command_line_is_exit_two_with_nothing_on_stdout() {
    let t = Tmp::new();
    let f = t.file("ok.yaml", valid());
    let f = f.to_str().unwrap();
    for args in [
        vec!["config"],
        vec!["config", "nope"],
        vec!["config", "check", "--config", f, "--wat"],
        vec!["config", "check", "--config", f, "--config", f],
        vec!["config", "check", "--config"],
        vec!["config", "check", "--config", f, "--repo", "Repo"],
        vec!["config", "check", "--config", f, "--repo", ""],
        vec!["config", "check", "--config", f, "--repo"],
    ] {
        refused_clean(&run(&args, None, None));
    }
    // A bad id is a command-line error (2) even when the config is missing too; it is never exit 1.
    refused_clean(&run(&["config", "check", "--repo", "Repo"], None, None));
}

// ---- AC 8: the public part holds no real data (pin) ----

/// The pin over the text of an example file. `Err` names what is wrong.
fn pin(text: &str) -> Result<(), String> {
    let mut section = "";
    let mut keys: Vec<String> = Vec::new();
    for line in text.lines() {
        let bare = line.trim_start();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') {
            section = line.trim_end_matches(':');
            continue;
        }
        let (k, v) = bare.split_once(':').ok_or("entry without a colon")?;
        let v = v.trim();
        match section {
            "repos" => return Err("the example has a repo entry".to_string()),
            "budgets" if v != "1" => return Err(format!("budget {k} is not 1")),
            "shadow" => {
                let want = match k {
                    "p1a_cap" => "5",
                    "p4a_cap" | "p7_cap" => "10",
                    "d2a_cap" => "20",
                    _ => "1",
                };
                if v != want {
                    return Err(format!("shadow {k} is not {want}"));
                }
            }
            "lanes" if v != "4" => return Err(format!("lanes {k} is not 4")),
            _ => {}
        }
        keys.push(format!("{section}.{k}"));
    }
    keys.sort();
    let mut want: Vec<String> = KEYS.iter().map(|k| k.to_string()).collect();
    want.sort();
    if keys != want {
        return Err("the key set differs from config::KEYS".to_string());
    }
    Ok(())
}

#[test]
fn the_public_example_is_placeholders_only_and_each_twin_breaks_the_pin() {
    let text = fs::read_to_string(EXAMPLE).unwrap();
    assert_eq!(pin(&text), Ok(()));
    let twins = [
        ("extra key", text.replace("  p7_cap: 10", "  p7_cap: 10\n  extra_key: 1")),
        ("missing key", text.replace("  p7_cap: 10\n", "")),
        ("repo entry", text.replace("repos:\n", "repos:\n  a: /x\n")),
        ("budget 2", text.replace("  s_tokens: 1", "  s_tokens: 2")),
        ("shadow token budget 2", text.replace("  p4a_token_budget: 1", "  p4a_token_budget: 2")),
        ("p1a cap 6", text.replace("  p1a_cap: 5", "  p1a_cap: 6")),
        ("lanes count 9", text.replace("  count: 4", "  count: 9")),
    ];
    for (label, t) in &twins {
        assert_ne!(t, &text, "{label}: the twin must differ from the example");
        assert!(pin(t).is_err(), "{label}: the pin should fail");
    }
}
