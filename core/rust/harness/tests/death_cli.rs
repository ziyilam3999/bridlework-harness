//! Process-level tests for the worker death record (P1B-07, #210): a `timed_out` event plus a handover
//! file for every ticket process that ends without being asked to. Every check runs the real binary as a
//! child process, spawns REAL programs, and reads the real manifest, log and deaths folder, so the
//! record-mode contract is exercised end to end: record flags are all-or-none usage errors before the
//! lock; a timeout records `cause` `timeout` with the handover on disk BEFORE the signal; a vanished
//! program records `died` at the next check; a deliberate stop records nothing; the same death is
//! recorded once; and every hostile folder/name plant refuses fail-closed with the entry KEPT and the
//! process still alive (the record precedes the signal). No wall-clock assert anywhere: order is proved
//! by bounded polls on observable state and by byte-for-byte file reads, never by elapsed time.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// The record-mode fixture values (decision 2): a full lowercase uuid and a 40-hex lowercase sha.
const SESSION: &str = "0b6e3c1a-2f4d-4c8e-9a7b-1d2e3f405162";
const CHECKPOINT: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c";
/// Decision 4's fixed handover key order.
const HANDOVER_KEYS: [&str; 15] = [
    "v", "ticket", "repo", "id", "pid", "pid_start_ms", "cause", "limit", "timeout_ms", "steps",
    "hard_limit", "detected_ms", "checkpoint", "recipe", "session_id",
];

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("dcli-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// A scratch layout: `R` is the ticket's root (and the event-log root), `O` a sibling outside it,
    /// `D` holds the manifest (and the deaths folder).
    fn fence(&self) -> (PathBuf, PathBuf, PathBuf) {
        let r = self.0.join("R");
        let o = self.0.join("O");
        let d = self.0.join("D");
        fs::create_dir_all(&r).unwrap();
        fs::create_dir_all(&o).unwrap();
        fs::create_dir_all(&d).unwrap();
        (r, o, d)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
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

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn poll(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    f()
}

/// Kills the whole process group of every registered pid when dropped (TERM then KILL), so a failed test
/// run leaves nothing behind.
#[derive(Default)]
struct Guard {
    pids: Vec<u32>,
}

impl Guard {
    fn add(&mut self, pid: u32) {
        self.pids.push(pid);
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        for (pid, sig) in self.pids.iter().flat_map(|p| [(*p, "TERM"), (*p, "KILL")]) {
            let _ = Command::new("kill")
                .arg(format!("-{}", sig))
                .arg("--")
                .arg(format!("-{}", pid))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn spawn_own_group(program: &str, arg: &str) -> Child {
    Command::new(program)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap()
}

fn init_manifest(ticket: &str, r: &Path, d: &Path) {
    let i = run(&["manifest", "init", ticket, "--dir", d.to_str().unwrap(), "--root", r.to_str().unwrap()]);
    assert_eq!(code(&i), 0, "init: {}", err(&i));
}

/// Start `prog` under `ticket`/`id`; answers (output, pid, pid_start_ms from the stdout line).
fn start(ticket: &str, id: &str, cwd: &Path, d: &Path, timeout_ms: Option<u64>, prog: &[&str]) -> (Output, u32, u64) {
    let mut v: Vec<String> = vec![
        "process".into(), "start".into(), ticket.into(), "--id".into(), id.into(),
        "--cwd".into(), cwd.to_string_lossy().into_owned(),
    ];
    if let Some(t) = timeout_ms {
        v.push("--timeout-ms".into());
        v.push(t.to_string());
    }
    v.push("--dir".into());
    v.push(d.to_string_lossy().into_owned());
    v.push("--".into());
    for a in prog {
        v.push((*a).into());
    }
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    let o = run(&args);
    let text = out(&o);
    let fields: Vec<&str> = text.trim_end().split('\t').collect();
    let pid = fields.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let start_ms = fields.get(2).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    (o, pid, start_ms)
}

/// The record-mode flag tail (decision 2) shared by check and stop: session, checkpoint, repo, log, root.
fn record_flags(log: &Path, root: &Path) -> Vec<String> {
    vec![
        "--session".to_string(), SESSION.to_string(),
        "--checkpoint".to_string(), CHECKPOINT.to_string(),
        "--repo".to_string(), "r".to_string(),
        "--log".to_string(), log.to_string_lossy().into_owned(),
        "--root".to_string(), root.to_string_lossy().into_owned(),
    ]
}

fn check_rec(ticket: &str, d: &Path, log: &Path, root: &Path) -> Output {
    let mut v: Vec<String> = vec!["process".into(), "check".into(), ticket.into(), "--dir".into(), d.to_string_lossy().into_owned()];
    v.extend(record_flags(log, root));
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    run(&args)
}

fn check_plain(ticket: &str, d: &Path) -> Output {
    run(&["process", "check", ticket, "--dir", d.to_str().unwrap()])
}

fn stop_rec(ticket: &str, id: &str, d: &Path, log: &Path, root: &Path) -> Output {
    let mut v: Vec<String> = vec![
        "process".into(), "stop".into(), ticket.into(), "--id".into(), id.into(),
        "--dir".into(), d.to_string_lossy().into_owned(),
    ];
    v.extend(record_flags(log, root));
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    run(&args)
}

/// `process step` with the full step flags plus the record pair `--session`/`--checkpoint`.
fn step_rec(ticket: &str, d: &Path, log: &Path, root: &Path, steps: u64, soft: u64, hard: u64) -> Output {
    let args: Vec<String> = vec![
        "process".into(), "step".into(), ticket.into(),
        "--repo".into(), "r".into(),
        "--steps".into(), steps.to_string(),
        "--soft".into(), soft.to_string(),
        "--hard".into(), hard.to_string(),
        "--grace-ms".into(), "500".into(),
        "--dir".into(), d.to_string_lossy().into_owned(),
        "--log".into(), log.to_string_lossy().into_owned(),
        "--root".into(), root.to_string_lossy().into_owned(),
        "--session".into(), SESSION.to_string(),
        "--checkpoint".into(), CHECKPOINT.to_string(),
    ];
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run(&a)
}

/// The deaths folder of `ticket` under manifest dir `d`, and the handover file of one process life.
fn deaths_dir(d: &Path, ticket: &str) -> PathBuf {
    d.join(format!("{ticket}.deaths"))
}

fn handover_path(d: &Path, ticket: &str, id: &str, pid: u64, pid_start_ms: u64) -> PathBuf {
    deaths_dir(d, ticket).join(format!("{id}.{pid}.{pid_start_ms}.json"))
}

/// The handover relpath as the event's `handover` field carries it (relative to the manifest dir).
fn handover_relpath(ticket: &str, id: &str, pid: u64, pid_start_ms: u64) -> String {
    format!("{ticket}.deaths/{id}.{pid}.{pid_start_ms}.json")
}

/// The keys of a flat compact JSON object in file order (a string followed by `:` is a key).
fn keys_in_order(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += 1;
            }
            let s = String::from_utf8_lossy(&b[i + 1..j]).into_owned();
            let mut k = j + 1;
            while k < b.len() && (b[k] == b' ' || b[k] == b'\t') {
                k += 1;
            }
            if k < b.len() && b[k] == b':' {
                out.push(s);
                i = k + 1;
                continue;
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// One string field of a flat compact JSON object (panics when absent: the asserts name the file).
fn json_str(text: &str, key: &str) -> String {
    let pat = format!("\"{key}\":");
    let at = text.find(&pat).unwrap_or_else(|| panic!("no key {key} in {text}"));
    let rest = &text[at + pat.len()..];
    if let Some(stripped) = rest.strip_prefix('"') {
        let end = stripped.find('"').unwrap_or_else(|| panic!("unterminated value for {key} in {text}"));
        stripped[..end].to_string()
    } else {
        panic!("key {key} is not a string in {text}")
    }
}

/// One numeric field of a flat compact JSON object (panics when absent).
fn json_num(text: &str, key: &str) -> u64 {
    let pat = format!("\"{key}\":");
    let at = text.find(&pat).unwrap_or_else(|| panic!("no key {key} in {text}"));
    let rest = text[at + pat.len()..].trim_start();
    let end = rest.find([',', '}']).unwrap_or_else(|| panic!("unterminated value for {key} in {text}"));
    rest[..end].parse().unwrap_or_else(|_| panic!("key {key} is not a number in {text}"))
}

/// The `timed_out` lines of a log file (empty when the file is absent).
fn timed_out_lines(log: &Path) -> Vec<String> {
    fs::read_to_string(log)
        .map(|t| t.lines().filter(|l| l.contains("\"kind\":\"timed_out\"")).map(|l| l.to_string()).collect())
        .unwrap_or_default()
}

/// The whole seconds of a pid's `ps etime` (the same granularity the supervisor's off-Linux age read
/// uses), or None when the pid is gone.
fn etime_secs(pid: u32) -> Option<u64> {
    let o = Command::new("ps").arg("-o").arg("etime=").arg("-p").arg(pid.to_string()).stdin(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let et = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let main = et.split('.').next().unwrap_or(&et).to_string();
    let (days, time) = match main.split_once('-') {
        Some((d, r)) => (d.trim().parse().ok()?, r),
        None => (0, main.as_str()),
    };
    let parts: Vec<u64> = time.split(':').map(|p| p.trim().parse().ok()).collect::<Option<Vec<u64>>>()?;
    match parts.as_slice() {
        [m, s] => Some(days * 86400 + m * 60 + s),
        [h, m, s] => Some(days * 86400 + h * 3600 + m * 60 + s),
        _ => None,
    }
}

/// Poll until the pid's whole-second age reaches `secs` (2 s clears the truncation of a 1000 ms timeout
/// with margin); a state poll, never a wall-clock assert.
fn wait_past_age(pid: u32, secs: u64) {
    assert!(poll(30_000, || etime_secs(pid).is_some_and(|s| s >= secs)), "pid {pid} never reached an etime of {secs} s");
}

/// The REAL OS start time (ms) of a live pid, read through the public `manifest add --class process` verb
/// into a throwaway manifest (the same read the supervisor itself uses).
fn real_start_ms(pid: u32) -> u64 {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("probe", &r, &d);
    let add = run(&["manifest", "add", "probe", "--class", "process", "--id", "probe-1", "--pid", &pid.to_string(), "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&add), 0, "probe add of pid {pid}: {}", err(&add));
    let l = run(&["manifest", "list", "probe", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    let listing = out(&l);
    let line = listing.lines().find(|x| x.starts_with("probe-1\tprocess\t")).expect("the probe manifest lists the process line").to_string();
    line.split('\t').nth(3).expect("the process line carries pid_start_ms").parse().expect("pid_start_ms is a number")
}

/// Plant a schema-valid `add` process line with a CHOSEN `pid_start_ms` (the fixture carrier for the
/// re-used-pid cases).
fn plant_process(d: &Path, ticket: &str, id: &str, pid: u32, pid_start_ms: u64) {
    let line = format!(
        "{{\"v\":1,\"op\":\"add\",\"ticket\":\"{ticket}\",\"id\":\"{id}\",\"class\":\"process\",\"pid\":{pid},\"pid_start_ms\":{pid_start_ms},\"ts_ms\":1730000000002}}\n"
    );
    let mut f = fs::OpenOptions::new().append(true).open(d.join(format!("{ticket}.jsonl"))).unwrap();
    f.write_all(line.as_bytes()).unwrap();
}

// (write_all needs the trait in scope for the plant helper)
use std::io::Write as _;

// ---------------------------------------------------------------- AC 2: usage refused before anything

/// AC 2: every partial or bad record-flag set on check, stop and step answers 2 BEFORE the lock: no lock
/// file, the manifest byte-identical, no log file, no deaths folder.
#[test]
fn record_flag_usage_errors_refuse_before_anything() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-2", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, _start) = start("t-2", "a", &r, &d, Some(600_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let log = r.join("events.jsonl");
    let file = d.join("t-2.jsonl");
    let before = fs::read(&file).unwrap();
    // `start` has already taken (and released) the per-ticket lock; a usage error must leave it untouched.
    let lock = d.join("t-2.proc.lock");
    let lock_before = fs::read(&lock).ok();

    let upper = "0B6E3C1A-2F4D-4C8E-9A7B-1D2E3F405162";
    let short = "0b6e3c1a-2f4d-4c8e-9a7b-1d2e3f40516";
    let hex39 = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3";
    // Each bad set: --session without --checkpoint, an uppercase uuid, a short uuid, a 39-hex
    // checkpoint, a non-token repo. Built as check-shaped flag tails, then run against all three verbs.
    let bad_sets: Vec<Vec<String>> = vec![
        vec!["--session".into(), SESSION.to_string()],
        vec!["--session".into(), upper.into(), "--checkpoint".into(), CHECKPOINT.to_string(), "--repo".into(), "r".into()],
        vec!["--session".into(), short.into(), "--checkpoint".into(), CHECKPOINT.to_string(), "--repo".into(), "r".into()],
        vec!["--session".into(), SESSION.to_string(), "--checkpoint".into(), hex39.into(), "--repo".into(), "r".into()],
        vec!["--session".into(), SESSION.to_string(), "--checkpoint".into(), CHECKPOINT.to_string(), "--repo".into(), "Not A Token".into()],
    ];

    for (i, tail) in bad_sets.iter().enumerate() {
        // check and stop take the whole tail; step already owns --repo, so the pair-only variants apply.
        let mut check_args: Vec<String> = vec!["process".into(), "check".into(), "t-2".into(), "--dir".into(), d.to_string_lossy().into_owned()];
        check_args.extend(tail.iter().cloned());
        let a: Vec<&str> = check_args.iter().map(|s| s.as_str()).collect();
        let c = run(&a);
        assert_eq!(code(&c), 2, "set {i} check: stdout {:?} stderr {:?}", out(&c), err(&c));

        let mut stop_args: Vec<String> = vec![
            "process".into(), "stop".into(), "t-2".into(), "--id".into(), "a".into(),
            "--dir".into(), d.to_string_lossy().into_owned(),
        ];
        stop_args.extend(tail.iter().cloned());
        let a: Vec<&str> = stop_args.iter().map(|s| s.as_str()).collect();
        let st = run(&a);
        assert_eq!(code(&st), 2, "set {i} stop: stdout {:?} stderr {:?}", out(&st), err(&st));

        // step: the session/checkpoint pair applied to the same bad values (its --repo is already a
        // required step flag, so the non-token repo set has no step variant here).
        let session = tail[1].clone();
        let checkpoint = match tail.iter().position(|x| x == "--checkpoint") {
            Some(at) => tail[at + 1].clone(),
            None => CHECKPOINT.to_string(),
        };
        if tail.iter().any(|x| x == "--checkpoint") {
            let args: Vec<String> = vec![
                "process".into(), "step".into(), "t-2".into(), "--repo".into(), "r".into(),
                "--steps".into(), "9".into(), "--soft".into(), "5".into(), "--hard".into(), "9".into(),
                "--dir".into(), d.to_string_lossy().into_owned(),
                "--session".into(), session, "--checkpoint".into(), checkpoint,
            ];
            let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let sp = run(&a);
            assert_eq!(code(&sp), 2, "set {i} step bad shape: stdout {:?} stderr {:?}", out(&sp), err(&sp));
        } else {
            let args: Vec<String> = vec![
                "process".into(), "step".into(), "t-2".into(), "--repo".into(), "r".into(),
                "--steps".into(), "9".into(), "--soft".into(), "5".into(), "--hard".into(), "9".into(),
                "--dir".into(), d.to_string_lossy().into_owned(),
                "--session".into(), session,
            ];
            let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let sp = run(&a);
            assert_eq!(code(&sp), 2, "set {i} step pair-only: stdout {:?} stderr {:?}", out(&sp), err(&sp));
        }

        assert_eq!(fs::read(&lock).ok(), lock_before, "set {i}: a usage error touched the lock file");
        assert_eq!(fs::read(&file).unwrap(), before, "set {i}: a usage error changed the manifest");
        assert!(!log.exists(), "set {i}: a usage error created the log");
        assert!(!deaths_dir(&d, "t-2").exists(), "set {i}: a usage error created the deaths folder");
    }
    assert!(alive(pid), "a usage error signalled the process");
}

// ---------------------------------------------------------------- AC 3: timeout recorded

/// AC 3: an expired entry under check + record flags answers 5 with today's exact stdout line plus ONE
/// `recorded` line; the log holds exactly one `timed_out` (cause timeout, the given checkpoint and
/// session, recipe, handover); the handover file has decision 4's exact keys in order with `limit` `time`
/// and `timeout_ms` 1000; folder 0700, file 0600; the manifest no longer lists the id.
#[test]
fn timeout_death_is_recorded() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-3", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, start_ms) = start("t-3", "a", &r, &d, Some(1_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");

    let c = check_rec("t-3", &d, &log, &r);
    assert_eq!(code(&c), 5, "an expired entry answers 5: stdout {:?} stderr {:?}", out(&c), err(&c));
    let want_recorded = format!("recorded\ta\ttimeout\t{}\n", handover_relpath("t-3", "a", pid as u64, start_ms));
    assert_eq!(out(&c), format!("a\t{pid}\ttimed_out\n{want_recorded}"), "stdout is the timed_out line then the recorded line");

    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 1, "exactly one timed_out line: {:?}", fs::read_to_string(&log));
    let ev = &lines[0];
    assert_eq!(json_str(ev, "cause"), "timeout");
    assert_eq!(json_str(ev, "checkpoint"), CHECKPOINT);
    assert_eq!(json_str(ev, "session_id"), SESSION);
    assert_eq!(json_str(ev, "recipe"), "commit_push_then_resume");
    assert_eq!(json_str(ev, "handover"), handover_relpath("t-3", "a", pid as u64, start_ms));
    assert_eq!(json_str(ev, "ticket"), "t-3");
    assert_eq!(json_str(ev, "repo"), "r");

    let v = run(&["events", "verify", "--log", log.to_str().unwrap()]);
    assert_eq!(code(&v), 0, "events verify: {}", err(&v));

    let hand = handover_path(&d, "t-3", "a", pid as u64, start_ms);
    let text = fs::read_to_string(&hand).unwrap();
    assert!(text.ends_with('\n'), "the handover line is newline-terminated");
    assert_eq!(keys_in_order(text.trim_end()), HANDOVER_KEYS.to_vec(), "the handover keys are exactly decision 4's, in order");
    assert_eq!(json_num(&text, "v"), 1);
    assert_eq!(json_str(&text, "ticket"), "t-3");
    assert_eq!(json_str(&text, "repo"), "r");
    assert_eq!(json_str(&text, "id"), "a");
    assert_eq!(json_num(&text, "pid"), pid as u64);
    assert_eq!(json_num(&text, "pid_start_ms"), start_ms);
    assert_eq!(json_str(&text, "cause"), "timeout");
    assert_eq!(json_str(&text, "limit"), "time");
    assert_eq!(json_num(&text, "timeout_ms"), 1_000);
    assert_eq!(json_num(&text, "steps"), 0);
    assert_eq!(json_num(&text, "hard_limit"), 0);
    assert!(json_num(&text, "detected_ms") > 0, "detected_ms is a wall timestamp");
    assert_eq!(json_str(&text, "checkpoint"), CHECKPOINT);
    assert_eq!(json_str(&text, "recipe"), "commit_push_then_resume");
    assert_eq!(json_str(&text, "session_id"), SESSION);
    assert!(text.len() <= 4096, "the handover line is within the byte cap");

    let dmode = fs::metadata(deaths_dir(&d, "t-3")).unwrap().mode() & 0o777;
    let fmode = fs::metadata(&hand).unwrap().mode() & 0o777;
    assert_eq!(dmode, 0o700, "the deaths folder is 0700");
    assert_eq!(fmode, 0o600, "the handover file is 0600");

    let l = run(&["manifest", "list", "t-3", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).lines().any(|x| x.starts_with("a\t")), "the stopped id is still listed: {}", out(&l));
}

// ---------------------------------------------------------------- AC 4: vanished program found by check

/// AC 4: a program that exited on its own (no time limit) is found by check in record mode: exit 5,
/// already-gone stderr verbatim, `recorded ... died` on stdout, one `timed_out` with cause `died`, the
/// handover `limit` `none`, the entry removed. The same check WITHOUT record flags on a fresh plant is
/// today's behaviour: exit 0, nothing written, the entry still listed.
#[test]
fn vanished_program_is_found_by_check() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-4", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, start_ms) = start("t-4", "a", &r, &d, None, &["/bin/sh", "-c", "exit 0"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    assert!(poll(10_000, || !alive(pid)), "the fixture process never exited");
    let log = r.join("events.jsonl");

    let c = check_rec("t-4", &d, &log, &r);
    assert_eq!(code(&c), 5, "a vanished entry answers 5: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(
        err(&c).contains(&format!("harness: a is already-gone: pid {pid} is provably no such process")),
        "stop's already-gone stderr line is verbatim: stderr {:?}",
        err(&c)
    );
    assert_eq!(out(&c), format!("recorded\ta\tdied\t{}\n", handover_relpath("t-4", "a", pid as u64, start_ms)), "stdout carries only the recorded line");
    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 1);
    assert_eq!(json_str(&lines[0], "cause"), "died");
    let hand = fs::read_to_string(handover_path(&d, "t-4", "a", pid as u64, start_ms)).unwrap();
    assert_eq!(json_str(&hand, "limit"), "none");
    assert_eq!(json_num(&hand, "timeout_ms"), 0);
    assert_eq!(json_str(&hand, "cause"), "died");
    let l = run(&["manifest", "list", "t-4", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).lines().any(|x| x.starts_with("a\t")), "the vanished id is still listed: {}", out(&l));

    // The control: the SAME plant without record flags is today's behaviour (exit 0, entry kept).
    let (s2, pid2, _ms2) = start("t-4", "b", &r, &d, None, &["/bin/sh", "-c", "exit 0"]);
    assert_eq!(code(&s2), 0, "start b: {}", err(&s2));
    guard.add(pid2);
    assert!(poll(10_000, || !alive(pid2)), "the second fixture process never exited");
    let log2 = r.join("events2.jsonl");
    let c2 = check_plain("t-4", &d);
    assert_eq!(code(&c2), 0, "legacy check of a no-limit entry is silent: stdout {:?} stderr {:?}", out(&c2), err(&c2));
    assert_eq!(out(&c2), "", "legacy check writes no stdout");
    assert!(!log2.exists(), "legacy check wrote a log");
    assert!(!deaths_dir(&d, "t-4").join(format!("b.{pid2}")).exists(), "legacy check wrote a handover");
    let l2 = run(&["manifest", "list", "t-4", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l2), 0);
    assert!(out(&l2).lines().any(|x| x.starts_with("b\t")), "legacy check removed the no-limit entry: {}", out(&l2));
}

// ---------------------------------------------------------------- AC 5: pid reuse records died on the recorded life

/// AC 5: the #183 pid-reuse plant plus record flags on stop: exit 3, the pid-reused line verbatim, one
/// `died` record keyed on the RECORDED `pid_start_ms`, the stranger still alive.
#[test]
fn reused_pid_records_died_on_the_recorded_life() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-5", &r, &d);
    let mut guard = Guard::default();
    let mut own = spawn_own_group("sleep", "60");
    let pid = own.id();
    guard.add(pid);
    let real = real_start_ms(pid);
    let recorded = real.saturating_sub(3_600_000);
    assert!(real.abs_diff(recorded) > 2000, "the fixture must carry a recorded start time far off the real one");
    plant_process(&d, "t-5", "a", pid, recorded);
    let log = r.join("events.jsonl");

    let st = stop_rec("t-5", "a", &d, &log, &r);
    assert_eq!(code(&st), 3, "a re-used pid answers 3: stdout {:?} stderr {:?}", out(&st), err(&st));
    assert!(err(&st).contains("pid-reused"), "the answer names pid-reused: stderr {:?}", err(&st));
    let rel = handover_relpath("t-5", "a", pid as u64, recorded);
    assert_eq!(out(&st), format!("recorded\ta\tdied\t{rel}\n"), "stdout carries the recorded line");
    assert!(handover_path(&d, "t-5", "a", pid as u64, recorded).exists(), "the handover is keyed on the RECORDED start time");
    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 1);
    assert_eq!(json_str(&lines[0], "cause"), "died");
    assert!(alive(pid), "the stranger was signalled");
    assert!(own.try_wait().unwrap().is_none(), "the stranger is dead");
    let _ = own.kill();
    let _ = own.wait();
}

// ---------------------------------------------------------------- AC 6: a deliberate stop is not a death

/// AC 6: `stop --id` with record flags on a live entry is exit 0, `a<TAB>pid<TAB>stopped`, no `timed_out`
/// line and no handover file at all.
#[test]
fn deliberate_stop_records_nothing() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-6", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, _start) = start("t-6", "a", &r, &d, None, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let log = r.join("events.jsonl");

    let st = stop_rec("t-6", "a", &d, &log, &r);
    assert_eq!(code(&st), 0, "a deliberate stop answers 0: stdout {:?} stderr {:?}", out(&st), err(&st));
    assert_eq!(out(&st), format!("a\t{pid}\tstopped\n"), "stdout is exactly the stopped line");
    assert!(!log.exists(), "a deliberate stop wrote a log");
    assert!(!deaths_dir(&d, "t-6").exists(), "a deliberate stop created the deaths folder");
}

// ---------------------------------------------------------------- AC 7: the step hard line

/// AC 7: two live processes at the hard line: exit 6; the `step_limit_stop` line comes BEFORE both
/// `timed_out` lines in the log; each handover carries `limit` `step`, `steps` 9, `hard_limit` 9.
#[test]
fn step_hard_line_records_after_step_limit_stop() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-7", &r, &d);
    let mut guard = Guard::default();
    let (s1, p1, m1) = start("t-7", "a", &r, &d, None, &["sleep", "30"]);
    assert_eq!(code(&s1), 0, "start a: {}", err(&s1));
    guard.add(p1);
    let (s2, p2, m2) = start("t-7", "b", &r, &d, None, &["sleep", "30"]);
    assert_eq!(code(&s2), 0, "start b: {}", err(&s2));
    guard.add(p2);
    let log = r.join("events.jsonl");

    let st = step_rec("t-7", &d, &log, &r, 9, 5, 9);
    assert_eq!(code(&st), 6, "the hard line answers 6: stdout {:?} stderr {:?}", out(&st), err(&st));
    let text = fs::read_to_string(&log).unwrap();
    let all: Vec<&str> = text.lines().collect();
    let at_limit = all.iter().position(|l| l.contains("\"kind\":\"step_limit_stop\"")).expect("a step_limit_stop line");
    for (id, pid, ms) in [("a", p1, m1), ("b", p2, m2)] {
        let at_death = all
            .iter()
            .position(|l| l.contains("\"kind\":\"timed_out\"") && l.contains(&format!("\"handover\":\"{}\"", handover_relpath("t-7", id, pid as u64, ms))))
            .unwrap_or_else(|| panic!("no timed_out line for {id}: {text}"));
        assert!(at_limit < at_death, "step_limit_stop precedes the timed_out of {id}");
        let hand = fs::read_to_string(handover_path(&d, "t-7", id, pid as u64, ms)).unwrap();
        assert_eq!(json_str(&hand, "cause"), "timeout");
        assert_eq!(json_str(&hand, "limit"), "step");
        assert_eq!(json_num(&hand, "steps"), 9);
        assert_eq!(json_num(&hand, "hard_limit"), 9);
    }
    assert_eq!(all.len(), 3, "one step_limit_stop plus two timed_out lines: {text}");
}

// ---------------------------------------------------------------- AC 8: the same death once

/// AC 8a: re-running AC 3's check after a full success answers 0 and changes neither the log nor the
/// deaths folder by one byte.
#[test]
fn same_death_recorded_once_rerun() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-8", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, start_ms) = start("t-8", "a", &r, &d, Some(1_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");
    let c = check_rec("t-8", &d, &log, &r);
    assert_eq!(code(&c), 5, "the first check records: stdout {:?} stderr {:?}", out(&c), err(&c));
    let log_before = fs::read(&log).unwrap();
    let hand = handover_path(&d, "t-8", "a", pid as u64, start_ms);
    let hand_before = fs::read(&hand).unwrap();

    let c2 = check_rec("t-8", &d, &log, &r);
    assert_eq!(code(&c2), 0, "the rerun finds nothing live: stdout {:?} stderr {:?}", out(&c2), err(&c2));
    assert_eq!(out(&c2), "", "the rerun prints nothing");
    assert_eq!(fs::read(&log).unwrap(), log_before, "the rerun changed the log");
    assert_eq!(fs::read(&hand).unwrap(), hand_before, "the rerun changed the handover");
}

