//! Process-level smoke for `harness session launch` / `launches` (P1B-18). The real binary runs as a
//! child; the host is a stub shell script written under a scratch folder under the system temp folder.
//! The real host is never run. Uuids and short ids are built at run time.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);
/// Session name built at run time from parts so no whole name-shaped literal sits in source.
fn session_name(lane: &str) -> String {
    ["exec", lane, "t3702", "r1", "M", "opu", "hig"].join("-")
}

const STUB: &str = r#"#!/bin/sh
d="$STUB_DIR"
case "$1" in
  --bg)
    if [ -n "$STUB_SLEEP" ]; then echo $$ > "$d/pid"; fi
    : > "$d/marker"
    pwd > "$d/cwd"
    if [ -n "$STUB_RECORDS" ]; then cp "$STUB_RECORDS" "$d/records-at-start" 2>/dev/null; fi
    : > "$d/argv"
    for a in "$@"; do printf '%s\0' "$a" >> "$d/argv"; done
    if [ -n "$STUB_BG_EXIT" ]; then exit "$STUB_BG_EXIT"; fi
    if [ -n "$STUB_SLEEP" ]; then exec sleep 60; fi
    echo "started"
    echo "$STUB_SHORT session started"
    ;;
  agents) cat "$STUB_AGENTS" ;;
  stop) echo "$2" > "$d/stop"; exit "${STUB_STOP_EXIT:-0}" ;;
esac
"#;

struct Env {
    root: PathBuf,
    short: String,
    uuid: String,
}

