//! Process-level smoke for the signer refusal (P1B-19, S-1.5 part 4). The real binary runs as a child;
//! the host is a stub shell script under a scratch folder under the system temp folder. The real host is
//! never run. The builder-state entry text is a scratch path under the test's own root, built at run time.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);
const ENTRY: &str = "builder-worktree";

fn session_name(role: &str) -> String {
    [role, "L2", "t3703", "r1", "M", "opu", "hig"].join("-")
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
  stop) echo "$2" > "$d/stop" ;;
esac
"#;

struct Env {
    root: PathBuf,
    short: String,
    uuid: String,
    /// The planted entry text: a scratch path under this test's own root.
    text: String,
}

impl Env {
    fn new() -> Env {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("hls-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&root);
        for d in ["a", "b", "stub"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        let stub = root.join("stub.sh");
        fs::write(&stub, STUB).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        let short = format!("s{:x}{}", std::process::id(), n);
        let uuid = format!("{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}", 0x2234_5678u32 + n as u32, 0xabcd, 0x123, 0x456, 0xdead_beef_0000u64 + n as u64);
        let text = format!("{}/builder-scratch-{n}", root.display());
        let e = Env { root, short, uuid, text };
        let items: Vec<String> =
            ["gov", "e-rvw", "exec"].iter().map(|r| format!("{{\"id\":\"{}\",\"sessionId\":\"{}\",\"name\":\"{}\"}}", e.short, e.uuid, session_name(r))).collect();
        fs::write(e.p("agents.json"), format!("[{}]", items.join(","))).unwrap();
        e
    }
    fn p(&self, s: &str) -> PathBuf {
        self.root.join(s)
    }
    fn config(&self, with_list: bool) -> PathBuf {
        let mut body = fs::read_to_string(EXAMPLE).unwrap();
        if with_list {
            body.push_str(&format!("\nbuilder_state:\n  decoy: {}/never-named\n  {ENTRY}: {}\n", self.root.display(), self.text));
        }
        let f = self.p(if with_list { "config-bs.yaml" } else { "config-plain.yaml" });
        fs::write(&f, body).unwrap();
        f
    }
    fn brief_naming(&self) -> Vec<u8> {
        format!("review the work in {}/src and sign off\n", self.text).into_bytes()
    }
    fn brief_clean(&self) -> Vec<u8> {
        b"sign off the merged change from the PR text alone\n".to_vec()
    }
    fn marker(&self) -> bool {
        self.p("stub/marker").exists()
    }
    fn run(&self, cwd: &Path, name: &str, brief: &[u8], with_list: bool, extra: &[&str]) -> Output {
        let bf = self.p("brief.txt");
        fs::write(&bf, brief).unwrap();
        Command::new(BIN)
            .current_dir(cwd)
            .args(["session", "launch", "--name", name, "--cwd"])
            .arg(self.p("b"))
            .arg("--brief")
            .arg(&bf)
            .arg("--host")
            .arg(self.p("stub.sh"))
            .arg("--config")
            .arg(self.config(with_list))
            .args(extra)
            .stdin(Stdio::null())
            .env_remove("HARNESS_CONFIG")
            .env("STUB_DIR", self.p("stub"))
            .env("STUB_SHORT", &self.short)
            .env("STUB_AGENTS", self.p("agents.json"))
            .output()
            .unwrap()
    }
    fn launch(&self, name: &str, brief: &[u8], with_list: bool, rec: &Path, extra: &[&str]) -> Output {
        let mut x = vec!["--records", rec.to_str().unwrap()];
        x.extend_from_slice(extra);
        self.run(&self.p("a"), name, brief, with_list, &x)
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

/// The shared refusal assertions: exit 6, one `launch_refused` row, no `launch` row, nothing started, no text leak.
fn assert_refused(e: &Env, o: &Output, rec: &Path, role: &str, brief: &[u8]) {
    assert_eq!(code(o), 6, "{}", err(o));
    let l = lines(rec);
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(l[0].contains("\"kind\":\"launch_refused\""), "{}", l[0]);
    assert!(l[0].contains(&format!("\"matched_entry\":\"{ENTRY}\"")), "{}", l[0]);
    assert!(l[0].contains(&format!("\"role\":\"{role}\"")), "{}", l[0]);
    assert!(l[0].contains(&format!("\"brief_sha256\":\"{}\"", sha(brief))), "{}", l[0]);
    assert!(!l.iter().any(|x| x.contains("\"kind\":\"launch\"")));
    assert!(!e.marker(), "the stub must not have been started");
    // AC7: only the entry NAME appears, never its text
    assert!(!err(o).contains(&e.text) && !l[0].contains(&e.text));
    assert!(err(o).contains(ENTRY));
}

#[test]
fn signer_brief_naming_one_planted_entry_is_refused() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let b = e.brief_naming();
    let o = e.launch(&session_name("gov"), &b, true, &rec, &[]);
    assert_refused(&e, &o, &rec, "gov", &b);
}

#[test]
fn clean_signer_brief_launches() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let o = e.launch(&session_name("gov"), &e.brief_clean(), true, &rec, &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(e.marker());
    let l = lines(&rec);
    assert_eq!(l.len(), 2);
    assert!(l[0].contains("\"kind\":\"launch\"") && l[1].contains("\"kind\":\"launch_ids\""));
    assert!(out(&e.launches(&rec)).contains("state=bound"));
}

#[test]
fn non_signer_role_is_not_checked() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let o = e.launch(&session_name("exec"), &e.brief_naming(), true, &rec, &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(e.marker());
}

#[test]
fn signer_launch_without_builder_state_list_is_refused() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let o = e.launch(&session_name("gov"), &e.brief_naming(), false, &rec, &[]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(err(&o).contains("builder_state"));
    assert!(lines(&rec).is_empty());
    assert!(!e.marker());
}

#[test]
fn launches_lists_refused_state() {
    let e = Env::new();
    let rec = e.p("r.jsonl");
    let o = e.launch(&session_name("gov"), &e.brief_naming(), true, &rec, &[]);
    assert_eq!(code(&o), 6, "{}", err(&o));
    let row = lines(&rec).remove(0);
    let run = row.split("\"run_id\":\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    let r = e.launches(&rec);
    assert_eq!(code(&r), 0, "{}", String::from_utf8_lossy(&r.stderr));
    assert!(out(&r).contains("state=refused") && out(&r).contains(&format!("run_id={run}")), "{}", out(&r));
    // a launch_ids row after the refusal for the same run_id is corrupt
    let mut body = fs::read_to_string(&rec).unwrap();
    body.push_str(&format!("{{\"v\":1,\"kind\":\"launch_ids\",\"ts_ms\":1,\"run_id\":\"{run}\",\"short_id\":\"{}\",\"session_id\":\"{}\"}}\n", e.short, e.uuid));
    let bad = e.p("bad.jsonl");
    fs::write(&bad, body).unwrap();
    let r = e.launches(&bad);
    assert_eq!(code(&r), 2);
    assert!(String::from_utf8_lossy(&r.stderr).contains("line 2"), "{}", String::from_utf8_lossy(&r.stderr));
    // a second refusal row with the same run_id is corrupt too
    let two = e.p("two.jsonl");
    fs::write(&two, format!("{row}\n{row}\n")).unwrap();
    assert_eq!(code(&e.launches(&two)), 2);
}

#[test]
fn refusal_without_records_flag_writes_default_records() {
    let e = Env::new();
    let cwd = e.p("fresh");
    fs::create_dir_all(&cwd).unwrap();
    assert!(!cwd.join(".harness").exists());
    let b = e.brief_naming();
    let o = e.run(&cwd, &session_name("gov"), &b, true, &[]);
    assert_eq!(code(&o), 6, "{}", err(&o));
    let l = lines(&cwd.join(".harness/launch-records.jsonl"));
    assert_eq!(l.len(), 1);
    assert!(l[0].contains("\"kind\":\"launch_refused\""));
    assert!(!e.marker());
}

#[test]
fn sign_off_flag_scopes_the_e_rvw_check() {
    let e = Env::new();
    let b = e.brief_naming();
    // e-rvw WITH --sign-off: refused
    let rec = e.p("r1.jsonl");
    let o = e.launch(&session_name("e-rvw"), &b, true, &rec, &["--sign-off"]);
    assert_refused(&e, &o, &rec, "e-rvw", &b);
    // e-rvw WITHOUT: a routine review launches
    let rec = e.p("r2.jsonl");
    let o = e.launch(&session_name("e-rvw"), &b, true, &rec, &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(e.marker());
    let l = lines(&rec);
    assert!(l.len() == 2 && l[0].contains("\"kind\":\"launch\"") && l[1].contains("launch_ids"));
    let _ = fs::remove_file(e.p("stub/marker"));
    // exec WITH --sign-off: usage error, nothing written
    let rec = e.p("r3.jsonl");
    let o = e.launch(&session_name("exec"), &b, true, &rec, &["--sign-off"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("--sign-off"));
    assert!(lines(&rec).is_empty() && !e.marker());
    // given twice
    let rec = e.p("r4.jsonl");
    let o = e.launch(&session_name("gov"), &b, true, &rec, &["--sign-off", "--sign-off"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("given twice") && !e.marker());
}