/// AC 8b: 20 rounds of two CONCURRENT record-mode checks on one vanished entry: exactly one `timed_out`
/// line and one handover per round (the per-ticket lock serialises; the loser finds the entry removed).
#[test]
fn same_death_recorded_once_concurrent() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-8b", &r, &d);
    let mut guard = Guard::default();
    for round in 0..20 {
        let id = format!("a{round}");
        let (s, pid, _ms) = start("t-8b", &id, &r, &d, None, &["/bin/sh", "-c", "exit 0"]);
        assert_eq!(code(&s), 0, "start {id}: {}", err(&s));
        guard.add(pid);
        assert!(poll(10_000, || !alive(pid)), "round {round}: the fixture process never exited");
        let log = r.join(format!("e{round}.jsonl"));

        let mut args: Vec<String> = vec!["process".into(), "check".into(), "t-8b".into(), "--dir".into(), d.to_string_lossy().into_owned()];
        args.extend(record_flags(&log, &r));
        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let mut c1 = Command::new(BIN).args(&a).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let mut c2 = Command::new(BIN).args(&a).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let w1 = c1.wait().unwrap().code().unwrap_or(-1);
        let w2 = c2.wait().unwrap().code().unwrap_or(-1);

        assert!([w1, w2].contains(&5), "round {round}: nobody recorded (codes {w1}/{w2})");
        assert!([w1, w2].contains(&0), "round {round}: both runs recorded (codes {w1}/{w2})");
        let lines = timed_out_lines(&log);
        assert_eq!(lines.len(), 1, "round {round}: exactly one timed_out line");
        let hands: Vec<_> = fs::read_dir(deaths_dir(&d, "t-8b")).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(hands.len(), (round + 1) as usize, "round {round}: exactly one handover per round");
    }
}

