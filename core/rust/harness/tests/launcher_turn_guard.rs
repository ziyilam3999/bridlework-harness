//! Process-level tests for `harness session launch --turn` and `--turn-reason`. The real binary runs as a
//! child; records files are written by the test, the host is a stub shell script. Session names are built
//! at run time from parts.
#![cfg(unix)]
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
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
        let root = std::env::temp_dir().join(format!("hltg-{}-{}", std::process::id(), n));
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
        let agents = format!("[{{\"id\":\"{}\",\"sessionId\":\"{}\",\"name\":\"{}\"}}]", e.short, e.uuid, exec_r1());
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
    fn cmd(&self, name: &str, extra: &[&str]) -> Command {
        let brief = self.p("brief.txt");
        fs::write(&brief, b"do the thing\n").unwrap();
        let mut c = Command::new(BIN);
        c.current_dir(self.p("a"))
            .args(["session", "launch", "--name", name, "--cwd"])
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
            .env("STUB_AGENTS", self.p("agents.json"));
        c
    }
    /// Launch the round 1 exec name with the given extra args.
    fn launch(&self, extra: &[&str]) -> Output {
        self.launch_as(&exec_r1(), extra)
    }
    fn launch_as(&self, name: &str, extra: &[&str]) -> Output {
        self.cmd(name, extra).output().unwrap()
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
fn exec_r1() -> String {
    nm("exec", Some("r1"))
}
fn text(f: &Path) -> String {
    fs::read_to_string(f).unwrap()
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
    fn save(&self, path: &Path) {
        let mut s = self.v.join("\n");
        if !s.is_empty() {
            s.push('\n');
        }
        fs::write(path, s).unwrap();
    }
}

/// A records file with one bound round 1 exec launch by `provider`.
fn history(f: &Path, provider: Option<&str>) {
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", provider);
    r.save(f);
}

fn rec(f: &Path) -> &str {
    f.to_str().unwrap()
}

#[test]
fn launch_turn_keeps_the_turn_and_records_turn_next() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, Some("zai"));
    let o = e.launch(&["--records", rec(&f), "--provider", "claude", "--turn", "zai,claude"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&f);
    let row = t.lines().nth(2).unwrap();
    assert!(row.ends_with(",\"owner\":\"orchestration\",\"provider\":\"claude\",\"turn_next\":\"claude\"}"), "{row}");
    assert!(!row.contains("turn_reason"));
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&f);
    assert!(t.lines().nth(4).unwrap().contains("\"provider\":\"zai\",\"turn_next\":\"zai\""), "{t}");
}

#[test]
fn launch_turn_on_an_empty_history_takes_the_first_name() {
    let e = Env::new();
    let f = e.p("none.jsonl");
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 7, "{}", err(&o));
    assert!(err(&o).starts_with("launch refused: turn:"), "{}", err(&o));
    assert!(!f.exists() || fs::read(&f).unwrap().is_empty());
    assert!(!e.marker());
    let o = e.launch(&["--records", rec(&f), "--provider", "claude", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&f);
    assert!(t.lines().next().unwrap().contains("\"provider\":\"claude\",\"turn_next\":\"claude\""), "{t}");
}

#[test]
fn launch_turn_refuses_a_repeat_without_reason_exit_7() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, Some("zai"));
    let before = fs::read(&f).unwrap();
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 7, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).starts_with("launch refused: turn: provider zai is not the turn, next is claude"), "{}", err(&o));
    assert_eq!(fs::read(&f).unwrap(), before);
    assert!(!e.marker());
}

#[test]
fn launch_turn_with_reason_records_turn_next_and_reason() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, Some("zai"));
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai", "--turn-reason", "claude is out of quota"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&f);
    let row = t.lines().nth(2).unwrap();
    assert!(
        row.ends_with(",\"provider\":\"zai\",\"turn_next\":\"claude\",\"turn_reason\":\"claude is out of quota\"}"),
        "{row}"
    );
    let l = out(&e.launches(&f));
    let last = l.lines().last().unwrap();
    assert!(last.ends_with("state=bound provider=zai turn_next=claude"), "{last}");
    // a reason on a launch that follows a reasoned row is written too
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai", "--turn-reason", "again"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&f);
    assert!(t.lines().nth(4).unwrap().ends_with("\"turn_next\":\"claude\",\"turn_reason\":\"again\"}"), "{t}");
}

