//! Process-level tests for the launch provider and `harness session next-provider`. The real binary runs
//! as a child; records files for the verb are written by the test, the host is a stub shell script.
//! Session names are built at run time from parts.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// A launcher name for `role`, with or without a round token, built from parts.
fn nm(role: &str, round: Option<&str>) -> String {
    let mut p = vec![role, "L2", "t3702"];
    if let Some(r) = round {
        p.push(r);
    }
    p.extend(["M", "opu", "hig"]);
    p.join("-")
}

const STUB: &str = r#"#!/bin/sh
d="$STUB_DIR"
case "$1" in
  --bg)
    : > "$d/marker"
    echo "started"
    echo "$STUB_SHORT session started"
    ;;
  agents) cat "$STUB_AGENTS" ;;
  stop) echo "$2" > "$d/stop"; exit 0 ;;
esac
"#;

struct Env {
    root: PathBuf,
    short: String,
    uuid: String,
}

impl Env {
    fn new() -> Env {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("hlt-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&root);
        for d in ["a", "b", "stub"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        let stub = root.join("stub.sh");
        fs::write(&stub, STUB).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let short = format!("s{:x}{}", std::process::id(), n);
        let uuid = uuid_of(n as u64);
        let e = Env { root, short, uuid };
        let agents = format!(
            "[{{\"id\":\"{}\",\"sessionId\":\"{}\",\"name\":\"{}\"}}]",
            e.short,
            e.uuid,
            nm("exec", Some("r1"))
        );
        fs::write(e.p("agents.json"), agents).unwrap();
        e
    }
    fn p(&self, s: &str) -> PathBuf {
        self.root.join(s)
    }
    fn config(&self) -> PathBuf {
        let f = self.p("config.yaml");
        fs::write(&f, fs::read_to_string(EXAMPLE).unwrap()).unwrap();
        f
    }
    fn marker(&self) -> bool {
        self.p("stub/marker").exists()
    }
    /// Launch the exec name with the given extra args (the records file is passed by the caller).
    fn launch(&self, extra: &[&str]) -> Output {
        let brief = self.p("brief.txt");
        fs::write(&brief, b"do the thing\n").unwrap();
        Command::new(BIN)
            .current_dir(self.p("a"))
            .args(["session", "launch", "--name", &nm("exec", Some("r1")), "--cwd"])
            .arg(self.p("b"))
            .arg("--brief")
            .arg(&brief)
            .arg("--host")
            .arg(self.p("stub.sh"))
            .arg("--config")
            .arg(self.config())
            .args(extra)
            .stdin(Stdio::null())
            .env_remove("HARNESS_CONFIG")
            .env("STUB_DIR", self.p("stub"))
            .env("STUB_SHORT", &self.short)
            .env("STUB_AGENTS", self.p("agents.json"))
            .output()
            .unwrap()
    }
    fn next(&self, rec: &Path, list: &str) -> Output {
        Command::new(BIN).args(["session", "next-provider", "--providers", list, "--records"]).arg(rec).output().unwrap()
    }
    fn launches(&self, rec: &Path) -> Output {
        Command::new(BIN).args(["session", "launches", "--records"]).arg(rec).output().unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn uuid_of(n: u64) -> String {
    format!("{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}", 0x1234_5678u32 + n as u32, 0xabcd, 0x123, 0x456, 0xdead_beef_0000u64 + n)
}
fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

/// A records file built row by row; run ids are `run0`, `run1`, ...
#[derive(Default)]
struct Rows {
    v: Vec<String>,
    n: usize,
}

impl Rows {
    fn raw(&mut self, name: &str, role: &str, ts: u64, tail: &str) -> String {
        let run = format!("run{}", self.n);
        self.n += 1;
        self.v.push(format!(
            "{{\"v\":1,\"kind\":\"launch\",\"ts_ms\":{ts},\"run_id\":\"{run}\",\"name\":\"{name}\",\"role\":\"{role}\",\"brief_sha256\":\"abc\",\"owner\":\"orchestration\"{tail}}}"
        ));
        run
    }
    /// A pending launch row.
    fn pending(&mut self, name: &str, role: &str, provider: Option<&str>) -> String {
        let tail = provider.map(|p| format!(",\"provider\":\"{p}\"")).unwrap_or_default();
        let ts = 1000 + self.n as u64;
        self.raw(name, role, ts, &tail)
    }
    fn bind(&mut self, run: &str) {
        let k: u64 = run.trim_start_matches("run").parse().unwrap();
        self.v.push(format!(
            "{{\"v\":1,\"kind\":\"launch_ids\",\"ts_ms\":5,\"run_id\":\"{run}\",\"short_id\":\"s{k}\",\"session_id\":\"{}\"}}",
            uuid_of(k + 100)
        ));
    }
    fn bound(&mut self, name: &str, role: &str, provider: Option<&str>) -> String {
        let run = self.pending(name, role, provider);
        self.bind(&run);
        run
    }
    fn failed(&mut self, name: &str, role: &str, provider: Option<&str>) {
        let run = self.pending(name, role, provider);
        self.v.push(format!("{{\"v\":1,\"kind\":\"launch_failed\",\"ts_ms\":6,\"run_id\":\"{run}\",\"stage\":\"start\",\"reason\":\"x\"}}"));
    }
    fn refused(&mut self, name: &str, role: &str) {
        let run = format!("run{}", self.n);
        self.n += 1;
        self.v.push(format!(
            "{{\"v\":1,\"kind\":\"launch_refused\",\"ts_ms\":7,\"run_id\":\"{run}\",\"name\":\"{name}\",\"role\":\"{role}\",\"brief_sha256\":\"abc\",\"matched_entry\":\"e1\"}}"
        ));
    }
    fn save(&self, path: &Path) {
        let mut s = self.v.join("\n");
        if !s.is_empty() {
            s.push('\n');
        }
        fs::write(path, s).unwrap();
    }
}

fn exec_r1() -> String {
    nm("exec", Some("r1"))
}

fn want(o: &Output, line: &str) {
    assert_eq!(code(o), 0, "{}", err(o));
    assert_eq!(out(o), format!("{line}\n"));
    assert!(err(o).is_empty(), "{}", err(o));
}

#[test]
fn next_provider_flips_after_a_bound_exec_launch() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=claude last_provider=zai last_run_id=run0 basis=turn");
    want(&e.next(&f, "claude,zai"), "next_provider=claude last_provider=zai last_run_id=run0 basis=turn");
    r.bound(&exec_r1(), "exec", Some("claude"));
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=zai last_provider=claude last_run_id=run1 basis=turn");
    want(&e.next(&f, "claude,zai"), "next_provider=zai last_provider=claude last_run_id=run1 basis=turn");
}

#[test]
fn next_provider_with_no_counted_launch_prints_the_first_name() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    Rows::default().save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=zai last_provider=- last_run_id=- basis=first");
    let mut r = Rows::default();
    r.bound(&nm("rsch", Some("r1")), "rsch", Some("claude"));
    r.bound(&nm("e-rvw", Some("r1")), "e-rvw", Some("claude"));
    r.save(&f);
    want(&e.next(&f, "claude,zai"), "next_provider=claude last_provider=- last_run_id=- basis=first");
}

#[test]
fn next_provider_with_one_name_prints_it() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("claude"));
    r.save(&f);
    want(&e.next(&f, "zai"), "next_provider=zai last_provider=claude last_run_id=run0 basis=single");
    want(&e.next(&f, "claude"), "next_provider=claude last_provider=claude last_run_id=run0 basis=single");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", None);
    r.save(&f);
    want(&e.next(&f, "zai"), "next_provider=zai last_provider=- last_run_id=run0 basis=single");
    Rows::default().save(&f);
    want(&e.next(&f, "zai"), "next_provider=zai last_provider=- last_run_id=- basis=single");
}