// ---------------------------------------------------------------- AC 10: hostile folder and names

/// One hostile plant of AC 10: an EXPIRED entry under check + record flags must refuse (exit 2) with the
/// decision-9 reason and `harness: DEATH NOT RECORDED`, print no absolute path, leave the outside target
/// untouched, write no `timed_out`, keep the entry listed and the process alive (the record precedes the
/// signal).
fn assert_hostile_refusal(name: &str, t: &Tmp, reason: &str, plant: impl FnOnce(&PathBuf, &PathBuf, u32, u64)) {
    let (r, o, d) = t.fence();
    let ticket = format!("h-{name}");
    init_manifest(&ticket, &r, &d);
    let mut guard = Guard::default();
    let (s, pid, start_ms) = start(&ticket, "a", &r, &d, Some(1_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");
    plant(&o, &d, pid, start_ms);
    let outside = o.join(format!("outside-{name}"));
    let outside_before = fs::read(&outside).unwrap_or_default();
    let manifest_before = fs::read(d.join(format!("{ticket}.jsonl"))).unwrap();

    let c = check_rec(&ticket, &d, &log, &r);
    assert_eq!(code(&c), 2, "{name}: the hostile plant refuses: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains(reason), "{name}: the refusal names `{reason}`: stderr {:?}", err(&c));
    assert!(err(&c).contains("harness: DEATH NOT RECORDED"), "{name}: the refusal says the death was not recorded: stderr {:?}", err(&c));
    assert!(!err(&c).contains(&t.0.to_string_lossy().to_string()), "{name}: an absolute path leaked: stderr {:?}", err(&c));
    assert_eq!(fs::read(&outside).unwrap_or_default(), outside_before, "{name}: the outside target changed");
    assert_eq!(fs::read(d.join(format!("{ticket}.jsonl"))).unwrap(), manifest_before, "{name}: the manifest changed");
    if log.exists() {
        assert!(!fs::read_to_string(&log).unwrap().contains("timed_out"), "{name}: a timed_out was written");
    }
    let l = run(&["manifest", "list", &ticket, "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(out(&l).lines().any(|x| x.starts_with("a\t")), "{name}: the entry was removed");
    assert!(alive(pid), "{name}: the process was signalled");
}

/// AC 10a: `t.deaths` is a symlink to an outside folder.
#[test]
fn deaths_folder_symlink_refuses() {
    let t = Tmp::new();
    assert_hostile_refusal("a", &t, "deaths folder is a symlink", |o, d, _pid, _ms| {
        let target = o.join("outside-a");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, deaths_dir(d, "h-a")).unwrap();
    });
}

/// AC 10b: `t.deaths` is a regular file.
#[test]
fn deaths_folder_file_refuses() {
    let t = Tmp::new();
    assert_hostile_refusal("b", &t, "deaths folder is not a directory", |_o, d, _pid, _ms| {
        fs::write(deaths_dir(d, "h-b"), b"x").unwrap();
    });
}

/// AC 10c: `t.deaths` exists with mode 0755.
#[test]
fn deaths_folder_open_mode_refuses() {
    let t = Tmp::new();
    assert_hostile_refusal("c", &t, "deaths folder is readable by group or others", |_o, d, _pid, _ms| {
        let f = deaths_dir(d, "h-c");
        fs::create_dir_all(&f).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
    });
}

/// AC 10d (fix r1, F3): the final handover name is a pre-planted symlink to an outside file that is a
/// VALID same-life handover — the one target for which dropping the final-name check would silently
/// REUSE a record outside the folder. (An empty or garbage target refuses through the strict decode
/// anyway, which is why the pre-r1 shape of this test stayed green without the check.)
#[test]
fn planted_handover_symlink_refuses() {
    let t = Tmp::new();
    assert_hostile_refusal("d", &t, "exists and does not match this death", |o, d, pid, ms| {
        let target = o.join("outside-d");
        fs::write(&target, valid_handover_line("h-d", "a", pid as u64, ms, "died", "none")).unwrap();
        let f = deaths_dir(d, "h-d");
        fs::create_dir_all(&f).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&target, f.join(format!("a.{pid}.{ms}.json"))).unwrap();
    });
}

/// AC 10e: the final handover name holds garbage.
#[test]
fn planted_handover_garbage_refuses() {
    let t = Tmp::new();
    assert_hostile_refusal("e", &t, "exists and does not match this death", |_o, d, pid, ms| {
        let f = deaths_dir(d, "h-e");
        fs::create_dir_all(&f).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(f.join(format!("a.{pid}.{ms}.json")), b"not json at all").unwrap();
    });
}

// ---------------------------------------------------------------- AC 11: a torn log refuses before signalling

/// AC 11: an expired entry with a torn-tailed log: check + record flags exits 2 with `event log
/// refused:` and `DEATH NOT RECORDED`, the process stays alive, the manifest and the log byte-identical.
#[test]
fn torn_log_refuses_before_signalling() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-11", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, _ms) = start("t-11", "a", &r, &d, Some(1_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");
    fs::write(&log, b"{\"v\":1").unwrap();
    let log_before = fs::read(&log).unwrap();
    let manifest_before = fs::read(d.join("t-11.jsonl")).unwrap();

    let c = check_rec("t-11", &d, &log, &r);
    assert_eq!(code(&c), 2, "a torn log refuses: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains("event log refused:"), "the refusal names the log: stderr {:?}", err(&c));
    assert!(err(&c).contains("harness: DEATH NOT RECORDED"), "stderr {:?}", err(&c));
    assert!(alive(pid), "the process was signalled despite the refusal");
    assert_eq!(fs::read(&log).unwrap(), log_before, "the log changed");
    assert_eq!(fs::read(d.join("t-11.jsonl")).unwrap(), manifest_before, "the manifest changed");
    assert!(!deaths_dir(&d, "t-11").exists(), "the deaths folder was created");
}

// ---------------------------------------------------------------- AC 13: a dirty manifest refuses first

/// AC 13: a torn manifest plus record flags on check, stop and step: exit 2, nothing written, no deaths
/// folder created.
#[test]
fn dirty_manifest_refuses_before_any_folder() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-13", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, _ms) = start("t-13", "a", &r, &d, None, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    {
        use std::io::Write as _;
        let mut f = fs::OpenOptions::new().append(true).open(d.join("t-13.jsonl")).unwrap();
        f.write_all(b"{\"v\":1").unwrap();
    }
    let log = r.join("events.jsonl");

    let c = check_rec("t-13", &d, &log, &r);
    assert_eq!(code(&c), 2, "check on a torn manifest refuses: stdout {:?} stderr {:?}", out(&c), err(&c));
    let st = stop_rec("t-13", "a", &d, &log, &r);
    assert_eq!(code(&st), 2, "stop on a torn manifest refuses: stdout {:?} stderr {:?}", out(&st), err(&st));
    let sp = step_rec("t-13", &d, &log, &r, 9, 5, 9);
    assert_eq!(code(&sp), 2, "step on a torn manifest refuses: stdout {:?} stderr {:?}", out(&sp), err(&sp));
    for (name, o) in [("check", &c), ("stop", &st), ("step", &sp)] {
        assert!(err(o).contains("torn tail"), "{name} names the torn manifest, not a usage error: stderr {:?}", err(o));
    }
    assert!(!deaths_dir(&d, "t-13").exists(), "the deaths folder was created");
    assert!(!log.exists(), "the log was created");
    assert!(alive(pid), "the process was signalled");
}

