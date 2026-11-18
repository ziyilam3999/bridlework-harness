//! Process-level tests of the plan link: `harness ticket intake --plan-index <file>`. Each test builds its own temp folder and temp git repo
//! (local git only), writes its own small index with made-up ids when it needs one, and runs the real binary. Apart from the temp folder
//! location, this file reads no environment variable.
//! Most exit 1 and exit 2 cases assert stdout empty and the exact stderr line.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);
const REPO_ID: &str = "scratch-1";
const NO_LINK: &str = "harness: ticket refused: no plan link: main_plan_ref or off_plan is required";
const BAD_PATH: &str = "harness: error: --plan-index must be an absolute path with no .. part";

struct Fx {
    root: PathBuf,
    repo: PathBuf,
    head: String,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "scratch git call failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Fx {
    fn new() -> Fx {
        let root = std::env::temp_dir().join(format!("hplan-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&repo, &["config", k, v]);
        }
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        Fx { root, repo, head }
    }

    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let f = self.root.join(name);
        fs::write(&f, body).unwrap();
        f
    }

    /// A ticket file: the 11 keys plus `extra` keys.
    fn ticket(&self, extra: &[(&str, &str)]) -> PathBuf {
        let mut pairs: Vec<(&str, String)> = vec![
            ("id", "\"T-1\"".into()),
            ("repo_id", format!("\"{REPO_ID}\"")),
            ("title", "\"A title\"".into()),
            ("group", "\"grp\"".into()),
            ("size", "\"M\"".into()),
            ("objective", "\"An objective\"".into()),
            ("acceptance_criteria", "[\"one\"]".into()),
            ("dependencies", "[]".into()),
            ("risk", "\"normal\"".into()),
            ("owner_role", "\"execution_verification\"".into()),
            ("evidence_links", "[]".into()),
        ];
        for (k, v) in extra {
            pairs.push((k, v.to_string()));
        }
        let body: Vec<String> = pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
        self.file("ticket.json", format!("{{{}}}", body.join(",")))
    }

    fn map(&self) -> String {
        format!("{REPO_ID}={}", self.repo.display())
    }

    /// Run intake; `index` is the `--plan-index` value when given.
    fn intake(&self, ticket: &Path, index: Option<&Path>) -> Run {
        let mut a: Vec<String> = vec!["ticket".into(), "intake".into(), ticket.display().to_string(), "--repo-map".into(), self.map()];
        if let Some(i) = index {
            a.push("--plan-index".into());
            a.push(i.display().to_string());
        }
        run(&a)
    }

    fn ok_line(&self) -> String {
        format!("ticket=T-1 repo_id={REPO_ID} head={}\n", self.head)
    }
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn run(args: &[String]) -> Run {
    let o = Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap();
    Run { code: o.status.code().expect("exited by a signal"), out: String::from_utf8_lossy(&o.stdout).into(), err: String::from_utf8_lossy(&o.stderr).into() }
}

fn expect(r: &Run, code: i32, stderr: &str) {
    assert_eq!(r.code, code, "exit code; stderr: {}", r.err);
    assert_eq!(r.out, "", "stdout is empty");
    assert_eq!(r.err, format!("{stderr}\n"), "the one stderr line");
}

fn expect_ok(fx: &Fx, r: &Run) {
    assert_eq!((r.code, r.out.as_str(), r.err.as_str()), (0, fx.ok_line().as_str(), ""));
}

/// The default index row as ordered (key, raw JSON) pairs.
fn pairs(id: &str) -> Vec<(String, String)> {
    let p = |k: &str, v: &str| (k.to_string(), v.to_string());
    vec![
        p("id", &format!("\"{id}\"")),
        p("title", "\"t\""),
        p("plan_line", "3"),
        p("state", "\"unknown\""),
        p("tickets", "[]"),
        p("deviation", "null"),
        p("trigger", "null"),
        p("code", "[]"),
    ]
}

fn render(p: &[(String, String)]) -> String {
    let body: Vec<String> = p.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{{}}}", body.join(","))
}

fn row(id: &str) -> String {
    render(&pairs(id))
}