#[test]
fn next_provider_after_an_old_row_exits_4() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    r.bound(&exec_r1(), "exec", None);
    r.save(&f);
    let o = e.next(&f, "zai,claude");
    assert_eq!(code(&o), 4, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("harness: next-provider refused: reason=no-provider run_id=run1"), "{}", err(&o));
    let o = e.next(&f, "zai");
    assert_eq!(code(&o), 0, "{}", err(&o));
}

#[test]
fn next_provider_with_an_unknown_last_provider_exits_5() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("gemini"));
    r.save(&f);
    let o = e.next(&f, "zai,claude");
    assert_eq!(code(&o), 5, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("reason=unknown-provider provider=gemini run_id=run0"), "{}", err(&o));
}

#[test]
fn helper_and_reviewer_launches_do_not_move_the_turn() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    for role in ["rsch", "e-rvw", "p-rvw"] {
        r.bound(&nm(role, Some("r1")), role, Some("claude"));
    }
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=claude last_provider=zai last_run_id=run0 basis=turn");
}

#[test]
fn failed_and_refused_launches_do_not_move_the_turn() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    r.failed(&exec_r1(), "exec", Some("claude"));
    r.refused(&exec_r1(), "exec");
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=claude last_provider=zai last_run_id=run0 basis=turn");
}

#[test]
fn a_pending_exec_launch_moves_the_turn() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    r.pending(&exec_r1(), "exec", Some("claude"));
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=zai last_provider=claude last_run_id=run1 basis=turn");
}

#[test]
fn a_fix_round_launch_does_not_move_the_turn() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    r.bound(&nm("exec", Some("r2")), "exec", Some("claude"));
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=claude last_provider=zai last_run_id=run0 basis=turn");
}