// ---------------------------------------------------------------- AC 14: nothing private in the record

/// AC 14: a program whose argv carries the marker `zz-marker-210` and whose cwd is a root: after AC 3's
/// flow neither the handover file nor the log line contains the marker, and the log line holds no `/`
/// outside the `handover` value.
#[test]
fn record_carries_nothing_private() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-14", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, start_ms) = start(
        "t-14",
        "a",
        &r,
        &d,
        Some(1_000),
        &["/bin/sh", "-c", "echo zz-marker-210; sleep 30"],
    );
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");

    let c = check_rec("t-14", &d, &log, &r);
    assert_eq!(code(&c), 5, "the death is recorded: stdout {:?} stderr {:?}", out(&c), err(&c));
    let hand = fs::read_to_string(handover_path(&d, "t-14", "a", pid as u64, start_ms)).unwrap();
    assert!(!hand.contains("zz-marker-210"), "the handover carries the argv marker: {hand}");
    assert!(!hand.contains('/'), "the handover carries a path separator: {hand}");
    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 1);
    let ev = &lines[0];
    assert!(!ev.contains("zz-marker-210"), "the event carries the argv marker: {ev}");
    let rel = handover_relpath("t-14", "a", pid as u64, start_ms);
    let stripped = ev.replace(&format!("\"handover\":\"{rel}\""), "");
    assert!(!stripped.contains('/'), "a `/` outside the handover value: {ev}");
}

