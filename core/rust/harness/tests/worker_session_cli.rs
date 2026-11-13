//! Worker session provenance (#3706): `process start --worker`, `process claim`, `process sessions`. Every
//! check drives the real binary, spawns real `/bin/sh -c` programs and reads the real manifest and log.
//! Waits are bounded polls on observable state; each worker a test starts is stopped by a drop guard.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);
const T: &str = "t-1";
const PLANTED: &str = "11111111-2222-4333-8444-555555555555";
const OTHER: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

struct Fx {
    base: PathBuf,
    r: PathBuf,
    d: PathBuf,
    l: PathBuf,
    ids: Vec<String>,
}

impl Fx {
    fn new() -> Fx {
        let base = std::env::temp_dir().join(format!(
            "wscli-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&base);
        let (r, d, l) = (base.join("R"), base.join("D"), base.join("L"));
        for p in [&r, &d, &l] {
            fs::create_dir_all(p).unwrap();
        }
        let base = base.canonicalize().unwrap();
        let (r, d, l) = (base.join("R"), base.join("D"), base.join("L"));
        let o = run(&["manifest", "init", T, "--dir", s(&d), "--root", s(&r)]);
        assert_eq!(code(&o), 0, "init: {}", err(&o));
        Fx {
            base,
            r,
            d,
            l,
            ids: vec![],
        }
    }
    fn log(&self) -> PathBuf {
        self.l.join("ev.jsonl")
    }
    fn start(&mut self, id: &str, worker: bool, prog: &str) -> Output {
        let log = self.log();
        let mut c = Command::new(BIN);
        c.args([
            "process",
            "start",
            T,
            "--id",
            id,
            "--cwd",
            s(&self.r),
            "--dir",
            s(&self.d),
        ]);
        if worker {
            c.args(["--worker", "--log", s(&log)]);
        }
        c.args(["--", "/bin/sh", "-c", prog])
            .env("BIN", BIN)
            .stdin(Stdio::null());
        let o = c.output().unwrap();
        if code(&o) == 0 {
            self.ids.push(id.to_string());
        }
        o
    }
    fn sessions(&self) -> String {
        let o = run(&["process", "sessions", T, "--dir", s(&self.d)]);
        assert_eq!(code(&o), 0, "{}", err(&o));
        out(&o)
    }
    fn claim(&self, sid: &str) -> Output {
        run(&[
            "process",
            "claim",
            T,
            "--session",
            sid,
            "--dir",
            s(&self.d),
            "--log",
            s(&self.log()),
        ])
    }
    fn log_text(&self) -> String {
        fs::read_to_string(self.log()).unwrap_or_default()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        for id in &self.ids {
            let _ = run(&[
                "process",
                "stop",
                T,
                "--id",
                id,
                "--grace-ms",
                "200",
                "--dir",
                s(&self.d),
            ]);
        }
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}
fn run(a: &[&str]) -> Output {
    Command::new(BIN)
        .args(a)
        .stdin(Stdio::null())
        .output()
        .unwrap()
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
fn wait_for(f: impl Fn() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}
/// Wait for a non-empty file and return its trimmed text.
fn read_file(p: &Path) -> String {
    assert!(
        wait_for(|| fs::read_to_string(p)
            .map(|t| t.ends_with('\n'))
            .unwrap_or(false)),
        "{} never appeared",
        p.display()
    );
    fs::read_to_string(p).unwrap().trim().to_string()
}
fn worker_uuid(o: &Output) -> String {
    let line = out(o)
        .lines()
        .find(|l| l.starts_with("worker "))
        .expect("a worker line")
        .to_string();
    line.split_whitespace().nth(3).unwrap().to_string()
}
fn lines_of(text: &str, kind: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.contains(&format!("\"kind\":\"{kind}\"")))
        .map(|l| l.to_string())
        .collect()
}

#[test]
fn worker_start_records_session() {
    let mut f = Fx::new();
    let sid_file = f.r.join("sid");
    let prog = format!(
        "echo \"$HARNESS_WORKER_SESSION\" > {}; sleep 30",
        s(&sid_file)
    );
    let o = f.start("w", true, &prog);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let u = worker_uuid(&o);
    assert_eq!(read_file(&sid_file), u);
    assert_ne!(u, PLANTED);
    let manifest = fs::read_to_string(f.d.join(format!("{T}.jsonl"))).unwrap();
    let sess: Vec<&str> = manifest
        .lines()
        .filter(|l| l.contains("\"op\":\"session\""))
        .collect();
    assert_eq!(sess.len(), 1);
    assert!(
        sess[0].contains(&format!("\"session_id\":\"{u}\"")),
        "{}",
        sess[0]
    );
    let started = lines_of(&f.log_text(), "session_started");
    assert_eq!(started.len(), 1);
    assert!(
        started[0].contains(&u) && started[0].contains("\"source\":\"launch\""),
        "{}",
        started[0]
    );
    // a second worker gets its own id
    let o2 = f.start("w2", true, "sleep 30");
    assert_eq!(code(&o2), 0, "{}", err(&o2));
    assert_ne!(worker_uuid(&o2), u);

    // negative end: without --worker nothing is written
    let mut g = Fx::new();
    let o = g.start("n", false, "sleep 30");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!out(&o).contains("worker "));
    let m = fs::read_to_string(g.d.join(format!("{T}.jsonl"))).unwrap();
    assert!(!m.contains("\"op\":\"session\""));
    assert!(!g.log().exists());
    // --log without --worker: usage error, manifest byte-identical, nothing spawned
    let before = fs::read(g.d.join(format!("{T}.jsonl"))).unwrap();
    let o = run(&[
        "process",
        "start",
        T,
        "--id",
        "x",
        "--cwd",
        s(&g.r),
        "--dir",
        s(&g.d),
        "--log",
        s(&g.log()),
        "--",
        "/bin/sh",
        "-c",
        "sleep 30",
    ]);
    assert_eq!(code(&o), 2);
    assert_eq!(fs::read(g.d.join(format!("{T}.jsonl"))).unwrap(), before);
}

#[test]
fn worker_start_event_failure_rolls_back() {
    let mut f = Fx::new();
    let outside = f.base.join("O");
    fs::create_dir_all(&outside).unwrap();
    let marker = f.r.join("ran");
    let prog = format!("echo $$ > {}; sleep 30", s(&marker));
    let o = Command::new(BIN)
        .args([
            "process",
            "start",
            T,
            "--id",
            "w",
            "--cwd",
            s(&f.r),
            "--dir",
            s(&f.d),
            "--worker",
            "--log",
            s(&outside.join("ev.jsonl")),
            "--root",
            s(&f.l),
            "--",
            "/bin/sh",
            "-c",
            &prog,
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(code(&o), 2, "{}", out(&o));
    assert!(
        err(&o).contains("session_started not recorded"),
        "{}",
        err(&o)
    );
    assert!(out(&f.sessions_o()).contains("counted 0"));
    let ls = run(&["manifest", "list", T, "--dir", s(&f.d)]);
    assert!(
        !out(&ls)
            .lines()
            .any(|l| l.split_whitespace().next() == Some("w")),
        "{}",
        out(&ls)
    );
    if marker.exists() {
        let pid: u32 = read_file(&marker).parse().unwrap();
        assert!(
            wait_for(|| !Command::new("kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()),
            "spawned group survived"
        );
    }
    assert!(!outside.join("ev.jsonl").exists());
    f.ids.clear();
}

impl Fx {
    fn sessions_o(&self) -> Output {
        let o = run(&["process", "sessions", T, "--dir", s(&self.d)]);
        assert_eq!(code(&o), 0, "{}", err(&o));
        o
    }
}

#[test]
fn claim_by_launched_worker_counts() {
    let mut f = Fx::new();
    let (rc, outf) = (f.r.join("rc"), f.r.join("out"));
    let prog = format!(
        "\"$BIN\" process claim {T} --session \"$HARNESS_WORKER_SESSION\" --dir {} --log {} > {}; echo $? > {}; sleep 30",
        s(&f.d), s(&f.log()), s(&outf), s(&rc)
    );
    let o = f.start("w", true, &prog);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let u = worker_uuid(&o);
    assert_eq!(read_file(&rc), "0");
    assert_eq!(read_file(&outf), format!("counted w {u}"));
    assert!(lines_of(&f.log_text(), "unmanaged_session").is_empty());
    assert_eq!(
        f.log_text().lines().count(),
        1,
        "the claim added a log line"
    );
}

#[test]
fn claim_execd_by_worker_shell_counts() {
    let mut f = Fx::new();
    let outf = f.r.join("out");
    let prog = format!("exec \"$BIN\" process claim {T} --session \"$HARNESS_WORKER_SESSION\" --dir {} --log {} > {}", s(&f.d), s(&f.log()), s(&outf));
    let o = f.start("w", true, &prog);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let u = worker_uuid(&o);
    assert_eq!(read_file(&outf), format!("counted w {u}"));
    assert!(lines_of(&f.log_text(), "unmanaged_session").is_empty());
}

#[test]
fn claim_with_wrong_id_refused() {
    let mut f = Fx::new();
    let rc = f.r.join("rc");
    let prog = format!("\"$BIN\" process claim {T} --session {OTHER} --dir {} --log {} >/dev/null 2>&1; echo $? > {}; sleep 30", s(&f.d), s(&f.log()), s(&rc));
    let o = f.start("w", true, &prog);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(read_file(&rc), "2");
    assert_eq!(f.log_text().lines().count(), 1);
    assert!(f.sessions().contains("counted 1"));
}

#[test]
fn claim_by_unlaunched_process_is_unmanaged() {
    let mut f = Fx::new();
    let o = f.start("w", true, "sleep 30");
    let u = worker_uuid(&o);
    let c = f.claim(&u);
    assert_eq!(code(&c), 7, "{}", err(&c));
    assert!(out(&c).contains("unmanaged: not counted"));
    let un = lines_of(&f.log_text(), "unmanaged_session");
    assert_eq!(un.len(), 1);
    assert!(
        un[0].contains(&u)
            && un[0].contains("\"ticket\":\"t-1\"")
            && un[0].contains("not_supervisor_launched"),
        "{}",
        un[0]
    );
    let v = run(&["events", "verify", "--log", s(&f.log())]);
    assert_eq!(code(&v), 0, "{}{}", out(&v), err(&v));
    assert!(f.sessions().contains("counted 1"));
    // a process started WITHOUT --worker claims: not a worker
    let rc = f.r.join("rc");
    let prog = format!("\"$BIN\" process claim {T} --session {OTHER} --dir {} --log {} >/dev/null 2>&1; echo $? > {}; sleep 30", s(&f.d), s(&f.log()), s(&rc));
    let o = f.start("plain", false, &prog);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(read_file(&rc), "7");
}

#[test]
fn stopped_worker_shows_closed() {
    let mut f = Fx::new();
    let a = worker_uuid(&f.start("w", true, "sleep 30"));
    let b = worker_uuid(&f.start("w2", true, "sleep 30"));
    let st = run(&[
        "process",
        "stop",
        T,
        "--id",
        "w",
        "--grace-ms",
        "500",
        "--dir",
        s(&f.d),
    ]);
    assert_eq!(code(&st), 0, "{}", err(&st));
    let t = f.sessions();
    assert!(t.contains(&format!("w {a} closed")), "{t}");
    assert!(t.contains(&format!("w2 {b} live")), "{t}");
    assert!(t.contains("counted 1"), "{t}");
    let st = run(&[
        "process",
        "stop",
        T,
        "--id",
        "w2",
        "--grace-ms",
        "500",
        "--dir",
        s(&f.d),
    ]);
    assert_eq!(code(&st), 0, "{}", err(&st));
    assert!(f.sessions().contains("counted 0"));
}