fn row_set(key: &str, raw: &str) -> String {
    render(&pairs("R-1").into_iter().map(|(k, v)| if k == key { (k, raw.to_string()) } else { (k, v) }).collect::<Vec<_>>())
}

fn row_without(key: &str) -> String {
    render(&pairs("R-1").into_iter().filter(|(k, _)| k != key).collect::<Vec<_>>())
}

fn two(first: &str) -> String {
    format!("{first}\n{}\n", row("R-2"))
}

fn index_refused(name: &str, body: impl AsRef<[u8]>, reason: &str) {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", body);
    let r = fx.intake(&fx.ticket(&[("off_plan", "\"why\"")]), Some(&idx));
    assert_eq!(r.code, 2, "{name}: {}", r.err);
    expect(&r, 2, &format!("harness: error: plan index: {reason}"));
}

fn index_read(name: &str, body: impl AsRef<[u8]>) {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", body);
    let r = fx.intake(&fx.ticket(&[("off_plan", "\"why\"")]), Some(&idx));
    assert_eq!(r.code, 0, "{name}: {}", r.err);
    expect_ok(&fx, &r);
}

const NOT_ONE: &str = "line 1: does not parse as one JSON object";

/// A default row with a left_estimate key added.
fn row_est(id: &str, est: &str) -> String {
    row(id).replace(r#""code":[]"#, &format!(r#""code":[],"left_estimate":{est}"#))
}

#[test]
fn intake_reads_an_index_with_a_left_estimate() {
    index_read("estimates", format!("{}\n{}\n", row_est("R-1", "0"), row_est("R-2", "7")));
}

#[test]
fn intake_refuses_a_bad_left_estimate() {
    index_refused("minus one", format!("{}\n", row_est("R-1", "-1")), "line 1: field `left_estimate`: wrong type or value");
}

#[test]
fn a_ticket_with_neither_link_is_refused_when_the_index_is_given() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    expect(&fx.intake(&fx.ticket(&[]), Some(&idx)), 1, NO_LINK);
}

#[test]
fn a_ticket_naming_an_id_not_in_the_index_is_refused() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    let t = fx.ticket(&[("main_plan_ref", "[\"HO-999\"]")]);
    expect(&fx.intake(&t, Some(&idx)), 1, "harness: ticket refused: main_plan_ref names HO-999, which is not in the plan index");
}

#[test]
fn a_ticket_naming_one_known_and_one_unknown_id_is_refused() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\",\"HO-999\"]")]);
    expect(&fx.intake(&t, Some(&idx)), 1, "harness: ticket refused: main_plan_ref names HO-999, which is not in the plan index");
}

#[test]
fn a_ticket_with_both_links_is_refused() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\"]"), ("off_plan", "\"why\"")]);
    let line = "harness: ticket refused: `main_plan_ref` and `off_plan` must not both be given";
    expect(&fx.intake(&t, None), 1, line);
    expect(&fx.intake(&t, Some(&idx)), 1, line);
}

#[test]
fn an_empty_plan_ref_list_is_refused() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    let t = fx.ticket(&[("main_plan_ref", "[]")]);
    let line = "harness: ticket refused: `main_plan_ref`: must not be empty";
    expect(&fx.intake(&t, None), 1, line);
    expect(&fx.intake(&t, Some(&idx)), 1, line);
}

#[test]
fn an_empty_or_multi_line_off_plan_is_refused() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    for (raw, line) in [
        ("\"\"", "harness: ticket refused: `off_plan`: must not be empty or blank"),
        ("\"a\\nb\"", "harness: ticket refused: `off_plan`: must not hold a control character"),
    ] {
        let t = fx.ticket(&[("off_plan", raw)]);
        expect(&fx.intake(&t, None), 1, line);
        expect(&fx.intake(&t, Some(&idx)), 1, line);
    }
}

#[test]
fn a_bad_link_shape_is_refused_by_status_too() {
    let fx = Fx::new();
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\"]"), ("off_plan", "\"why\"")]);
    let a: Vec<String> = ["status".to_string(), t.display().to_string(), "--repo-map".to_string(), fx.map()]
        .into_iter()
        .chain(["--result-dir".to_string(), fx.root.join("res").display().to_string(), "--caps-dir".to_string(), fx.root.join("caps").display().to_string()])
        .collect();
    let r = run(&a);
    assert_eq!(r.code, 1, "{}", r.err);
    assert_eq!(r.out, "");
    assert!(r.err.contains("main_plan_ref"), "names the field: {}", r.err);
}