// ---------------------------------------------------------------- fix r1 helpers

/// A schema-valid handover line for one life (the fixture carrier for the reuse/symlink plants).
fn valid_handover_line(ticket: &str, id: &str, pid: u64, ms: u64, cause: &str, limit: &str) -> String {
    format!(
        "{{\"v\":1,\"ticket\":\"{ticket}\",\"repo\":\"r\",\"id\":\"{id}\",\"pid\":{pid},\"pid_start_ms\":{ms},\"cause\":\"{cause}\",\"limit\":\"{limit}\",\"timeout_ms\":0,\"steps\":0,\"hard_limit\":0,\"detected_ms\":1,\"checkpoint\":\"{CHECKPOINT}\",\"recipe\":\"commit_push_then_resume\",\"session_id\":\"{SESSION}\"}}\n"
    )
}

/// Run the binary with a DEADLINE (fix r1, F2): a verb that would block forever on a planted FIFO or
/// socket is killed at the bound and the test fails, instead of hanging the suite. The bound is on the
/// WAIT only (25 ms state polls up to `ms`); no wall-clock assert on the verb's own runtime.
fn run_bounded(args: &[&str], ms: u64) -> Output {
    let mut c = Command::new(BIN).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        match c.try_wait().unwrap() {
            Some(_) => return c.wait_with_output().unwrap(),
            None if Instant::now() >= deadline => {
                let _ = c.kill();
                let o = c.wait_with_output().unwrap();
                panic!("the verb did not answer within {ms} ms: killed; stdout {:?} stderr {:?}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// check + record flags, bounded (for the plants that must refuse, never hang).
fn check_rec_bounded(ticket: &str, d: &Path, log: &Path, root: &Path) -> Output {
    let mut v: Vec<String> = vec!["process".into(), "check".into(), ticket.into(), "--dir".into(), d.to_string_lossy().into_owned()];
    v.extend(record_flags(log, root));
    let a: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    run_bounded(&a, 10_000)
}

// ---------------------------------------------------------------- fix r1 F1: step's cause at the hard line

/// Fix r1 F1: at the step hard line with one GONE entry and one LIVE entry, the gone one is `died`
/// (limit `none` from its entry) in BOTH the event log and the handover, the live one alone is
/// `timeout`/`step`; the step_limit_stop line stays first in the log.
#[test]
fn step_gone_entry_records_died_live_times_out() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-f1", &r, &d);
    let mut guard = Guard::default();
    let (s1, p1, m1) = start("t-f1", "a", &r, &d, None, &["/bin/sh", "-c", "exit 0"]);
    assert_eq!(code(&s1), 0, "start a: {}", err(&s1));
    guard.add(p1);
    assert!(poll(10_000, || !alive(p1)), "the gone fixture never exited");
    let (s2, p2, m2) = start("t-f1", "b", &r, &d, None, &["sleep", "30"]);
    assert_eq!(code(&s2), 0, "start b: {}", err(&s2));
    guard.add(p2);
    let log = r.join("events.jsonl");

    let st = step_rec("t-f1", &d, &log, &r, 9, 5, 9);
    assert_eq!(code(&st), 6, "the hard line answers 6: stdout {:?} stderr {:?}", out(&st), err(&st));
    // stdout, in order: the header, the gone entry's recorded line, then the live entry's pair.
    assert_eq!(
        out(&st),
        format!(
            "step_limit_stop\t9\t9\nrecorded\ta\tdied\t{}\nb\t{p2}\tstopped\nrecorded\tb\ttimeout\t{}\n",
            handover_relpath("t-f1", "a", p1 as u64, m1),
            handover_relpath("t-f1", "b", p2 as u64, m2)
        ),
        "stdout names died for the gone entry and timeout for the live one"
    );
    // the log: step_limit_stop first, then a's died line, then b's timeout line.
    let text = fs::read_to_string(&log).unwrap();
    let all: Vec<&str> = text.lines().collect();
    assert_eq!(all.len(), 3, "three lines: {text}");
    assert!(all[0].contains("\"kind\":\"step_limit_stop\""), "the limit event is first: {text}");
    assert!(all[1].contains(&format!("\"handover\":\"{}\"", handover_relpath("t-f1", "a", p1 as u64, m1))), "line 2 is a's death: {text}");
    assert_eq!(json_str(all[1], "cause"), "died", "the gone entry is died in the LOG: {text}");
    assert!(all[2].contains(&format!("\"handover\":\"{}\"", handover_relpath("t-f1", "b", p2 as u64, m2))), "line 3 is b's death: {text}");
    assert_eq!(json_str(all[2], "cause"), "timeout", "the live entry is timeout in the LOG: {text}");
    // the handovers carry the same causes, and the gone one its own entry's limit.
    let ha = fs::read_to_string(handover_path(&d, "t-f1", "a", p1 as u64, m1)).unwrap();
    assert_eq!(json_str(&ha, "cause"), "died", "the gone entry is died in the HANDOVER: {ha}");
    assert_eq!(json_str(&ha, "limit"), "none", "the gone entry's limit comes from its entry: {ha}");
    assert_eq!(json_num(&ha, "timeout_ms"), 0);
    let hb = fs::read_to_string(handover_path(&d, "t-f1", "b", p2 as u64, m2)).unwrap();
    assert_eq!(json_str(&hb, "cause"), "timeout");
    assert_eq!(json_str(&hb, "limit"), "step");
    // both entries are off the sheet.
    let l = run(&["manifest", "list", "t-f1", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).lines().any(|x| x.starts_with("a\t") || x.starts_with("b\t")), "an entry survived: {}", out(&l));
}

/// Fix r1 F1 (the pid-reuse arm): a planted entry whose pid a stranger now holds, at the step hard
/// line, records `died` keyed on the RECORDED life — never `timeout`/`step` — and the stranger is
/// never signalled.
#[test]
fn step_pid_reused_records_died() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-f1r", &r, &d);
    let mut guard = Guard::default();
    let mut own = spawn_own_group("sleep", "60");
    let pid = own.id();
    guard.add(pid);
    let recorded = real_start_ms(pid).saturating_sub(3_600_000);
    plant_process(&d, "t-f1r", "a", pid, recorded);
    let log = r.join("events.jsonl");

    let st = step_rec("t-f1r", &d, &log, &r, 9, 5, 9);
    assert_eq!(code(&st), 6, "a reused pid at the hard line still answers 6: stdout {:?} stderr {:?}", out(&st), err(&st));
    let rel = handover_relpath("t-f1r", "a", pid as u64, recorded);
    assert_eq!(out(&st), format!("step_limit_stop\t9\t9\nrecorded\ta\tdied\t{rel}\n"), "stdout carries the died recorded line: {:?}", out(&st));
    assert!(err(&st).contains("pid-reused"), "the answer names pid-reused: stderr {:?}", err(&st));
    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 1, "one death line: {}", fs::read_to_string(&log).unwrap());
    assert_eq!(json_str(&lines[0], "cause"), "died", "the reused pid is died in the LOG, never timeout: {}", lines[0]);
    let hand = fs::read_to_string(handover_path(&d, "t-f1r", "a", pid as u64, recorded)).unwrap();
    assert_eq!(json_str(&hand, "cause"), "died", "the reused pid is died in the HANDOVER: {hand}");
    assert!(alive(pid), "the stranger was signalled");
    let _ = own.kill();
    let _ = own.wait();
}

/// Fix r1 F1 (the sibling hit the search found in `check`): an expired entry whose pid a stranger now
/// holds records `died` through check too — check's timeout staging is gated on the identity probe,
/// so the pre-signalled handover never carries a cause the next probe contradicts.
#[test]
fn check_pid_reused_expired_records_died() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-f1c", &r, &d);
    let mut guard = Guard::default();
    let mut own = spawn_own_group("sleep", "60");
    let pid = own.id();
    guard.add(pid);
    let recorded = real_start_ms(pid).saturating_sub(3_600_000);
    plant_process(&d, "t-f1c", "a", pid, recorded);
    {
        use std::io::Write as _;
        let line = "{\"v\":1,\"op\":\"limit\",\"ticket\":\"t-f1c\",\"id\":\"a\",\"timeout_ms\":1000,\"ts_ms\":1730000000003}\n";
        let mut f = fs::OpenOptions::new().append(true).open(d.join("t-f1c.jsonl")).unwrap();
        f.write_all(line.as_bytes()).unwrap();
    }
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");

    let c = check_rec("t-f1c", &d, &log, &r);
    assert_eq!(code(&c), 5, "the expired reused entry resolves: stdout {:?} stderr {:?}", out(&c), err(&c));
    let rel = handover_relpath("t-f1c", "a", pid as u64, recorded);
    assert_eq!(out(&c), format!("recorded\ta\tdied\t{rel}\n"), "stdout carries died, never a timeout line: {:?}", out(&c));
    assert!(err(&c).contains("pid-reused"), "the answer names pid-reused: stderr {:?}", err(&c));
    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 1);
    assert_eq!(json_str(&lines[0], "cause"), "died", "the log says died: {}", lines[0]);
    let hand = fs::read_to_string(handover_path(&d, "t-f1c", "a", pid as u64, recorded)).unwrap();
    assert_eq!(json_str(&hand, "cause"), "died", "the handover says died: {hand}");
    assert!(alive(pid), "the stranger was signalled");
    let _ = own.kill();
    let _ = own.wait();
}