#[test]
fn launch_turn_after_an_old_row_needs_a_reason() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("zai"));
    r.bound(&exec_r1(), "exec", None);
    r.save(&f);
    let before = fs::read(&f).unwrap();
    let o = e.launch(&["--records", rec(&f), "--provider", "claude", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 7, "{}", err(&o));
    assert!(err(&o).starts_with("launch refused: turn: the last counted launch run1 carries no provider"), "{}", err(&o));
    assert_eq!(fs::read(&f).unwrap(), before);
    assert!(!e.marker());
    let o = e.launch(&["--records", rec(&f), "--provider", "claude", "--turn", "claude,zai", "--turn-reason", "old row"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&f);
    let row = t.lines().nth(4).unwrap();
    assert!(row.contains("\"turn_reason\":\"old row\"") && !row.contains("turn_next"), "{row}");
}

#[test]
fn launch_turn_with_an_unknown_last_provider_needs_a_reason() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, Some("gemini"));
    let before = fs::read(&f).unwrap();
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 7, "{}", err(&o));
    assert!(err(&o).starts_with("launch refused: turn: the last counted launch run0 carries provider gemini, not in --turn"), "{}", err(&o));
    assert_eq!(fs::read(&f).unwrap(), before);
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai", "--turn-reason", "gemini lane"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!text(&f).lines().nth(2).unwrap().contains("turn_next"));
}

#[test]
fn launch_turn_with_one_name_goes_on_after_an_old_row() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, None);
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "zai"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(text(&f).lines().nth(2).unwrap().ends_with("\"provider\":\"zai\",\"turn_next\":\"zai\"}"));
}

#[test]
fn launch_turn_refusals_exit_2() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, Some("zai"));
    let before = fs::read(&f).unwrap();
    let r = rec(&f);
    let long = "a".repeat(201);
    let rsch = nm("rsch", Some("r1"));
    let cases: Vec<(String, Vec<&str>)> = vec![
        (exec_r1(), vec!["--records", r, "--turn", "claude,zai"]),
        (exec_r1(), vec!["--records", r, "--provider", "gemini", "--turn", "claude,zai"]),
        (rsch, vec!["--records", r, "--provider", "zai", "--turn", "claude,zai"]),
        (exec_r1(), vec!["--records", r, "--provider", "zai", "--turn-reason", "x"]),
        (exec_r1(), vec!["--records", r, "--provider", "zai", "--turn", "claude,zai", "--turn-reason", &long]),
        (exec_r1(), vec!["--records", r, "--provider", "zai", "--turn", "claude,zai", "--turn-reason", ""]),
        (exec_r1(), vec!["--records", r, "--provider", "zai", "--turn", "claude,zai", "--turn-reason", "   "]),
        (exec_r1(), vec!["--records", r, "--provider", "zai", "--turn", "claude,zai", "--turn-reason", "a\nb"]),
    ];
    for (name, args) in cases {
        let o = e.launch_as(&name, &args);
        assert_eq!(code(&o), 2, "{args:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert_eq!(fs::read(&f).unwrap(), before, "{args:?}");
        assert!(!e.marker(), "{args:?}");
    }
}

#[test]
fn launch_turn_on_a_fix_round_exits_2() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, Some("claude"));
    let before = fs::read(&f).unwrap();
    let o = e.launch_as(&nm("exec", Some("r2")), &["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(err(&o).contains("round 1"), "{}", err(&o));
    assert_eq!(fs::read(&f).unwrap(), before);
    assert!(!e.marker());
}

#[test]
fn launch_turn_reason_with_a_quote_a_backslash_and_non_ascii_text_round_trips() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let reason = "say \"hi\" \\ caf\u{e9} \u{4e2d}\u{6587}";
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai", "--turn-reason", reason]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&f);
    assert_eq!(t.lines().count(), 2, "{t}");
    let row = t.lines().next().unwrap();
    assert!(row.contains("\"turn_reason\":\"say \\\"hi\\\" \\\\ caf\u{e9} \u{4e2d}\u{6587}\"}"), "{row}");
    assert_eq!(code(&e.launches(&f)), 0);
    let exact = "b".repeat(200);
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai", "--turn-reason", &exact]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(text(&f).contains(&format!("\"turn_reason\":\"{exact}\"")));
    assert_eq!(code(&e.launches(&f)), 0);
}