#[test]
fn a_linked_ticket_is_accepted_with_the_same_ok_line() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\",\"R-2\"]")]);
    expect_ok(&fx, &fx.intake(&t, Some(&idx)));
}

#[test]
fn an_off_plan_ticket_is_accepted() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    expect_ok(&fx, &fx.intake(&fx.ticket(&[("off_plan", "\"a tool for the operator\"")]), Some(&idx)));
}

#[test]
fn without_the_flag_a_ticket_with_no_link_is_accepted_as_today() {
    let fx = Fx::new();
    expect_ok(&fx, &fx.intake(&fx.ticket(&[]), None));
}

#[test]
fn without_the_flag_a_linked_ticket_is_accepted() {
    let fx = Fx::new();
    expect_ok(&fx, &fx.intake(&fx.ticket(&[("main_plan_ref", "[\"NOT-IN-ANY-INDEX\"]")]), None));
    expect_ok(&fx, &fx.intake(&fx.ticket(&[("off_plan", "\"why\"")]), None));
}

#[test]
fn a_missing_index_file_exits_2() {
    let fx = Fx::new();
    let t = fx.ticket(&[("off_plan", "\"why\"")]);
    let line = "harness: error: plan index: the file is missing or cannot be read";
    expect(&fx.intake(&t, Some(&fx.root.join("nothing.jsonl"))), 2, line);
    expect(&fx.intake(&t, Some(&fx.root)), 2, line);
}

#[test]
fn an_index_that_does_not_parse_exits_2() {
    index_refused("not json", two("this is not json"), NOT_ONE);
    index_refused("bytes after", two(&format!("{} x", row("R-1"))), NOT_ONE);
    index_refused("not an object", two("[1]"), NOT_ONE);
    index_refused("not utf-8", [0xffu8, 0xfe, b'\n'], "the file is not utf-8");
}

#[test]
fn an_index_with_hostile_json_exits_2() {
    let bom = format!("\u{feff}{}", row("R-1"));
    index_refused("bom", two(&bom), NOT_ONE);
    index_refused("trailing comma in array", two(&row_set("code", "[\"a\",]")), NOT_ONE);
    index_refused("trailing comma in object", two(&format!("{},}}", &row("R-1")[..row("R-1").len() - 1])), NOT_ONE);
    index_refused("trailing comma in deviation", two(&row_set("deviation", "{\"reason\":\"x\",}")), NOT_ONE);
    index_refused("raw nul", two(&row_set("title", "\"a\u{0}b\"")), NOT_ONE);
    index_refused("raw control byte", two(&row_set("title", "\"a\u{1}b\"")), NOT_ONE);
    index_refused("lone surrogate", two(&row_set("title", "\"\\ud800\"")), NOT_ONE);
    index_refused("blank line", format!("{}\n   \n{}\n", row("R-1"), row("R-2")), "line 2: is empty or blank");
}

#[test]
fn an_index_row_with_a_key_twice_exits_2() {
    let top = row("R-1").replacen('{', "{\"id\":\"R-9\",", 1);
    index_refused("top key twice", two(&top), "line 1: field `id`: given twice");
    index_refused("deviation key twice", two(&row_set("deviation", "{\"reason\":\"a\",\"reason\":\"b\"}")), "line 1: field `reason`: given twice");
}

#[test]
fn a_number_or_literal_is_not_read_as_null() {
    for (key, raw) in [("deviation", "1.5"), ("deviation", "true"), ("trigger", "-1"), ("trigger", "[]"), ("trigger", "false"), ("deviation", "[]")] {
        index_refused(&format!("{key}={raw}"), two(&row_set(key, raw)), &format!("line 1: field `{key}`: wrong type or value"));
    }
    for raw in ["-1", "1.5", "1e2", "null", "\"3\"", "18446744073709551616", "true"] {
        index_refused(&format!("plan_line={raw}"), two(&row_set("plan_line", raw)), "line 1: field `plan_line`: wrong type or value");
    }
    index_read("plan_line max", two(&row_set("plan_line", "18446744073709551615")));
    index_read("plan_line zero", two(&row_set("plan_line", "0")));
}