// ---------------------------------------------------------------- fix r1 F2/v3: a non-regular final name never hangs

/// Fix r1 F2: a FIFO planted at the final handover name is refused (2, the mismatch reason, `DEATH NOT
/// RECORDED`) within the bounded wait — the open is never made, so nothing can block on the pipe with
/// the per-ticket lock held. The entry is kept and the process alive.
#[test]
fn fifo_at_final_name_refuses_without_hanging() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-f2", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, ms) = start("t-f2", "a", &r, &d, Some(1_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");
    let f = deaths_dir(&d, "t-f2");
    fs::create_dir_all(&f).unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o700)).unwrap();
    let mk = Command::new("mkfifo").arg(f.join(format!("a.{pid}.{ms}.json"))).stdin(Stdio::null()).output().unwrap();
    assert!(mk.status.success(), "mkfifo: {}", String::from_utf8_lossy(&mk.stderr));

    let c = check_rec_bounded("t-f2", &d, &log, &r);
    assert_eq!(code(&c), 2, "the FIFO refuses: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains("exists and does not match this death"), "the refusal is the planted-name reason: stderr {:?}", err(&c));
    assert!(err(&c).contains("harness: DEATH NOT RECORDED"), "stderr {:?}", err(&c));
    assert!(!err(&c).contains(&t.0.to_string_lossy().to_string()), "an absolute path leaked: stderr {:?}", err(&c));
    assert!(alive(pid), "the process was signalled despite the refusal");
    let l = run(&["manifest", "list", "t-f2", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(out(&l).lines().any(|x| x.starts_with("a\t")), "the entry was removed");
    if log.exists() {
        assert!(!fs::read_to_string(&log).unwrap().contains("timed_out"), "a timed_out was written");
    }
}

/// Held-out v3: a DIRECTORY planted at the final handover name refuses with the same message class as
/// the FIFO, within the bounded wait — no hang, no open.
#[test]
fn directory_at_final_name_refuses_without_hanging() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-v3", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, ms) = start("t-v3", "a", &r, &d, Some(1_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");
    let f = deaths_dir(&d, "t-v3");
    fs::create_dir_all(f.join(format!("a.{pid}.{ms}.json"))).unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o700)).unwrap();

    let c = check_rec_bounded("t-v3", &d, &log, &r);
    assert_eq!(code(&c), 2, "the directory refuses: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains("exists and does not match this death"), "the same planted-name reason as the FIFO: stderr {:?}", err(&c));
    assert!(err(&c).contains("harness: DEATH NOT RECORDED"), "stderr {:?}", err(&c));
    assert!(alive(pid), "the process was signalled despite the refusal");
    let l = run(&["manifest", "list", "t-v3", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(out(&l).lines().any(|x| x.starts_with("a\t")), "the entry was removed");
}

// ---------------------------------------------------------------- fix r1 F4: a failed append after the signal

/// Fix r1 F4: the `timed_out` append fails AFTER the pre-flight passed and the signal was sent (the
/// log file is read-only: readable at pre-flight, unwritable at the append). The verb answers 2 with
/// `DEATH NOT RECORDED`, the entry is KEPT, the staged handover stays as the intent record, and the
/// log holds no `timed_out` line.
#[test]
fn failed_append_after_signal_keeps_entry() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-f4", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, ms) = start("t-f4", "a", &r, &d, Some(1_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");
    fs::write(&log, b"").unwrap();
    fs::set_permissions(&log, fs::Permissions::from_mode(0o444)).unwrap();

    let c = check_rec("t-f4", &d, &log, &r);
    assert_eq!(code(&c), 2, "the failed append answers 2: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains("event log refused"), "the refusal names the log: stderr {:?}", err(&c));
    assert!(err(&c).contains("harness: DEATH NOT RECORDED"), "stderr {:?}", err(&c));
    let l = run(&["manifest", "list", "t-f4", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(out(&l).lines().any(|x| x.starts_with("a\t")), "the entry was removed after a failed append: the death was silently lost");
    assert_eq!(fs::read(&log).unwrap(), b"", "the log changed");
    assert!(handover_path(&d, "t-f4", "a", pid as u64, ms).exists(), "the staged handover is the intent record for the retry");
    assert!(!timed_out_lines(&log).iter().any(|x| json_str(x, "id") == "a"), "a timed_out was written");
}

// ---------------------------------------------------------------- fix r1 held-out variants

/// Held-out v1: step at the hard line with TWO gone entries and one live entry: both gone are `died`
/// (in the log AND their handovers), the live one alone is `timeout`.
#[test]
fn held_out_step_two_gone_one_live() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-v1", &r, &d);
    let mut guard = Guard::default();
    let mut gone = Vec::new();
    for id in ["a", "b"] {
        let (s, pid, ms) = start("t-v1", id, &r, &d, None, &["/bin/sh", "-c", "exit 0"]);
        assert_eq!(code(&s), 0, "start {id}: {}", err(&s));
        guard.add(pid);
        assert!(poll(10_000, || !alive(pid)), "{id} never exited");
        gone.push((id.to_string(), pid, ms));
    }
    let (s3, p3, m3) = start("t-v1", "c", &r, &d, None, &["sleep", "30"]);
    assert_eq!(code(&s3), 0, "start c: {}", err(&s3));
    guard.add(p3);
    let log = r.join("events.jsonl");

    let st = step_rec("t-v1", &d, &log, &r, 9, 5, 9);
    assert_eq!(code(&st), 6, "stdout {:?} stderr {:?}", out(&st), err(&st));
    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 3, "one death per entry: {}", fs::read_to_string(&log).unwrap());
    for (id, pid, ms) in &gone {
        let ev = lines.iter().find(|l| l.contains(&format!("\"handover\":\"{}\"", handover_relpath("t-v1", id, *pid as u64, *ms)))).unwrap_or_else(|| panic!("no line for {id}: {lines:?}"));
        assert_eq!(json_str(ev, "cause"), "died", "{id} is not died in the log: {ev}");
        let hand = fs::read_to_string(handover_path(&d, "t-v1", id, *pid as u64, *ms)).unwrap();
        assert_eq!(json_str(&hand, "cause"), "died", "{id} is not died in the handover: {hand}");
        assert_eq!(json_str(&hand, "limit"), "none");
    }
    let ev = lines.iter().find(|l| l.contains(&format!("\"handover\":\"{}\"", handover_relpath("t-v1", "c", p3 as u64, m3)))).unwrap();
    assert_eq!(json_str(ev, "cause"), "timeout", "the live entry is not timeout: {ev}");
    let hand = fs::read_to_string(handover_path(&d, "t-v1", "c", p3 as u64, m3)).unwrap();
    assert_eq!(json_str(&hand, "cause"), "timeout");
    assert_eq!(json_str(&hand, "limit"), "step");
}

/// Held-out v2: pid reused AND past the step limit, with a live entry beside it: the reused entry is
/// `died` in the log and its handover, and no `timeout` line for that life exists anywhere.
#[test]
fn held_out_step_reused_and_live_died_never_timeout() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-v2", &r, &d);
    let mut guard = Guard::default();
    let mut own = spawn_own_group("sleep", "60");
    let stranger = own.id();
    guard.add(stranger);
    let recorded = real_start_ms(stranger).saturating_sub(3_600_000);
    plant_process(&d, "t-v2", "a", stranger, recorded);
    let (s2, p2, m2) = start("t-v2", "b", &r, &d, None, &["sleep", "30"]);
    assert_eq!(code(&s2), 0, "start b: {}", err(&s2));
    guard.add(p2);
    let log = r.join("events.jsonl");

    let st = step_rec("t-v2", &d, &log, &r, 9, 5, 9);
    assert_eq!(code(&st), 6, "stdout {:?} stderr {:?}", out(&st), err(&st));
    let text = fs::read_to_string(&log).unwrap();
    let rel_a = handover_relpath("t-v2", "a", stranger as u64, recorded);
    let line_a = text.lines().find(|l| l.contains(&format!("\"handover\":\"{rel_a}\""))).unwrap_or_else(|| panic!("no death line for a: {text}"));
    assert_eq!(json_str(line_a, "cause"), "died", "the reused pid past the step limit is died, never timeout: {line_a}");
    let hand = fs::read_to_string(handover_path(&d, "t-v2", "a", stranger as u64, recorded)).unwrap();
    assert_eq!(json_str(&hand, "cause"), "died", "the handover says died: {hand}");
    assert!(!text.contains("\"cause\":\"timeout\"") || !text.contains(&rel_a) || !line_a.contains("timeout"), "a timeout line for the reused life exists: {text}");
    let hand_b = fs::read_to_string(handover_path(&d, "t-v2", "b", p2 as u64, m2)).unwrap();
    assert_eq!(json_str(&hand_b, "cause"), "timeout", "the live entry beside it is timeout: {hand_b}");
    assert!(alive(stranger), "the stranger was signalled");
    let _ = own.kill();
    let _ = own.wait();
}

