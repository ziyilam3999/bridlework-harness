//! Process-level smoke for `harness session sign-off-check` (P1B-20, S-1.5 part 5). The real binary runs as a
//! child. Launch records, manifests and logs are synthesized under the test's own temp folder with run-time
//! uuids and names; the one marked-row case gets its row from a real launch through a stub host script.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);
const T: &str = "3704";
const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn name(role: &str, ticket: &str, round: u32) -> String {
    [role, "L2", &format!("t{ticket}"), &format!("r{round}"), "M", "opu", "hig"].join("-")
}
fn uuid(n: u64) -> String {
    format!("{:08x}-0000-4000-8000-{:012x}", 0x1000_0000 + n, n)
}

struct Env {
    root: PathBuf,
    cwd: PathBuf,
    rows: Vec<String>,
}

impl Env {
    fn new() -> Env {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("hso-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("cwd")).unwrap();
        fs::create_dir_all(root.join("mf")).unwrap();
        let root = root.canonicalize().unwrap();
        let cwd = root.join("cwd");
        Env { root, cwd, rows: Vec::new() }
    }
    fn p(&self, s: &str) -> PathBuf {
        self.root.join(s)
    }
    /// A bound launch: a `launch` row then a `launch_ids` row.
    fn bound(&mut self, run: &str, nm: &str, role: &str, short: &str, sid: &str) {
        self.pending(run, nm, role);
        self.rows.push(format!("{{\"v\":1,\"kind\":\"launch_ids\",\"ts_ms\":2,\"run_id\":\"{run}\",\"short_id\":\"{short}\",\"session_id\":\"{sid}\"}}"));
    }
    fn pending(&mut self, run: &str, nm: &str, role: &str) {
        self.rows.push(format!(
            "{{\"v\":1,\"kind\":\"launch\",\"ts_ms\":1,\"run_id\":\"{run}\",\"name\":\"{nm}\",\"role\":\"{role}\",\"brief_sha256\":\"{SHA}\",\"owner\":\"orchestration\"}}"
        ));
    }
    fn write_records(&self) -> PathBuf {
        let f = self.p("records.jsonl");
        let mut s = self.rows.join("\n");
        s.push('\n');
        fs::write(&f, s).unwrap();
        f
    }
    fn manifest(&self, ticket: &str, worker: Option<&str>) {
        let mut s = format!("{{\"v\":1,\"op\":\"root\",\"ticket\":\"{ticket}\",\"path\":\"{}\",\"ts_ms\":1}}\n", self.root.display());
        if let Some(w) = worker {
            s.push_str(&format!("{{\"v\":1,\"op\":\"add\",\"ticket\":\"{ticket}\",\"id\":\"w\",\"class\":\"process\",\"pid\":4242,\"pid_start_ms\":1,\"ts_ms\":2}}\n"));
            s.push_str(&format!("{{\"v\":1,\"op\":\"session\",\"ticket\":\"{ticket}\",\"id\":\"w\",\"session_id\":\"{w}\",\"ts_ms\":3}}\n"));
        }
        fs::write(self.p("mf").join(format!("{ticket}.jsonl")), s).unwrap();
    }
    /// A hook row, written by the real `record-start` writer.
    fn hook_row(&self, log: &Path, sid: &str) {
        let o = Command::new(BIN).args(["session", "record-start", "--session-id", sid, "--log"]).arg(log).output().unwrap();
        assert_eq!(code(&o), 0, "{}", err(&o));
    }
    fn check(&self, rec: &Path, who: (&str, &str, &str, &str), extra: &[&str], env: Option<&str>) -> Output {
        let (ticket, role, sid, claim) = who;
        let mut c = Command::new(BIN);
        c.current_dir(&self.cwd)
            .args(["session", "sign-off-check", "--ticket", ticket, "--role", role, "--session", sid, "--claim", claim, "--records"])
            .arg(rec)
            .arg("--manifest-dir")
            .arg(self.p("mf"))
            .args(extra)
            .stdin(Stdio::null())
            .env_remove("HARNESS_EVENT_LOG");
        if let Some(v) = env {
            c.env("HARNESS_EVENT_LOG", v);
        }
        c.output().unwrap()
    }
    fn plain(&self, rec: &Path, role: &str, sid: &str, claim: &str, log: &Path) -> Output {
        self.check(rec, (T, role, sid, claim), &["--log", log.to_str().unwrap()], None)
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

/// AC7: exactly one stdout line of the refusal shape, nothing on stderr, no foreign uuid in it.
fn refused(o: &Output, reason: &str, foreign: &[&str]) {
    assert_eq!(code(o), 1, "{} / {}", out(o), err(o));
    let s = out(o);
    assert_eq!(s.lines().count(), 1, "{s}");
    assert!(s.starts_with("sign-off=refused ticket="), "{s}");
    assert!(s.contains(&format!(" reason={reason}: ")), "{s}");
    let r = s.split(" reason=").nth(1).unwrap().split(':').next().unwrap();
    assert!(["build-id", "orchestration-id", "no-launch-record", "role-mismatch", "not-marked-sign-off"].contains(&r), "{s}");
    assert!(err(o).is_empty(), "{}", err(o));
    for f in foreign {
        assert!(!s.contains(f), "{s}");
    }
}

/// The base records: `exec` for T bound to A, `gov` for T bound to B.
fn base(e: &mut Env) -> (String, String) {
    let (a, b) = (uuid(1), uuid(2));
    e.bound("ra", &name("exec", T, 1), "exec", "sa", &a);
    e.bound("rb", &name("gov", T, 1), "gov", "sb", &b);
    (a, b)
}

#[test]
fn distinct_launched_signer_is_accepted() {
    let mut e = Env::new();
    let (_, b) = base(&mut e);
    let rec = e.write_records();
    let log = e.p("ev.jsonl");
    let o = e.plain(&rec, "gov", &b, "done", &log);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let s = out(&o);
    assert_eq!(s.lines().count(), 1);
    assert!(s.starts_with(&format!("sign-off=accepted ticket={T} role=gov claim=done")), "{s}");
    assert!(!log.exists());
}

#[test]
fn signer_uuid_equal_to_build_uuid_is_refused() {
    let mut e = Env::new();
    let (a, b) = base(&mut e);
    let w = uuid(3);
    let (h, h2) = (uuid(4), uuid(5));
    e.manifest(T, Some(&w));
    // a planted signer row that reuses the build uuid
    e.bound("rp", &name("gov", T, 2), "gov", "sp", &a);
    e.bound("rw", &name("gov", T, 3), "gov", "sw", &w);
    e.bound("rh", &name("gov", T, 4), "gov", "sh", &h);
    e.bound("rh2", &name("gov", T, 5), "gov", "sh2", &h2);
    let rec = e.write_records();
    let log = e.p("ev.jsonl");
    e.hook_row(&log, &h);
    refused(&e.plain(&rec, "gov", &a, "done", &log), "build-id", &[&w, &h]);
    refused(&e.plain(&rec, "gov", &w, "done", &log), "build-id", &[&a, &h]);
    let o = e.plain(&rec, "gov", &h, "done", &log);
    refused(&o, "orchestration-id", &[&a, &w]);
    assert_eq!(code(&e.plain(&rec, "gov", &b, "done", &log)), 0);

    // env-log arm: the hook row for H2 lives only in the env-named log; no --log; separate cwd
    let envlog = e.p("hook-events.jsonl");
    e.hook_row(&envlog, &h2);
    let ev = envlog.to_str().unwrap();
    refused(&e.check(&rec, (T, "gov", &h2, "done"), &[], Some(ev)), "orchestration-id", &[&a, &w]);
    assert!(!e.cwd.join(".harness").exists());
    // env twin: the same env does not refuse a distinct signer
    assert_eq!(code(&e.check(&rec, (T, "gov", &b, "done"), &[], Some(ev))), 0);
    // env refusal: a relative value is exit 2, nothing created
    let o = e.check(&rec, (T, "gov", &b, "done"), &[], Some("rel/events.jsonl"));
    assert_eq!(code(&o), 2, "{}", out(&o));
    assert!(!out(&o).contains("sign-off="));
    assert!(fs::read_dir(&e.cwd).unwrap().next().is_none());
    // --log beats the env: H2 has a bound gov row and the --log log has no hook row for it
    let other = e.p("other.jsonl");
    let o = e.check(&rec, (T, "gov", &h2, "done"), &["--log", other.to_str().unwrap()], Some(ev));
    assert_eq!(code(&o), 0, "{} {}", out(&o), err(&o));
}

#[test]
fn short_id_mapping_to_build_uuid_is_refused() {
    let mut e = Env::new();
    let (a, b) = base(&mut e);
    let rec = e.write_records();
    let log = e.p("ev.jsonl");
    refused(&e.plain(&rec, "gov", "sa", "done", &log), "build-id", &[&a]);
    assert_eq!(code(&e.plain(&rec, "gov", "sb", "done", &log)), 0);
    let o = e.plain(&rec, "gov", "nosuch", "done", &log);
    refused(&o, "no-launch-record", &[&a, &b]);
    assert!(!log.exists());
    // one short id bound to two uuids: exit 2
    e.bound("rx", &name("gov", T, 6), "gov", "dup", &uuid(7));
    e.bound("ry", &name("gov", T, 7), "gov", "dup", &uuid(8));
    let rec = e.write_records();
    let o = e.plain(&rec, "gov", "dup", "done", &log);
    assert_eq!(code(&o), 2);
    assert!(!out(&o).contains("sign-off="));
}

#[test]
fn unlaunched_signer_is_refused_and_logged() {
    let mut e = Env::new();
    let (_, b) = base(&mut e);
    let (u, f) = (uuid(20), uuid(21));
    // another ticket's gov launch bound to U
    e.bound("ro", &name("gov", "3999", 1), "gov", "so", &u);
    e.pending("rpend", &name("gov", T, 8), "gov");
    // launch_ids followed by launch_failed
    e.bound("rf", &name("gov", T, 9), "gov", "sf", &f);
    e.rows.push("{\"v\":1,\"kind\":\"launch_failed\",\"ts_ms\":3,\"run_id\":\"rf\",\"stage\":\"start\",\"reason\":\"x\"}".into());
    let rec = e.write_records();
    let sl = Command::new(BIN).args(["session", "launches", "--records"]).arg(&rec).output().unwrap();
    assert!(out(&sl).lines().any(|l| l.contains("short_id=sf") && l.ends_with("state=failed")), "{}", out(&sl));
    let log = e.p("ev.jsonl");
    let un = uuid(22);
    refused(&e.plain(&rec, "gov", &un, "done", &log), "no-launch-record", &[]);
    let l = lines(&log);
    assert_eq!(l.len(), 1, "{l:?}");
    assert!(l[0].contains("\"kind\":\"unmanaged_session\"") && l[0].contains(&format!("\"session_id\":\"{un}\"")), "{}", l[0]);
    assert!(l[0].contains(&format!("\"ticket\":\"{T}\"")) && l[0].contains("\"reason\":\"no_launch_record\""), "{}", l[0]);
    // another ticket's launch of U
    refused(&e.plain(&rec, "gov", &u, "done", &log), "no-launch-record", &[]);
    assert_eq!(lines(&log).len(), 2);
    // failed after bind
    refused(&e.plain(&rec, "gov", &f, "done", &log), "no-launch-record", &[]);
    let l = lines(&log);
    assert_eq!(l.len(), 3);
    assert!(l[2].contains(&format!("\"session_id\":\"{f}\"")));
    // env-log append: the row lands in the env file, the cwd gets no .harness
    let envlog = e.p("env-events.jsonl");
    let o = e.check(&rec, (T, "gov", &un, "done"), &[], Some(envlog.to_str().unwrap()));
    refused(&o, "no-launch-record", &[]);
    assert_eq!(lines(&envlog).len(), 1);
    assert!(!e.cwd.join(".harness").exists());
    // ticket token arm
    let raw = format!("t{T}");
    refused(&e.check(&rec, (&raw, "gov", &b, "done"), &["--log", log.to_str().unwrap()], None), "no-launch-record", &[]);
    assert_eq!(code(&e.plain(&rec, "gov", &b, "done", &log)), 0);
}

#[test]
fn mismatched_role_and_unreadable_inputs_refuse() {
    let mut e = Env::new();
    let (_, b) = base(&mut e);
    let rec = e.write_records();
    let log = e.p("ev.jsonl");
    refused(&e.plain(&rec, "e-rvw", &b, "verdict", &log), "role-mismatch", &[]);
    // corrupt records line
    let bad = e.p("bad.jsonl");
    fs::write(&bad, "not json\n").unwrap();
    let o = e.plain(&bad, "gov", &b, "done", &log);
    assert_eq!(code(&o), 2);
    assert!(!out(&o).contains("sign-off="));
    // missing records
    assert_eq!(code(&e.plain(&e.p("nope.jsonl"), "gov", &b, "done", &log)), 2);
    // damaged manifest
    fs::write(e.p("mf").join(format!("{T}.jsonl")), "garbage\n").unwrap();
    assert_eq!(code(&e.plain(&rec, "gov", &b, "done", &log)), 2);
    fs::remove_file(e.p("mf").join(format!("{T}.jsonl"))).unwrap();
    // torn log tail
    let torn = e.p("torn.jsonl");
    fs::write(&torn, "{\"v\":2").unwrap();
    assert_eq!(code(&e.plain(&rec, "gov", &b, "done", &torn)), 2);
    // missing manifest and log: accepted
    assert_eq!(code(&e.plain(&rec, "gov", &b, "done", &log)), 0);
    // bad role and claim
    assert_eq!(code(&e.plain(&rec, "exec", &b, "done", &log)), 2);
    assert_eq!(code(&e.plain(&rec, "gov", &b, "maybe", &log)), 2);
}

const STUB: &str = r#"#!/bin/sh
case "$1" in
  --bg) echo "started"; echo "$STUB_SHORT session started" ;;
  agents) cat "$STUB_AGENTS" ;;
  stop) : ;;