#[test]
fn deep_nesting_exits_2_without_a_crash() {
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    index_refused("deep", two(&row_set("code", &deep)), "line 1: nesting is too deep");
}

#[test]
fn an_index_row_missing_a_field_exits_2() {
    for key in ["id", "title", "plan_line", "state", "tickets", "deviation", "trigger", "code"] {
        index_refused(key, two(&row_without(key)), &format!("line 1: field `{key}`: missing"));
    }
}

#[test]
fn an_index_row_with_a_wrong_type_exits_2() {
    for (key, raw) in [("tickets", "\"x\""), ("deviation", "\"x\""), ("code", "{}"), ("title", "\"\""), ("title", "5"), ("code", "[1]"), ("tickets", "[\"a b\"]")] {
        index_refused(&format!("{key}={raw}"), two(&row_set(key, raw)), &format!("line 1: field `{key}`: wrong type or value"));
    }
    index_refused("deviation key", two(&row_set("deviation", "{\"other\":\"x\"}")), "line 1: field `deviation`: wrong type or value");
    index_refused("deviation value", two(&row_set("deviation", "{\"reason\":1}")), "line 1: field `deviation`: wrong type or value");
}

#[test]
fn an_index_id_outside_the_character_rule_exits_2() {
    let long = format!("\"{}\"", "a".repeat(65));
    for raw in ["\"a,b\"", "\"a b\"", "\"\u{e9}\"", "\"\"", long.as_str()] {
        index_refused(raw, two(&row_set("id", raw)), "line 1: field `id`: wrong type or value");
    }
    index_read("64 bytes", two(&row_set("id", &format!("\"{}\"", "a".repeat(64)))));
}

#[test]
fn an_index_state_outside_the_list_exits_2() {
    for raw in ["\"done\"", "\"Built\"", "\"\"", "3"] {
        index_refused(raw, two(&row_set("state", raw)), "line 1: field `state`: wrong type or value");
    }
}

#[test]
fn each_of_the_six_states_is_read() {
    for s in ["not-built", "in-build", "built", "deviation", "later", "unknown"] {
        index_read(s, two(&row_set("state", &format!("\"{s}\""))));
    }
}

const DEV_OK: &str = r#"{"reason":"r","decided_by":"operator","date":"2026-01-01"}"#;

/// A dropped row with the given raw deviation and title.
fn dropped_row(id: &str, dev: &str, title: &str) -> String {
    render(
        &pairs(id)
            .into_iter()
            .map(|(k, v)| match k.as_str() {
                "state" => (k, "\"dropped\"".to_string()),
                "deviation" => (k, dev.to_string()),
                "title" => (k, format!("\"{title}\"")),
                _ => (k, v),
            })
            .collect::<Vec<_>>(),
    )
}

fn dropped_line(el: &str) -> String {
    format!("harness: ticket refused: main_plan_ref names {el}, which is a dropped row of the plan index")
}

#[test]
fn the_dropped_state_is_read() {
    index_read("dropped", two(&dropped_row("R-1", DEV_OK, "t")));
    index_read("dropped null deviation", two(&dropped_row("R-1", "null", "t")));
}

#[test]
fn a_ticket_naming_a_dropped_row_is_refused() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&dropped_row("R-1", DEV_OK, "t")));
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\"]")]);
    expect(&fx.intake(&t, Some(&idx)), 1, &dropped_line("R-1"));
}

#[test]
fn a_ticket_naming_a_live_row_and_a_dropped_row_is_refused() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&dropped_row("R-1", DEV_OK, "t")));
    let t = fx.ticket(&[("main_plan_ref", "[\"R-2\",\"R-1\"]")]);
    expect(&fx.intake(&t, Some(&idx)), 1, &dropped_line("R-1"));
}

#[test]
fn the_first_bad_element_names_the_refusal() {
    let fx = Fx::new();
    let idx = fx.file("idx.jsonl", two(&dropped_row("R-1", DEV_OK, "t")));
    let t = fx.ticket(&[("main_plan_ref", "[\"HO-999\",\"R-1\"]")]);
    expect(&fx.intake(&t, Some(&idx)), 1, "harness: ticket refused: main_plan_ref names HO-999, which is not in the plan index");
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\",\"HO-999\"]")]);
    expect(&fx.intake(&t, Some(&idx)), 1, &dropped_line("R-1"));
}