/// Held-out v4: a regular, valid, same-life handover already in the folder is REUSED: its bytes are
/// unchanged, its cause wins, exactly one `timed_out` line is appended, rc 5 as before.
#[test]
fn held_out_valid_existing_handover_is_reused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-v4", &r, &d);
    let mut guard = Guard::default();
    let (s, pid, ms) = start("t-v4", "a", &r, &d, Some(1_000), &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);
    let log = r.join("events.jsonl");
    let f = deaths_dir(&d, "t-v4");
    fs::create_dir_all(&f).unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o700)).unwrap();
    let planted = valid_handover_line("t-v4", "a", pid as u64, ms, "timeout", "time");
    fs::write(handover_path(&d, "t-v4", "a", pid as u64, ms), planted.as_bytes()).unwrap();

    let c = check_rec("t-v4", &d, &log, &r);
    assert_eq!(code(&c), 5, "the death still resolves to 5: stdout {:?} stderr {:?}", out(&c), err(&c));
    let rel = handover_relpath("t-v4", "a", pid as u64, ms);
    assert_eq!(out(&c), format!("a\t{pid}\ttimed_out\nrecorded\ta\ttimeout\t{rel}\n"), "stdout is the verb's line then the recorded line");
    assert_eq!(fs::read_to_string(handover_path(&d, "t-v4", "a", pid as u64, ms)).unwrap(), planted, "the existing handover was rewritten");
    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 1);
    assert_eq!(json_str(&lines[0], "cause"), "timeout", "the reused handover's cause wins: {}", lines[0]);
    let l = run(&["manifest", "list", "t-v4", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).lines().any(|x| x.starts_with("a\t")), "the entry is off the sheet");
}