#[test]
fn names_without_a_round_count_as_round_one_and_unreadable_names_do_not_count() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound("not-a-session-name", "exec", Some("zai"));
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=zai last_provider=- last_run_id=- basis=first");
    r.bound(&nm("exec", None), "exec", Some("zai"));
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=claude last_provider=zai last_run_id=run1 basis=turn");
    r.bound("not-a-session-name", "exec", Some("claude"));
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=claude last_provider=zai last_run_id=run1 basis=turn");
}

#[test]
fn the_last_launch_is_taken_in_file_order() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    let a = r.raw(&exec_r1(), "exec", 9000, ",\"provider\":\"zai\"");
    r.bind(&a);
    let b = r.raw(&exec_r1(), "exec", 1000, ",\"provider\":\"claude\"");
    r.bind(&b);
    r.save(&f);
    want(&e.next(&f, "zai,claude"), "next_provider=zai last_provider=claude last_run_id=run1 basis=turn");
}

#[test]
fn launch_with_provider_writes_the_field_and_binds() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let o = e.launch(&["--records", f.to_str().unwrap(), "--provider", "zai"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(err(&o).is_empty(), "{}", err(&o));
    let so = out(&o);
    assert_eq!(so.lines().count(), 1);
    assert!(so.starts_with("launched run_id="), "{so}");
    let text = fs::read_to_string(&f).unwrap();
    let first = text.lines().next().unwrap();
    assert!(first.ends_with(",\"owner\":\"orchestration\",\"provider\":\"zai\"}"), "{first}");
    let l = e.launches(&f);
    assert!(out(&l).trim_end().ends_with("state=bound provider=zai"), "{}", out(&l));
    want(&e.next(&f, "zai,claude"), &format!("next_provider=claude last_provider=zai last_run_id={} basis=turn", so.split_whitespace().nth(1).unwrap().trim_start_matches("run_id=")));
}

#[test]
fn launch_without_provider_writes_the_old_row() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let o = e.launch(&["--records", f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(err(&o).is_empty());
    let toks: Vec<String> = out(&o).split_whitespace().map(str::to_string).collect();
    let keys = ["launched", "run_id=", "name=", "role=", "short_id=", "session_id="];
    assert_eq!(toks.len(), keys.len(), "{toks:?}");
    for (t, k) in toks.iter().zip(keys) {
        assert!(t.starts_with(k), "{t}");
    }
    let text = fs::read_to_string(&f).unwrap();
    let row = text.lines().next().unwrap();
    let mut at = 0;
    for k in ["v", "kind", "ts_ms", "run_id", "name", "role", "brief_sha256", "owner"] {
        let pos = row[at..].find(&format!("\"{k}\":")).unwrap_or_else(|| panic!("{k} out of order in {row}"));
        at += pos + k.len();
    }
    assert!(row.ends_with("\"owner\":\"orchestration\"}"), "{row}");
    assert!(!row.contains("provider"));
}

#[test]
fn launch_ignores_a_corrupt_earlier_line() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    fs::write(&f, "garbage\n").unwrap();
    let o = e.launch(&["--records", f.to_str().unwrap(), "--provider", "claude"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let text = fs::read_to_string(&f).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "garbage");
    assert!(lines[1].contains("\"kind\":\"launch\"") && lines[1].contains("\"provider\":\"claude\""));
}

#[test]
fn launches_line_adds_provider_only_when_present() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", None);
    r.bound(&exec_r1(), "exec", Some("zai"));
    let run = r.raw(&nm("e-rvw", Some("r1")), "e-rvw", 3000, ",\"sign_off\":true,\"provider\":\"claude\"");
    r.bind(&run);
    r.save(&f);
    let o = e.launches(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let so = out(&o);
    let ls: Vec<&str> = so.lines().collect();
    assert_eq!(ls.len(), 3);
    assert!(ls[0].ends_with("state=bound"), "{}", ls[0]);
    assert!(ls[1].ends_with("state=bound provider=zai"), "{}", ls[1]);
    assert!(ls[2].ends_with("state=bound sign_off=yes provider=claude"), "{}", ls[2]);
}

#[test]
fn providers_list_refusals_exit_2() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    Rows::default().save(&f);
    let long = "a".repeat(17);
    let lists = [
        "", "zai,", ",zai", ",", "zai,claude,gem", "zai,zai", "Zai", "z ai", "z-ai", "zai;x", "-zai", "1zai", long.as_str(), "zai,Claude",
    ];
    for l in lists {
        let o = e.next(&f, l);
        assert_eq!(code(&o), 2, "list {l:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert!(err(&o).contains("--providers"), "{}", err(&o));
    }
    let rec = f.to_str().unwrap();
    let cases: Vec<Vec<&str>> = vec![
        vec![],
        vec!["--records", rec],
        vec!["--providers"],
        vec!["--providers", "zai", "--providers", "claude", "--records", rec],
        vec!["--providers", "zai", "--records", rec, "--records", rec],
        vec!["--providers", "zai", "--bogus", "x"],
        vec!["--providers", "zai", "extra"],
    ];
    for c in cases {
        let o = Command::new(BIN).args(["session", "next-provider"]).args(&c).output().unwrap();
        assert_eq!(code(&o), 2, "{c:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert!(!err(&o).is_empty());
    }
}

#[test]
fn usage_names_the_provider_flag_and_the_verb() {
    for args in [&["session"][..], &["session", "launch"][..]] {
        let o = Command::new(BIN).args(args).output().unwrap();
        assert_eq!(code(&o), 2);
        assert!(err(&o).contains("--provider") && err(&o).contains("next-provider"), "{}", err(&o));
    }
}

// ---- hostile ----

#[test]
fn a_row_with_provider_twice_is_a_corrupt_line() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    let run = r.raw(&exec_r1(), "exec", 1000, ",\"provider\":\"zai\",\"provider\":\"claude\"");
    r.bind(&run);
    r.save(&f);
    let o = e.next(&f, "zai,claude");
    assert_eq!(code(&o), 3, "{}", err(&o));
    assert!(err(&o).contains("line 1"), "{}", err(&o));
    let o = e.launches(&f);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    let o = Command::new(BIN)
        .args(["session", "sign-off-check", "--ticket", "t1", "--role", "gov", "--session", "x", "--claim", "done", "--records"])
        .arg(&f)
        .output()
        .unwrap();
    assert_eq!(code(&o), 2, "{}", err(&o));
}

#[test]
fn a_torn_last_line_refuses() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    r.save(&f);
    let mut s = fs::read_to_string(&f).unwrap();
    s.push_str("{\"v\":1,\"kind\":\"launch\"");
    fs::write(&f, s).unwrap();
    let o = e.next(&f, "zai,claude");
    assert_eq!(code(&o), 3, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).starts_with("harness: next-provider refused: reason=unreadable"), "{}", err(&o));
    assert!(err(&o).contains("line 3"), "{}", err(&o));
}