esac
"#;

#[test]
fn e_rvw_done_claim_needs_the_sign_off_mark() {
    let e = Env::new();
    let stub = e.p("stub.sh");
    fs::write(&stub, STUB).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(e.p("work")).unwrap();
    let (n1, n2) = (name("e-rvw", T, 1), name("e-rvw", T, 2));
    let (u1, u2) = (uuid(31), uuid(32));
    fs::write(
        e.p("agents.json"),
        format!(
            "[{{\"id\":\"s1\",\"sessionId\":\"{u1}\",\"name\":\"{n1}\"}},{{\"id\":\"s2\",\"sessionId\":\"{u2}\",\"name\":\"{n2}\"}}]"
        ),
    )
    .unwrap();
    let mut cfg = fs::read_to_string(EXAMPLE).unwrap();
    cfg.push_str(&format!("\nbuilder_state:\n  scratch: {}/never-named\n", e.root.display()));
    fs::write(e.p("config.yaml"), cfg).unwrap();
    fs::write(e.p("brief.txt"), "sign off the merged change from the PR text alone\n").unwrap();
    let rec = e.p("records.jsonl");
    let launch = |nm: &str, short: &str, extra: &[&str]| {
        Command::new(BIN)
            .args(["session", "launch", "--name", nm, "--brief"])
            .arg(e.p("brief.txt"))
            .arg("--cwd")
            .arg(e.p("work"))
            .arg("--host")
            .arg(&stub)
            .arg("--config")
            .arg(e.p("config.yaml"))
            .arg("--records")
            .arg(&rec)
            .args(extra)
            .stdin(Stdio::null())
            .env_remove("HARNESS_CONFIG")
            .env("STUB_SHORT", short)
            .env("STUB_AGENTS", e.p("agents.json"))
            .output()
            .unwrap()
    };
    let o = launch(&n1, "s1", &["--sign-off"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = launch(&n2, "s2", &[]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let l = lines(&rec);
    let launch_rows: Vec<&String> = l.iter().filter(|x| x.contains("\"kind\":\"launch\"")).collect();
    assert_eq!(launch_rows.len(), 2);
    assert!(launch_rows[0].contains("\"sign_off\":true"), "{}", launch_rows[0]);
    assert!(!launch_rows[1].contains("sign_off"), "{}", launch_rows[1]);
    let sl = Command::new(BIN).args(["session", "launches", "--records"]).arg(&rec).output().unwrap();
    let ls = out(&sl);
    let (m, u): (Vec<&str>, Vec<&str>) = ls.lines().partition(|x| x.contains("short_id=s1"));
    assert!(m[0].ends_with(" sign_off=yes"), "{ls}");
    assert!(!u[0].contains("sign_off="), "{ls}");
    let log = e.p("ev.jsonl");
    assert_eq!(code(&e.plain(&rec, "e-rvw", &u1, "done", &log)), 0);
    refused(&e.plain(&rec, "e-rvw", &u2, "done", &log), "not-marked-sign-off", &[]);
    assert_eq!(code(&e.plain(&rec, "e-rvw", &u2, "verdict", &log)), 0);
    // a planted `sign_off:false` is a corrupt line
    let bad = e.p("bad.jsonl");
    let mut s = fs::read_to_string(&rec).unwrap();
    s = s.replacen("\"owner\":\"orchestration\"", "\"owner\":\"orchestration\",\"sign_off\":false", 1);
    fs::write(&bad, s).unwrap();
    let o = Command::new(BIN).args(["session", "launches", "--records"]).arg(&bad).output().unwrap();
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("line 1"), "{}", err(&o));
}