/// Held-out v5: check and stop on a gone entry keep today's exact bytes — `died`, the recorded line on
/// stdout, stop's already-gone stderr line verbatim — with no new line and no changed line.
#[test]
fn held_out_gone_entry_check_and_stop_exact_bytes() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-v5", &r, &d);
    let mut guard = Guard::default();
    let (s1, p1, m1) = start("t-v5", "a", &r, &d, None, &["/bin/sh", "-c", "exit 0"]);
    assert_eq!(code(&s1), 0, "start a: {}", err(&s1));
    guard.add(p1);
    assert!(poll(10_000, || !alive(p1)), "a never exited");
    let (s2, p2, m2) = start("t-v5", "b", &r, &d, None, &["/bin/sh", "-c", "exit 0"]);
    assert_eq!(code(&s2), 0, "start b: {}", err(&s2));
    guard.add(p2);
    assert!(poll(10_000, || !alive(p2)), "b never exited");
    let log = r.join("events.jsonl");

    // stop first (it targets one id), then check (it visits every live entry left).
    let st = stop_rec("t-v5", "b", &d, &log, &r);
    assert_eq!(code(&st), 3);
    assert_eq!(out(&st), format!("recorded\tb\tdied\t{}\n", handover_relpath("t-v5", "b", p2 as u64, m2)), "stop stdout is byte-exact");
    assert_eq!(err(&st), format!("harness: b is already-gone: pid {p2} is provably no such process; nothing signalled, entries removed\n"), "stop stderr is byte-exact");

    let c = check_rec("t-v5", &d, &log, &r);
    assert_eq!(code(&c), 5);
    assert_eq!(out(&c), format!("recorded\ta\tdied\t{}\n", handover_relpath("t-v5", "a", p1 as u64, m1)), "check stdout is byte-exact");
    assert_eq!(err(&c), format!("harness: a is already-gone: pid {p1} is provably no such process; nothing signalled, entries removed\n"), "check stderr is stop's line verbatim, byte-exact");

    let lines = timed_out_lines(&log);
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| json_str(l, "cause") == "died"), "both are died: {lines:?}");
}