#[test]
fn launch_turn_success_prints_only_the_launched_line() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, Some("zai"));
    let o = e.launch(&["--records", rec(&f), "--provider", "claude", "--turn", "zai,claude"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(err(&o).is_empty(), "{}", err(&o));
    let so = out(&o);
    assert_eq!(so.lines().count(), 1, "{so}");
    let toks: Vec<&str> = so.split_whitespace().collect();
    let keys = ["launched", "run_id=", "name=", "role=", "short_id=", "session_id="];
    assert_eq!(toks.len(), keys.len(), "{toks:?}");
    for (t, k) in toks.iter().zip(keys) {
        assert!(t.starts_with(k), "{t}");
    }
}

#[test]
fn launch_without_turn_ignores_a_corrupt_earlier_line() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    fs::write(&f, "garbage\n").unwrap();
    let o = e.launch(&["--records", rec(&f), "--provider", "claude"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let t = text(&f);
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines[0], "garbage");
    assert!(lines[1].contains("\"kind\":\"launch\"") && !lines[1].contains("turn_"));
}

#[test]
fn launches_line_adds_turn_next_only_when_present() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    let a = r.raw(&exec_r1(), "exec", 1000, ",\"provider\":\"zai\",\"turn_next\":\"zai\",\"turn_reason\":\"why not\"");
    r.bind(&a);
    r.bound(&exec_r1(), "exec", Some("claude"));
    r.bound(&exec_r1(), "exec", None);
    r.save(&f);
    let o = e.launches(&f);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let so = out(&o);
    let ls: Vec<&str> = so.lines().collect();
    assert_eq!(ls.len(), 3);
    assert!(ls[0].ends_with("state=bound provider=zai turn_next=zai"), "{}", ls[0]);
    assert!(ls[1].ends_with("state=bound provider=claude"), "{}", ls[1]);
    assert!(ls[2].ends_with("state=bound"), "{}", ls[2]);
    assert!(!so.contains("why not") && !so.contains("turn_reason"));
}

#[test]
fn usage_names_the_turn_flags() {
    for args in [&["session"][..], &["session", "launch"][..]] {
        let o = Command::new(BIN).args(args).output().unwrap();
        assert_eq!(code(&o), 2);
        assert!(err(&o).contains("--turn ") && err(&o).contains("--turn-reason"), "{}", err(&o));
    }
}

// ---- hostile ----

#[test]
fn two_launches_at_once_alternate() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.pending(&exec_r1(), "exec", Some("zai"));
    r.save(&f);
    let o = e.launch(&["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 7, "{}", err(&o));
    let o = e.launch(&["--records", rec(&f), "--provider", "claude", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
}

fn finish(c: Child) -> Output {
    c.wait_with_output().unwrap()
}

#[test]
fn the_turn_check_reads_the_records_under_the_lock() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let mut r = Rows::default();
    r.bound(&exec_r1(), "exec", Some("claude"));
    r.save(&f);
    let mut held: File = OpenOptions::new().read(true).append(true).open(&f).unwrap();
    held.lock().unwrap();
    let mut c = e.cmd(&exec_r1(), &["--records", rec(&f), "--provider", "zai", "--turn", "claude,zai"]);
    let child = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    r.pending(&exec_r1(), "exec", Some("zai"));
    let row = format!("{}\n", r.v.last().unwrap());
    held.write_all(row.as_bytes()).unwrap();
    held.sync_all().unwrap();
    drop(held);
    let o = finish(child);
    assert_eq!(code(&o), 7, "{}", err(&o));
    assert!(!e.marker());
}

#[test]
fn a_row_with_turn_fields_twice_or_without_provider_is_a_corrupt_line() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    let tails = [
        ",\"provider\":\"zai\",\"turn_reason\":\"a\",\"turn_reason\":\"b\"",
        ",\"provider\":\"zai\",\"turn_next\":\"zai\",\"turn_next\":\"claude\"",
        ",\"turn_next\":\"zai\"",
        ",\"turn_reason\":\"a\"",
        ",\"provider\":\"zai\",\"turn_next\":\"Zai\"",
        ",\"provider\":\"zai\",\"turn_next\":5",
        ",\"provider\":\"zai\",\"turn_reason\":5",
    ];
    for t in tails {
        let mut r = Rows::default();
        let run = r.raw(&exec_r1(), "exec", 1000, t);
        r.bind(&run);
        r.save(&f);
        let o = e.next(&f, "zai,claude");
        assert_eq!(code(&o), 3, "{t}: {}", err(&o));
        assert!(err(&o).contains("line 1"), "{t}: {}", err(&o));
        let o = e.launches(&f);
        assert_eq!(code(&o), 2, "{t}: {}", err(&o));
        assert!(out(&o).is_empty());
    }
}

#[test]
fn a_torn_last_line_refuses_a_turn_launch() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    history(&f, Some("zai"));
    let mut s = text(&f);
    s.push_str("{\"v\":1,\"kind\":\"launch\"");
    fs::write(&f, &s).unwrap();
    let before = fs::read(&f).unwrap();
    let o = e.launch(&["--records", rec(&f), "--provider", "claude", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(fs::read(&f).unwrap(), before);
    assert!(!e.marker());
}

#[test]
fn a_corrupt_line_refuses_a_turn_launch() {
    let e = Env::new();
    let f = e.p("r.jsonl");
    fs::write(&f, "garbage\n").unwrap();
    let before = fs::read(&f).unwrap();
    let o = e.launch(&["--records", rec(&f), "--provider", "claude", "--turn", "claude,zai"]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(fs::read(&f).unwrap(), before);
    assert!(!e.marker());
}