#[test]
fn an_index_state_close_to_dropped_exits_2() {
    for raw in ["\"Dropped\"", "\"DROPPED\"", "\"drop\"", "\"dropped \"", "\"\""] {
        index_refused(raw, two(&row_set("state", raw)), "line 1: field `state`: wrong type or value");
    }
}

#[test]
fn a_dropped_refusal_runs_no_git() {
    let fx = Fx::new();
    let not_repo = fx.root.join("plain");
    fs::create_dir_all(&not_repo).unwrap();
    let idx = fx.file("idx.jsonl", two(&dropped_row("R-1", DEV_OK, "t")));
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\"]")]);
    let r = run(&[
        "ticket".to_string(),
        "intake".to_string(),
        t.display().to_string(),
        "--repo-map".to_string(),
        format!("{REPO_ID}={}", not_repo.display()),
        "--plan-index".to_string(),
        idx.display().to_string(),
    ]);
    expect(&r, 1, &dropped_line("R-1"));
}

#[test]
fn a_dropped_refusal_prints_no_row_text() {
    let fx = Fx::new();
    let dev = r#"{"reason":"zqreason9","decided_by":"operator","date":"2026-01-01"}"#;
    let idx = fx.file("idx.jsonl", two(&dropped_row("R-1", dev, "zqtitle9")));
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\"]")]);
    let r = fx.intake(&t, Some(&idx));
    expect(&r, 1, &dropped_line("R-1"));
    for text in [&r.out, &r.err] {
        assert!(!text.contains("zqtitle9") && !text.contains("zqreason9"), "no row text: {text}");
    }
}

#[test]
fn without_the_flag_a_ticket_naming_a_dropped_row_is_accepted() {
    let fx = Fx::new();
    expect_ok(&fx, &fx.intake(&fx.ticket(&[("main_plan_ref", "[\"R-1\"]")]), None));
}

#[test]
fn an_off_plan_ticket_listed_by_a_dropped_row_is_accepted() {
    // Known gap: intake reads main_plan_ref only.
    let fx = Fx::new();
    let row1 = dropped_row("R-1", DEV_OK, "t").replace("\"tickets\":[]", "\"tickets\":[\"T-1\"]");
    let idx = fx.file("idx.jsonl", two(&row1));
    expect_ok(&fx, &fx.intake(&fx.ticket(&[("off_plan", "\"why\"")]), Some(&idx)));
}

#[test]
fn an_index_with_an_unknown_field_exits_2() {
    let extra = row("R-1").replacen('{', "{\"extra\":1,", 1);
    index_refused("unknown", two(&extra), "line 1: an unknown key");
}

#[test]
fn an_index_with_two_rows_of_one_id_exits_2() {
    index_refused("repeat", format!("{}\n{}\n", row("R-1"), row("R-1")), "an id appears on more than one row");
}

#[test]
fn an_empty_index_exits_2() {
    index_refused("zero bytes", "", "the file has no rows");
    index_refused("lone newline", "\n", "line 1: is empty or blank");
}

#[test]
fn an_index_over_the_cap_exits_2() {
    index_refused("over", "x".repeat(1024 * 1024 + 10), "the file is over 1 MiB");
}

#[test]
fn a_row_shaped_like_the_seed_is_read() {
    let fx = Fx::new();
    let dev = row_set("deviation", "{\"reason\":\"r\",\"decided_by\":\"d\",\"date\":\"2026-01-01\",\"source\":\"s\"}");
    let dev = dev.replacen("\"R-1\"", "\"R-2\"", 1).replacen("\"unknown\"", "\"deviation\"", 1);
    let last = row("R-3").replace("\"trigger\":null", "\"trigger\":\"when x\"").replace("\"tickets\":[]", "\"tickets\":[\"T-1\"]").replace("\"code\":[]", "\"code\":[\"a.rs\"]");
    // The last row has no newline after it.
    let idx = fx.file("idx.jsonl", format!("{}\n{dev}\n{last}", row("R-1")));
    let t = fx.ticket(&[("main_plan_ref", "[\"R-1\",\"R-2\",\"R-3\"]")]);
    expect_ok(&fx, &fx.intake(&t, Some(&idx)));
    // A CR LF line ending is read.
    let crlf = fx.file("crlf.jsonl", format!("{}\r\n{}\r\n", row("R-1"), row("R-2")));
    let t2 = fx.ticket(&[("main_plan_ref", "[\"R-1\",\"R-2\"]")]);
    expect_ok(&fx, &fx.intake(&t2, Some(&crlf)));
}