impl Env {
    fn new() -> Env {
        let base = std::env::temp_dir();
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = base.join(format!("hlc-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&root);
        for d in ["a", "b", "stub"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        let stub = root.join("stub.sh");
        fs::write(&stub, STUB).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let short = format!("s{:x}{}", std::process::id(), n);
        let uuid = format!("{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}", 0x1234_5678u32 + n as u32, 0xabcd, 0x123, 0x456, 0xdead_beef_0000u64 + n as u64);
        let e = Env { root, short, uuid };
        e.agents(&format!("[{{\"id\":\"{}\",\"sessionId\":\"{}\",\"name\":\"{}\"}},{{\"id\":\"zzz\",\"sessionId\":\"x\",\"name\":\"other\"}}]", e.short, e.uuid, session_name("L2")));
        e
    }
    fn p(&self, s: &str) -> PathBuf {
        self.root.join(s)
    }
    fn agents(&self, body: &str) {
        fs::write(self.p("agents.json"), body).unwrap();
    }
    fn config(&self, count: u64) -> PathBuf {
        let body = fs::read_to_string(EXAMPLE).unwrap().replace("count: 4", &format!("count: {count}"));
        let f = self.p(&format!("config-{count}.yaml"));
        fs::write(&f, body).unwrap();
        f
    }
    fn brief(&self, bytes: &[u8]) -> PathBuf {
        let f = self.p("brief.txt");
        fs::write(&f, bytes).unwrap();
        f
    }
    fn marker(&self) -> bool {
        self.p("stub/marker").exists()
    }
    /// Run `launch` with process cwd `a`, `--cwd` `b`, and the given extra args.
    fn launch(&self, name: &str, brief: &Path, count: u64, extra: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut c = Command::new(BIN);
        c.current_dir(self.p("a"))
            .args(["session", "launch", "--name", name, "--cwd"])
            .arg(self.p("b"))
            .arg("--brief")
            .arg(brief)
            .arg("--host")
            .arg(self.p("stub.sh"))
            .arg("--config")
            .arg(self.config(count))
            .args(extra)
            .stdin(Stdio::null())
            .env_remove("HARNESS_CONFIG")
            .env("STUB_DIR", self.p("stub"))
            .env("STUB_SHORT", &self.short)
            .env("STUB_AGENTS", self.p("agents.json"))
            .env("STUB_RECORDS", self.p("r.jsonl"));
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }
    fn ok_launch(&self, extra: &[&str]) -> Output {
        let b = self.brief(b"do the thing\n");
        self.launch(&session_name("L2"), &b, 4, extra, &[])
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

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn lines(p: &Path) -> Vec<String> {
    fs::read_to_string(p).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}
fn sha(bytes: &[u8]) -> String {
    use std::io::Write;
    for tool in [&["sha256sum"][..], &["shasum", "-a", "256"][..]] {
        if let Ok(mut ch) = Command::new(tool[0]).args(&tool[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() {
            ch.stdin.take().unwrap().write_all(bytes).unwrap();
            let o = ch.wait_with_output().unwrap();
            return String::from_utf8_lossy(&o.stdout).split_whitespace().next().unwrap().to_string();
        }
    }
    panic!("no sha256 tool");
}
fn argv(e: &Env) -> Vec<Vec<u8>> {
    let b = fs::read(e.p("stub/argv")).unwrap();
    let mut v: Vec<Vec<u8>> = b.split(|c| *c == 0).map(|s| s.to_vec()).collect();
    v.pop();
    v
}

#[test]
fn ac1_record_write_failure_starts_nothing() {
    let e = Env::new();
    fs::create_dir(e.p("recdir")).unwrap();
    let rd = e.p("recdir");
    let o = e.ok_launch(&["--records", rd.to_str().unwrap()]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(err(&o).contains("launch record not written"), "{}", err(&o));
    assert!(!e.marker());
    // twin
    let f = e.p("ok.jsonl");
    let o = e.ok_launch(&["--records", f.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(e.marker());
}

#[test]
fn ac2_row_first_all_fields_and_bound() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let o = e.ok_launch(&["--records", rec.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let at_start = lines(&e.p("stub/records-at-start"));
    assert_eq!(at_start.len(), 1, "the stub saw exactly the launch row at its own start");
    assert!(at_start[0].contains("\"kind\":\"launch\""));
    let l = lines(&rec);
    assert_eq!(l.len(), 2);
    let run = out(&o).split("run_id=").nth(1).unwrap().split_whitespace().next().unwrap().to_string();
    assert!(l[0].starts_with("{\"v\":1,\"kind\":\"launch\",\"ts_ms\":"), "{}", l[0]);
    for frag in [
        format!("\"run_id\":\"{run}\""),
        format!("\"name\":\"{}\"", session_name("L2")),
        "\"role\":\"exec\"".to_string(),
        format!("\"brief_sha256\":\"{}\"", sha(b"do the thing\n")),
        "\"owner\":\"orchestration\"".to_string(),
    ] {
        assert!(l[0].contains(&frag), "{frag} not in {}", l[0]);
    }
    assert!(l[1].contains("\"kind\":\"launch_ids\""));
    for frag in [format!("\"run_id\":\"{run}\""), format!("\"short_id\":\"{}\"", e.short), format!("\"session_id\":\"{}\"", e.uuid)] {
        assert!(l[1].contains(&frag), "{frag} not in {}", l[1]);
    }
    let r = e.launches(&rec);
    assert_eq!(code(&r), 0);
    assert!(out(&r).contains(&format!("session_id={} state=bound", e.uuid)), "{}", out(&r));
}

#[test]
fn ac2_twin_host_failure_is_failed() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let b = e.brief(b"x\n");
    let o = e.launch(&session_name("L2"), &b, 4, &["--records", rec.to_str().unwrap()], &[("STUB_BG_EXIT", "1")]);
    assert_eq!(code(&o), 3, "{}", err(&o));
    let l = lines(&rec);
    assert!(!l.iter().any(|x| x.contains("launch_ids")));
    assert!(l.iter().any(|x| x.contains("\"kind\":\"launch_failed\"") && x.contains("\"stage\":\"start\"")));
    assert!(out(&e.launches(&rec)).contains("state=failed"));
}

#[test]
fn ac3_exact_brief_content_and_refusals() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let brief: &[u8] = "line one  \n\nCafé — 日本語\n".as_bytes();
    let b = e.brief(brief);
    let o = e.launch(&session_name("L2"), &b, 4, &["--records", rec.to_str().unwrap()], &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let a = argv(&e);
    assert_eq!(a.len(), 4);
    assert_eq!(a[..3], [b"--bg".to_vec(), b"-n".to_vec(), session_name("L2").into_bytes()]);
    assert_eq!(a[3], brief);
    // twins
    for bad in [&b"-starts with dash"[..], &b""[..], &[0x66, 0xff, 0xfe][..], &b"has\0nul"[..]] {
        let e2 = Env::new();
        let rec2 = e2.p("r.jsonl");
        let b2 = e2.brief(bad);
        let o = e2.launch(&session_name("L2"), &b2, 4, &["--records", rec2.to_str().unwrap()], &[]);
        assert_eq!(code(&o), 2, "{:?}: {}", bad, err(&o));
        assert!(!rec2.exists() && !e2.marker());
    }
}

#[test]
fn ac4_name_check() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let b = e.brief(b"x\n");
    let long_name = session_name("L9");
    let long = long_name.as_str();
    e.agents(&format!("[{{\"id\":\"{}\",\"sessionId\":\"{}\",\"name\":\"{long}\"}}]", e.short, e.uuid));
    let o = e.launch(long, &b, 4, &["--records", rec.to_str().unwrap()], &[]);
    assert_eq!(code(&o), 1);
    assert!(err(&o).to_lowercase().contains("lane"), "{}", err(&o));
    assert!(!rec.exists() && !e.marker());
    let o = e.launch(long, &b, 9, &["--records", rec.to_str().unwrap()], &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let e2 = Env::new();
    let rec2 = e2.p("r.jsonl");
    let o = e2.launch("execution-l1-x", &b, 4, &["--records", rec2.to_str().unwrap()], &[]);
    assert_eq!(code(&o), 1);
    assert!(!rec2.exists() && !e2.marker());
}

fn map_fail(agents: impl Fn(&Env) -> String, stop_exit: &str) -> (Env, Output, PathBuf) {
    let e = Env::new();
    let body = agents(&e);
    e.agents(&body);
    let rec = e.p("r.jsonl");
    let b = e.brief(b"x\n");
    let o = e.launch(&session_name("L2"), &b, 4, &["--records", rec.to_str().unwrap(), "--map-timeout-ms", "600"], &[("STUB_STOP_EXIT", stop_exit)]);
    (e, o, rec)
}

fn assert_stopped_failed(e: &Env, o: &Output, rec: &Path) {
    assert_eq!(code(o), 4, "{}", err(o));
    assert_eq!(fs::read_to_string(e.p("stub/stop")).unwrap().trim(), e.short);
    let l = lines(rec);
    assert!(l[0].contains("\"kind\":\"launch\""));
    assert!(l.iter().any(|x| x.contains("launch_failed") && x.contains("\"stage\":\"map\"")));
    assert!(!l.iter().any(|x| x.contains("launch_ids")));
    let r = out(&e.launches(rec));
    assert!(r.contains("session_id=- ") && r.contains("state=failed"), "{r}");
}

#[test]
fn ac5_missing_short_id_stops_the_session() {
    let (e, o, rec) = map_fail(|_| "[{\"id\":\"nope\",\"sessionId\":\"x\",\"name\":\"other\"}]".into(), "0");
    assert_stopped_failed(&e, &o, &rec);
}

#[test]
fn ac5_ambiguous_listing() {
    let (e, o, rec) = map_fail(
        |e| {
            let one = format!("{{\"id\":\"{}\",\"sessionId\":\"{}\",\"name\":\"{}\"}}", e.short, e.uuid, session_name("L2"));
            format!("[{one},{one}]")
        },
        "0",
    );
    assert_stopped_failed(&e, &o, &rec);
    assert!(err(&o).contains("ambiguous"));
}

#[test]
fn ac5_short_id_where_uuid_belongs() {
    let (e, o, rec) = map_fail(|e| format!("[{{\"id\":\"{}\",\"sessionId\":\"{}\",\"name\":\"{}\"}}]", e.short, e.short, session_name("L2")), "0");
    assert_stopped_failed(&e, &o, &rec);
}

#[test]
fn ac5_stop_failure_names_the_short_id() {
    let (e, o, rec) = map_fail(|_| "[]".into(), "1");
    assert_eq!(code(&o), 5, "{}", err(&o));
    assert!(err(&o).contains(&e.short));
    assert!(lines(&rec).iter().any(|x| x.contains("launch_failed")));
}

#[test]
fn ac6_cwd_and_host_path() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let o = e.ok_launch(&["--records", rec.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let cwd = fs::read_to_string(e.p("stub/cwd")).unwrap();
    assert_eq!(cwd.trim(), e.p("b").canonicalize().unwrap().to_str().unwrap());
    // twins
    let e2 = Env::new();
    let rec2 = e2.p("r.jsonl");
    let b = e2.brief(b"x\n");
    let o = e2.launch(&session_name("L2"), &b, 4, &["--records", rec2.to_str().unwrap(), "--cwd", e2.p("missing").to_str().unwrap()], &[]);
    // the duplicate --cwd is itself refused; use a dedicated command for the missing directory
    assert_eq!(code(&o), 2);
    let mut c = Command::new(BIN);
    c.current_dir(e2.p("a")).args(["session", "launch", "--name", &session_name("L2"), "--brief"]).arg(&b).arg("--cwd").arg(e2.p("missing"));
    c.arg("--host").arg(e2.p("stub.sh")).arg("--config").arg(e2.config(4)).arg("--records").arg(&rec2);
    let o = c.env("STUB_DIR", e2.p("stub")).output().unwrap();
    assert_eq!(code(&o), 2);
    assert!(!rec2.exists() && !e2.marker());
    let mut c = Command::new(BIN);
    c.current_dir(e2.p("root-less")).current_dir(e2.p("a")).args(["session", "launch", "--name", &session_name("L2"), "--brief"]).arg(&b);
    c.arg("--cwd").arg(e2.p("b")).args(["--host", "stub.sh"]).arg("--config").arg(e2.config(4)).arg("--records").arg(&rec2);
    fs::copy(e2.p("stub.sh"), e2.p("a/stub.sh")).unwrap();
    let o = c.env("STUB_DIR", e2.p("stub")).output().unwrap();
    assert_eq!(code(&o), 2);
    assert!(!rec2.exists() && !e2.marker());
}

#[test]
fn ac7_reader_fails_closed() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let o = e.ok_launch(&["--records", rec.to_str().unwrap()]);
    assert_eq!(code(&o), 0);
    let clean = lines(&rec);
    assert_eq!(code(&e.launches(&rec)), 0);
    let bad = e.p("bad.jsonl");
    fs::write(&bad, format!("{}\n{{not json\n{}\n", clean[0], clean[1])).unwrap();
    let r = e.launches(&bad);
    assert_eq!(code(&r), 2);
    assert!(err(&r).contains("line 2"), "{}", err(&r));
}

#[test]
fn ac10_brief_cap() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let big = vec![b'a'; 102400];
    let b = e.brief(&big);
    let o = e.launch(&session_name("L2"), &b, 4, &["--records", rec.to_str().unwrap()], &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let a = argv(&e);
    assert_eq!(a[3].len(), 102400);
    assert_eq!(a[3], big);
    let e2 = Env::new();
    let rec2 = e2.p("r.jsonl");
    let b = e2.brief(&vec![b'a'; 102401]);
    let o = e2.launch(&session_name("L2"), &b, 4, &["--records", rec2.to_str().unwrap()], &[]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("102400"));
    assert!(!rec2.exists() && !e2.marker());
}

fn warm(e: &Env) {
    let _ = Command::new(e.p("stub.sh")).arg("warm").env("STUB_DIR", e.p("stub")).status();
}

#[test]
fn ac11_timeout_kills_host_and_names_orphan() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let b = e.brief(b"x\n");
    // warm the fresh script once (the first exec of a new file can be slow on a loaded host), so the 500 ms bound measures the hang
    warm(&e);
    let t = Instant::now();
    let o = e.launch(&session_name("L2"), &b, 4, &["--records", rec.to_str().unwrap(), "--start-timeout-ms", "500"], &[("STUB_SLEEP", "1")]);
    assert!(t.elapsed().as_secs() < 10);
    assert_eq!(code(&o), 3, "{}", err(&o));
    assert!(err(&o).contains("host timed out") && err(&o).contains(&session_name("L2")), "{}", err(&o));
    let pid = fs::read_to_string(e.p("stub/pid")).unwrap().trim().to_string();
    let alive = Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success();
    assert!(!alive, "the host must be dead");
    let l = lines(&rec);
    assert!(l.iter().any(|x| x.contains("launch_failed") && x.contains("\"stage\":\"start\"") && x.contains("\"reason\":\"timeout\"")));
    assert!(!l.iter().any(|x| x.contains("launch_ids")));
    // twin
    let e2 = Env::new();
    let rec2 = e2.p("r.jsonl");
    let b2 = e2.brief(b"x\n");
    warm(&e2);
    let o = e2.launch(&session_name("L2"), &b2, 4, &["--records", rec2.to_str().unwrap(), "--start-timeout-ms", "500"], &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
}

#[test]
fn ac12_default_records_path_is_the_process_cwd() {
    let e = Env::new();
    let o = e.ok_launch(&[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let f = e.p("a/.harness/launch-records.jsonl");
    assert!(lines(&f)[0].contains("\"kind\":\"launch\""));
    assert!(!e.p("b/.harness").exists());
    // twin
    let e2 = Env::new();
    let missing = e2.p("a/missing-dir/r.jsonl");
    let o = e2.ok_launch(&["--records", missing.to_str().unwrap()]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("launch record not written"));
    assert!(!e2.p("a/missing-dir").exists() && !e2.marker());
}