#[test]
fn a_provider_with_odd_characters_in_a_row_is_a_corrupt_line() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let long = format!("\"{}\"", "a".repeat(17));
    let odd = ["\"Zai\"", "\"z ai\"", long.as_str(), "5", "\"\"", "\"za\\\"i\"", "\"za\\\\i\"", "null", "true"];
    for v in odd {
        let mut r = Rows::default();
        r.raw(&exec_r1(), "exec", 1000, &format!(",\"provider\":{v}"));
        r.save(&f);
        let o = e.next(&f, "zai,claude");
        assert_eq!(code(&o), 3, "{v}: {}", err(&o));
        assert!(err(&o).contains("line 1"), "{v}: {}", err(&o));
        assert_eq!(code(&e.launches(&f)), 2, "{v}");
    }
}

#[test]
fn a_launch_with_an_odd_provider_name_starts_nothing() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    r.save(&f);
    let before = fs::read(&f).unwrap();
    let rec = f.to_str().unwrap();
    let long = "a".repeat(17);
    for bad in ["Zai", "z ai", "z-ai", "zai;x", "", "-zai", long.as_str()] {
        let o = e.launch(&["--records", rec, "--provider", bad]);
        assert_eq!(code(&o), 2, "{bad:?}: {}", err(&o));
        assert_eq!(fs::read(&f).unwrap(), before, "{bad:?}");
        assert!(!e.marker(), "{bad:?}");
    }
    for extra in [vec!["--records", rec, "--provider", "zai", "--provider", "claude"], vec!["--records", rec, "--provider"]] {
        let o = e.launch(&extra);
        assert_eq!(code(&o), 2, "{extra:?}: {}", err(&o));
        assert_eq!(fs::read(&f).unwrap(), before);
        assert!(!e.marker());
    }
}

#[test]
fn a_records_file_over_the_read_cap_refuses() {
    let e = Env::new();
    let f = e.p("big.jsonl");
    fs::write(&f, vec![b'\n'; (16 << 20) + 1]).unwrap();
    let o = e.next(&f, "zai,claude");
    assert_eq!(code(&o), 3, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("reason=unreadable") && err(&o).contains("read cap"), "{}", err(&o));
}

#[test]
fn a_missing_records_file_refuses_with_exit_3() {
    let e = Env::new();
    let f = e.p("none.jsonl");
    let o = e.next(&f, "zai,claude");
    assert_eq!(code(&o), 3, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).starts_with("harness: next-provider refused: reason=unreadable"), "{}", err(&o));
    assert!(!f.exists());
}