#[test]
fn the_index_is_read_before_the_ticket_file() {
    let fx = Fx::new();
    let gone = fx.root.join("no-ticket.json");
    expect(&fx.intake(&gone, Some(&fx.root.join("nothing.jsonl"))), 2, "harness: error: plan index: the file is missing or cannot be read");
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    expect(&fx.intake(&gone, Some(&idx)), 2, "harness: error: the ticket file cannot be read");
}

#[test]
fn index_refusals_print_no_row_text_and_no_path() {
    let fx = Fx::new();
    let dir = fx.root.join("zqfolder9");
    fs::create_dir_all(&dir).unwrap();
    let idx = dir.join("idx.jsonl");
    fs::write(&idx, two(&row_set("title", "\"zqtitle9\"").replace("\"unknown\"", "\"zqstate9\""))).unwrap();
    let r = fx.intake(&fx.ticket(&[("off_plan", "\"why\"")]), Some(&idx));
    assert_eq!(r.code, 2);
    for text in [&r.out, &r.err] {
        assert!(!text.contains("zqtitle9") && !text.contains("zqstate9") && !text.contains("zqfolder9"), "no row text and no path: {text}");
    }
    let missing = fx.intake(&fx.ticket(&[("off_plan", "\"why\"")]), Some(&dir.join("zqgone9.jsonl")));
    assert!(!missing.err.contains("zqfolder9") && !missing.err.contains("zqgone9"), "no path: {}", missing.err);
}

#[test]
fn plan_index_flag_usage_errors_exit_2() {
    let fx = Fx::new();
    let t = fx.ticket(&[("off_plan", "\"why\"")]);
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    let base = ["ticket".to_string(), "intake".to_string(), t.display().to_string(), "--repo-map".to_string(), fx.map()];
    let with = |more: Vec<String>| run(&base.iter().cloned().chain(more).collect::<Vec<_>>());
    let i = idx.display().to_string();
    expect(&with(vec!["--plan-index".into()]), 2, "harness: error: --plan-index is missing its value");
    expect(&with(vec!["--plan-index".into(), i.clone(), "--plan-index".into(), i.clone()]), 2, "harness: error: --plan-index was given twice");
    expect(&with(vec![format!("--plan-index={i}")]), 2, "harness: error: unknown option");
    expect(&with(vec!["--plan-index".into(), "idx.jsonl".into()]), 2, BAD_PATH);
    expect(&with(vec!["--plan-index".into(), format!("{}/../idx.jsonl", fx.root.display())]), 2, BAD_PATH);
    // Control: the same line with a good path runs.
    expect_ok(&fx, &with(vec!["--plan-index".into(), i]));
}

#[test]
fn a_refused_link_runs_no_git() {
    let fx = Fx::new();
    let not_repo = fx.root.join("plain");
    fs::create_dir_all(&not_repo).unwrap();
    let idx = fx.file("idx.jsonl", two(&row("R-1")));
    let t = fx.ticket(&[]);
    let a = |ticket: &Path| {
        run(&[
            "ticket".to_string(),
            "intake".to_string(),
            ticket.display().to_string(),
            "--repo-map".to_string(),
            format!("{REPO_ID}={}", not_repo.display()),
            "--plan-index".to_string(),
            idx.display().to_string(),
        ])
    };
    expect(&a(&t), 1, NO_LINK);
    // Control: with a link the repo step runs and refuses the folder.
    let linked = fx.ticket(&[("off_plan", "\"why\"")]);
    expect(&a(&linked), 1, &format!("harness: ticket refused: repo not found for repo id `{REPO_ID}`"));
}
