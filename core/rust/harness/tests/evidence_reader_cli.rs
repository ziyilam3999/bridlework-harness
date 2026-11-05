//! Reader tests for `harness evidence show|check` (P1B-09 / #214): the Done link over a ticket's
//! runtime evidence. Every check observes behaviour from OUTSIDE (the real binary, real files, the
//! real manifest, runs planted by real `process start` calls) and every bad condition is CARRIED by
//! the fixture (an over-cap record, a symlinked record or run dir, a directory-mode break, a swapped
//! key, a FIFO): an ABSENT `exit` still means no events, never success. No captured byte is ever
//! expected on stdout or stderr. No test reads env vars; the binary is found through
//! `CARGO_BIN_EXE_harness`. No wall-clock assert measures the TEST's own span: every bounded wait is
//! a poll on observable state (a landed record, a returned child), never an elapsed-time assert.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("evrd-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// A scratch layout: `R` is the ticket's root, `O` a sibling folder outside every root, `D` holds the manifest.
    fn fence(&self) -> (PathBuf, PathBuf, PathBuf) {
        let r = self.0.join("R");
        let o = self.0.join("O");
        let d = self.0.join("D");
        fs::create_dir_all(&r).unwrap();
        fs::create_dir_all(&o).unwrap();
        fs::create_dir_all(&d).unwrap();
        (r.canonicalize().unwrap(), o, d.canonicalize().unwrap())
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

/// Call `f` every 25 ms until it answers true or `ms` runs out; the answer when the budget ran out.
fn poll(ms: u64, f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut f = f;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    f()
}

/// Wait the whole bounded span (the negative waits: proving a file does NOT land needs the full
/// budget, there is nothing to poll for). No assert ever reads the clock's answer.
fn hold(ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
}

/// Run a command that must RETURN within `ms` (a FIFO at the `exit` name would hang a reader that
/// opens before judging the type): past the bound the child is killed and the assert fires.
fn run_bounded(args: &[&str], ms: u64) -> Output {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        assert!(Instant::now() < deadline, "the command did not return within {ms} ms: {args:?}");
        thread::sleep(Duration::from_millis(25));
    }
}

/// Kills the whole process group of every registered pid when dropped (TERM, then KILL), so a failed
/// test run leaves nothing behind. Only pids spawned in their OWN group may be registered.
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
                .arg(format!("-{sig}"))
                .arg("--")
                .arg(format!("-{pid}"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn init_manifest(ticket: &str, r: &Path, d: &Path) {
    let i = run(&["manifest", "init", ticket, "--dir", d.to_str().unwrap(), "--root", r.to_str().unwrap()]);
    assert_eq!(code(&i), 0, "init: {}", err(&i));
}

/// Run `process start` for `prog` under `ticket`, id `id`, cwd `cwd`; returns (output, program pid).
fn start(ticket: &str, id: &str, cwd: &Path, d: &Path, prog: &[&str]) -> (Output, u32) {
    let mut v: Vec<String> = vec![
        "process".into(),
        "start".into(),
        ticket.into(),
        "--id".into(),
        id.into(),
        "--cwd".into(),
        cwd.to_string_lossy().into_owned(),
        "--dir".into(),
        d.to_string_lossy().into_owned(),
        "--".into(),
    ];
    for a in prog {
        v.push((*a).into());
    }
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    let o = run(&args);
    let pid = out(&o).split('\t').nth(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    (o, pid)
}

/// The run dir of one started id, located through the PUBLIC `manifest list` (the evidence entry's
/// stored path, so the test never re-derives the layout itself).
fn run_dir_of(d: &Path, ticket: &str, id: &str) -> PathBuf {
    let l = run(&["manifest", "list", ticket, "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0, "list: {}", err(&l));
    let listed = out(&l);
    let line = listed
        .lines()
        .find(|x| x.starts_with(&format!("{id}.")) && x.ends_with("/exit"))
        .unwrap_or_else(|| panic!("the evidence exit entry of {id} is not listed: {listed}"));
    let path = line.split('\t').nth(2).expect("an evidence line carries a path");
    PathBuf::from(path).parent().expect("the evidence path has a run dir parent").to_path_buf()
}

/// Poll until the run dir's `exit` record has landed (it is written by the waiter, asynchronously).
fn wait_exit(rd: &Path, ms: u64) {
    assert!(poll(ms, || rd.join("exit").exists()), "no exit record within {ms} ms in {}", rd.display());
}

fn show(ticket: &str, d: &Path) -> Output {
    run(&["evidence", "show", ticket, "--manifest-dir", d.to_str().unwrap()])
}

fn check(ticket: &str, d: &Path) -> Output {
    run(&["evidence", "check", ticket, "--manifest-dir", d.to_str().unwrap()])
}

/// The tab-separated fields of one `show` run line.
fn fields(line: &str) -> Vec<&str> {
    line.split('\t').collect()
}

/// The space-separated words of one `refused` line (the check verb's own shape).
fn words(line: &str) -> Vec<&str> {
    line.split(' ').collect()
}

/// Start a program and wait for its record; returns (root, manifest dir, run dir).
fn plant_run(t: &Tmp, ticket: &str, id: &str, prog: &[&str]) -> (PathBuf, PathBuf, PathBuf) {
    let (r, _o, d) = t.fence();
    init_manifest(ticket, &r, &d);
    let (s, pid) = start(ticket, id, &r, &d, prog);
    assert_eq!(code(&s), 0, "start: {} (stderr {:?})", out(&s), err(&s));
    assert!(pid > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    let rd = run_dir_of(&d, ticket, id);
    wait_exit(&rd, 5000);
    (r, d, rd)
}

/// A chmod that keeps the file readable (the tests run as the owner).
fn chmod(p: &Path, mode: u32) {
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

/// One `show` line must exist for `id`, with state and reason at their fixed positions; returns it.
fn run_line<'a>(o: &'a str, id: &str) -> &'a str {
    o.lines()
        .find(|l| l.starts_with("run\t") && fields(l).get(1) == Some(&id))
        .unwrap_or_else(|| panic!("no run line for {id} in {o:?}"))
}

/// Neither stdout nor stderr of a reader call may carry any path at all (folder-free output).
fn assert_folder_free(o: &Output) {
    for (name, text) in [("stdout", out(o).as_str()), ("stderr", err(o).as_str())] {
        assert!(!text.contains('/'), "{name} carries a path: {text:?}");
    }
}

// ---------------------------------------------------------------- AC 2

/// AC 2: a ticket that never started a program has ZERO runs and Done is ALLOWED (most tickets never
/// start one; whether one was NEEDED is the gate's business). `show` prints only the summary, every
/// count 0. Mutant "refuse on zero runs" turns this red.
#[test]
fn zero_runs_allowed() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("evr-2", &r, &d);
    let c = check("evr-2", &d);
    assert_eq!(code(&c), 0, "check: {} {:?}", out(&c), err(&c));
    assert_eq!(out(&c), "evidence=allowed ticket=evr-2 runs=0\n", "the allow line is exact");
    let s = show("evr-2", &d);
    assert_eq!(code(&s), 0);
    assert_eq!(
        out(&s),
        "evidence ticket=evr-2 runs=0 ok=0 stopped=0 attempt=0 failed=0 running=0 unknown=0 damaged=0 other=0\n",
        "zero runs print only the summary, every count 0"
    );
}

// ---------------------------------------------------------------- AC 3

/// AC 3: an ok run prints the exact 14-field tab-separated line (`-` where unknown, numbers from the
/// parsed record) and allows Done. Mutant "read `code` from the `signal` key" turns this red.
#[test]
fn ok_run_shows_and_allows() {
    let t = Tmp::new();
    let (_r, d, _rd) = plant_run(&t, "evr-3", "a", &["/bin/sh", "-c", "printf hi; exit 0"]);
    let s = show("evr-3", &d);
    assert_eq!(code(&s), 0, "show: {} {:?}", out(&s), err(&s));
    let text = out(&s);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "one run line plus the summary: {:?}", out(&s));
    let f = fields(lines[0]);
    assert_eq!(f.len(), 14, "the run line has exactly the 14 fields: {:?}", lines[0]);
    assert_eq!(&f[0..4], &["run", "a", f[2], "ok"]);
    assert!(f[2].bytes().all(|b| b.is_ascii_digit()) && !f[2].is_empty(), "the stamp is digits: {}", f[2]);
    assert_eq!(f[4], "-");
    assert_eq!(&f[5..], &["exited", "0", "0", "true", f[9], "2", "0", "false", "false"]);
    assert!(f[9].bytes().all(|b| b.is_ascii_digit()) && !f[9].is_empty(), "duration_ms is a number: {}", f[9]);
    assert_eq!(lines[1], "evidence ticket=evr-3 runs=1 ok=1 stopped=0 attempt=0 failed=0 running=0 unknown=0 damaged=0 other=0");
    let c = check("evr-3", &d);
    assert_eq!(code(&c), 0, "check: {} {:?}", out(&c), err(&c));
    assert_eq!(out(&c), "evidence=allowed ticket=evr-3 runs=1\n");
}

// ---------------------------------------------------------------- AC 4

/// AC 4: a crashed run REFUSES Done: exit 1, a `refused` line, the refused summary, and exactly one
/// `harness: error:` stderr line. Mutant "treat any `exited` as ok" turns this red.
#[test]
fn failed_run_refuses_done() {
    let t = Tmp::new();
    let (_r, d, _rd) = plant_run(&t, "evr-4", "a", &["/bin/sh", "-c", "exit 7"]);
    let c = check("evr-4", &d);
    assert_eq!(code(&c), 1, "check: {} {:?}", out(&c), err(&c));
    let text = out(&c);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "one refused line plus the summary: {:?}", out(&c));
    assert_eq!(&words(lines[0])[0..4], &["refused", "a", words(lines[0])[2], "failed"], "the refused line: {:?}", lines[0]);
    assert_eq!(lines[1], "evidence=refused ticket=evr-4 blocking=1");
    assert_eq!(lines.last().unwrap(), &"evidence=refused ticket=evr-4 blocking=1");
    assert!(out(&c).ends_with("evidence=refused ticket=evr-4 blocking=1\n"), "stdout ends with the summary");
    assert_eq!(err(&c).lines().count(), 1, "exactly one stderr line: {:?}", err(&c));
    assert!(
        err(&c).starts_with("harness: error: Done refused by evidence: 1 run(s)"),
        "the stderr line: {:?}",
        err(&c)
    );
    let s = show("evr-4", &d);
    let text = out(&s);
    assert_eq!(fields(run_line(&text, "a"))[3], "failed", "show names the failed state");
}

// ---------------------------------------------------------------- AC 5

/// AC 5: a harness stop (TERM, or KILL) ends a default-handling program as `stopped`, and Done stays
/// allowed. Mutant "treat every `signalled` as failed" turns this red.
#[test]
fn harness_stop_is_allowed() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("evr-5", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("evr-5", "a", &r, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let st = run(&["process", "stop", "evr-5", "--id", "a", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&st), 0, "stop answers 0: {:?} {:?}", out(&st), err(&st));
    let rd = run_dir_of(&d, "evr-5", "a");
    wait_exit(&rd, 5000);
    let s = show("evr-5", &d);
    assert_eq!(code(&s), 0);
    let text = out(&s);
    let line = run_line(&text, "a");
    let f = fields(line);
    assert_eq!(f[3], "stopped", "a harness stop reads stopped: {line}");
    assert!(f[7] == "15" || f[7] == "9", "the signal is the TERM (15) or the KILL (9): {}", f[7]);
    let c = check("evr-5", &d);
    assert_eq!(code(&c), 0, "a stopped run allows Done: {} {:?}", out(&c), err(&c));
}

// ---------------------------------------------------------------- AC 6

/// AC 6: a crash by a signal the harness never sends (USR1) reads `failed` and refuses Done. (The
/// signal number itself is platform-shaped — 10 on Linux, 30 on the BSDs — so the assert is "not a
/// harness stop signal".)
/// Mutant "allow every signal" turns this red.
#[test]
fn crash_by_another_signal_refuses() {
    let t = Tmp::new();
    let (_r, d, _rd) = plant_run(&t, "evr-6", "a", &["/bin/sh", "-c", "kill -USR1 $$"]);
    let s = show("evr-6", &d);
    let text = out(&s);
    let line = run_line(&text, "a");
    let f = fields(line);
    assert_eq!(f[3], "failed", "USR1 is not a harness stop: {line}");
    assert!(f[7] != "15" && f[7] != "9", "the signal is one the harness never sends: {}", f[7]);
    let c = check("evr-6", &d);
    assert_eq!(code(&c), 1, "a signal crash refuses Done: {:?} {:?}", out(&c), err(&c));
}

// ---------------------------------------------------------------- AC 7

/// AC 7: a run with no record yet whose process entry is LIVE reads `running` and refuses Done (the
/// test then stops it). Mutant "no `exit` with a live process = ok" turns this red.
#[test]
fn running_refuses_done() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("evr-7", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("evr-7", "a", &r, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let s = show("evr-7", &d);
    assert_eq!(code(&s), 0);
    let text = out(&s);
    let f = fields(run_line(&text, "a"));
    assert_eq!(f[3], "running", "a live process with no record yet reads running");
    assert_eq!(f[4], "-", "no reason");
    let c = check("evr-7", &d);
    assert_eq!(code(&c), 1, "a running program refuses Done: {:?} {:?}", out(&c), err(&c));
    let st = run(&["process", "stop", "evr-7", "--id", "a", "--dir", d.to_str().unwrap()]);
    assert!(code(&st) == 0 || code(&st) == 3, "cleanup stop: {:?} {:?}", out(&st), err(&st));
}

// ---------------------------------------------------------------- AC 8

/// AC 8: the waiter SIGKILLed leaves NO record; without a live process entry that is `unknown` —
/// unknown is NEVER success — and Done is refused. Mutant "missing `exit` without a live process =
/// ok" turns this red.
#[test]
fn unknown_refuses_never_success() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("evr-8", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("evr-8", "a", &r, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    // the waiter is the program's parent; SIGKILL it (the named P2 case: no record can ever land)
    let ps = Command::new("ps").arg("-o").arg("ppid=").arg("-p").arg(pid.to_string()).stdin(Stdio::null()).output().unwrap();
    let waiter: u32 = String::from_utf8_lossy(&ps.stdout).trim().parse().expect("ps names the parent");
    assert!(waiter > 1 && waiter != pid, "the waiter pid is plausible: {waiter}");
    let k = Command::new("kill").arg("-9").arg("--").arg(waiter.to_string()).status().unwrap();
    assert!(k.success(), "the waiter was killed");
    // the program itself is stopped through the sheet, exactly as the driver would
    let st = run(&["process", "stop", "evr-8", "--id", "a", "--dir", d.to_str().unwrap()]);
    assert!(code(&st) == 0 || code(&st) == 3, "stop after the waiter's death: {:?} {:?}", out(&st), err(&st));
    let rd = run_dir_of(&d, "evr-8", "a");
    hold(5000);
    assert!(!rd.join("exit").exists(), "no record lands for a killed waiter");
    let s = show("evr-8", &d);
    let text = out(&s);
    let f = fields(run_line(&text, "a"));
    assert_eq!(f[3], "unknown", "no record and no live process reads unknown");
    let c = check("evr-8", &d);
    assert_eq!(code(&c), 1, "unknown refuses Done: {:?} {:?}", out(&c), err(&c));
}

// ---------------------------------------------------------------- AC 9

/// AC 9: removing the manifest's evidence lines does NOT hide the crash (the FULL append log is the
/// truth), and moving the run dir out of the root turns the same run `unknown` — still refused.
/// Mutant "judge only `live` entries" turns this red.
#[test]
fn removing_the_sheet_line_does_not_hide_a_crash() {
    let t = Tmp::new();
    let (r, o, d) = t.fence();
    init_manifest("evr-9", &r, &d);
    let (s, pid) = start("evr-9", "a", &r, &d, &["/bin/sh", "-c", "exit 7"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    let _ = pid;
    let rd = run_dir_of(&d, "evr-9", "a");
    wait_exit(&rd, 5000);
    let c = check("evr-9", &d);
    assert_eq!(code(&c), 1, "the crash refuses before any removal");
    // cross the three evidence ids off the sheet, one `manifest remove` each
    let l = run(&["manifest", "list", "evr-9", "--dir", d.to_str().unwrap()]);
    for line in out(&l).lines().filter(|x| x.starts_with("a.")) {
        let id = line.split('\t').next().unwrap();
        let rm = run(&["manifest", "remove", "evr-9", "--id", id, "--dir", d.to_str().unwrap()]);
        assert_eq!(code(&rm), 0, "remove {id}: {}", err(&rm));
    }
    let c = check("evr-9", &d);
    assert_eq!(code(&c), 1, "a removed line does not erase what happened: {:?} {:?}", out(&c), err(&c));
    assert_eq!(words(out(&c).lines().next().unwrap())[3], "failed", "the run is still judged failed");
    // and the files gone: the same run reads unknown — never success
    fs::rename(&rd, o.join("moved-out")).unwrap();
    let c = check("evr-9", &d);
    assert_eq!(code(&c), 1, "unknown refuses Done");
    assert_eq!(words(out(&c).lines().next().unwrap())[3], "unknown", "a moved run dir reads unknown");
}

// ---------------------------------------------------------------- AC 10

/// The common AC 10 assert: `check` refuses (exit 1) naming `damaged <reason>`, returns within the
/// bound (a FIFO never hangs it), and prints no path anywhere.
fn assert_damaged(ticket: &str, d: &Path, reason: &str) {
    let c = run_bounded(&["evidence", "check", ticket, "--manifest-dir", d.to_str().unwrap()], 5000);
    assert_eq!(code(&c), 1, "damaged refuses Done: {:?} {:?}", out(&c), err(&c));
    let cout = out(&c);
    let first = cout.lines().next().unwrap_or_else(|| panic!("no refused line: {cout:?}"));
    assert_eq!(words(first)[3], "damaged", "the state is damaged: {first}");
    assert_eq!(words(first)[4], reason, "the reason token is {reason}: {first}");
    assert_eq!(
        out(&c).lines().last().unwrap(),
        &format!("evidence=refused ticket={ticket} blocking=1"),
        "the refused summary"
    );
    assert_eq!(err(&c).lines().count(), 1);
    assert_folder_free(&c);
    let s = show(ticket, d);
    assert_eq!(code(&s), 0);
    let text = out(&s);
    let line = run_line(&text, "a");
    assert_eq!(fields(line)[3], "damaged", "show agrees: {line}");
    assert_eq!(fields(line)[4], reason, "show names the reason: {line}");
    assert_folder_free(&s);
}

/// AC 10(a): `exit` replaced by a symlink to a valid record copy -> `symlink`. Mutant "drop the
/// symlink check on the record" turns this red.
#[test]
fn damaged_symlinked_record() {
    let t = Tmp::new();
    let (_r, o, d, rd) = {
        let (r, o, d) = t.fence();
        init_manifest("evr-10a", &r, &d);
        let (s, pid) = start("evr-10a", "a", &r, &d, &["/bin/sh", "-c", "printf hi; exit 0"]);
        assert_eq!(code(&s), 0);
        let _ = pid;
        let rd = run_dir_of(&d, "evr-10a", "a");
        wait_exit(&rd, 5000);
        (r, o, d, rd)
    };
    let copy = o.join("record-copy");
    fs::copy(rd.join("exit"), &copy).unwrap();
    chmod(&copy, 0o600);
    fs::remove_file(rd.join("exit")).unwrap();
    std::os::unix::fs::symlink(&copy, rd.join("exit")).unwrap();
    assert_damaged("evr-10a", &d, "symlink");
}

/// AC 10(b): `exit` = 5000 bytes -> `over-cap`, never read past the cap. Mutant "drop the size
/// check" turns this red.
#[test]
fn damaged_over_cap_record() {
    let t = Tmp::new();
    let (_r, _o, d, rd) = fresh_ok_run_named(&t, "evr-10b");
    fs::write(rd.join("exit"), vec![b'x'; 5000]).unwrap();
    chmod(&rd.join("exit"), 0o600);
    assert_damaged("evr-10b", &d, "over-cap");
}

/// AC 10(c): `exit` chmod 0644 -> `bad-mode`. Mutant "drop the mode check" turns this red.
#[test]
fn damaged_bad_mode_record() {
    let t = Tmp::new();
    let (_r, _o, d, rd) = fresh_ok_run_named(&t, "evr-10c");
    chmod(&rd.join("exit"), 0o644);
    assert_damaged("evr-10c", &d, "bad-mode");
}

/// AC 10(d): `exit` with two keys swapped -> `malformed` (the parser is the encoder's strict
/// inverse). Mutant "parse leniently" turns this red.
#[test]
fn damaged_swapped_keys_record() {
    let t = Tmp::new();
    let (_r, _o, d, rd) = fresh_ok_run_named(&t, "evr-10d");
    let text = fs::read_to_string(rd.join("exit")).unwrap();
    let swapped = text.replacen("\"code\":0,\"signal\":0", "\"signal\":0,\"code\":0", 1);
    assert_ne!(swapped, text, "the fixture really swapped two keys");
    fs::write(rd.join("exit"), swapped).unwrap();
    chmod(&rd.join("exit"), 0o600);
    assert_damaged("evr-10d", &d, "malformed");
}

/// AC 10(e): `exit` with `"v":2` -> `malformed`. Mutant "accept any v" turns this red.
#[test]
fn damaged_v2_record() {
    let t = Tmp::new();
    let (_r, _o, d, rd) = fresh_ok_run_named(&t, "evr-10e");
    let text = fs::read_to_string(rd.join("exit")).unwrap();
    fs::write(rd.join("exit"), text.replace("\"v\":1", "\"v\":2")).unwrap();
    chmod(&rd.join("exit"), 0o600);
    assert_damaged("evr-10e", &d, "malformed");
}

/// AC 10(f): `exit` replaced by a FIFO -> `not-regular`, and the reader RETURNS (the type is judged
/// before any open; a reader that opened the FIFO would hang and this test kills it red).
/// Mutant "drop the not-regular check" turns this red (by hanging).
#[test]
fn damaged_fifo_record() {
    let t = Tmp::new();
    let (_r, _o, d, rd) = fresh_ok_run_named(&t, "evr-10f");
    fs::remove_file(rd.join("exit")).unwrap();
    let mk = Command::new("mkfifo").arg(rd.join("exit").to_str().unwrap()).status().unwrap();
    assert!(mk.success(), "mkfifo made the fixture");
    chmod(&rd.join("exit"), 0o600);
    assert_damaged("evr-10f", &d, "not-regular");
}

/// AC 10(g): the run dir replaced by a symlink to a copy -> `symlink`. Mutant "drop the run-dir
/// check" turns this red.
#[test]
fn damaged_symlinked_run_dir() {
    let t = Tmp::new();
    let (r, o, d) = t.fence();
    init_manifest("evr-10g", &r, &d);
    let (s, _pid) = start("evr-10g", "a", &r, &d, &["/bin/sh", "-c", "printf hi; exit 0"]);
    assert_eq!(code(&s), 0);
    let rd = run_dir_of(&d, "evr-10g", "a");
    wait_exit(&rd, 5000);
    let moved = o.join("run-copy");
    fs::rename(&rd, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &rd).unwrap();
    assert_damaged("evr-10g", &d, "symlink");
}

/// AC 10(h): `stdout` deleted -> `missing-stream` (a whole record must still have both streams).
/// Mutant "drop the stream checks" turns this red.
#[test]
fn damaged_missing_stream() {
    let t = Tmp::new();
    let (_r, _o, d, rd) = fresh_ok_run_named(&t, "evr-10h");
    fs::remove_file(rd.join("stdout")).unwrap();
    assert_damaged("evr-10h", &d, "missing-stream");
}

/// AC 10(i): `evidence/proc` chmod 0755 -> `bad-mode` (the dir chain is checked, never trusted).
/// Mutant "drop the dir-chain check" turns this red.
#[test]
fn damaged_proc_dir_mode() {
    let t = Tmp::new();
    let (_r, _o, d, rd) = fresh_ok_run_named(&t, "evr-10i");
    chmod(rd.parent().unwrap(), 0o755);
    assert_damaged("evr-10i", &d, "bad-mode");
    chmod(rd.parent().unwrap(), 0o700);
}

/// AC 10 fixture helper: a fresh ok run, named, returning (root, outside, dir, run dir).
fn fresh_ok_run_named(t: &Tmp, ticket: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let (r, o, d) = t.fence();
    init_manifest(ticket, &r, &d);
    let (s, pid) = start(ticket, "a", &r, &d, &["/bin/sh", "-c", "printf hi; exit 0"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    let _ = pid;
    let rd = run_dir_of(&d, ticket, "a");
    wait_exit(&rd, 5000);
    let c = check(ticket, &d);
    assert_eq!(code(&c), 0, "the pristine run allows Done: {:?} {:?}", out(&c), err(&c));
    (r, o, d, rd)
}

// ---------------------------------------------------------------- AC 11

/// AC 11: a start whose program could not spawn answers 2 and leaves an ATTEMPT record
/// (`spawn_failed`, `committed:false`): counted, never judged; Done stays allowed. Mutant "refuse
/// `committed:false`" turns this red.
#[test]
fn attempt_is_allowed() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("evr-11", &r, &d);
    let (s, _pid) = start("evr-11", "a", &r, &d, &["/nonexistent-harness-prog"]);
    assert_eq!(code(&s), 2, "the refused start answers 2: {:?} {:?}", out(&s), err(&s));
    let rd = run_dir_of(&d, "evr-11", "a");
    wait_exit(&rd, 5000);
    let s = show("evr-11", &d);
    assert_eq!(code(&s), 0);
    let text = out(&s);
    let f = fields(run_line(&text, "a"));
    assert_eq!(f[3], "attempt", "a refused start is an attempt, not a failure");
    assert_eq!(f[5], "spawn_failed");
    assert_eq!(f[8], "false", "start never commits a spawn it refused");
    let c = check("evr-11", &d);
    assert_eq!(code(&c), 0, "an attempt allows Done: {:?} {:?}", out(&c), err(&c));
    assert_eq!(out(&c), "evidence=allowed ticket=evr-11 runs=1\n");
}

// ---------------------------------------------------------------- AC 12

/// AC 12: NO captured byte is ever echoed — a program that prints a marker to both streams leaves
/// the marker only in the private files, never in `show` or `check` output. Mutant "print the last
/// stdout line on a refused run" turns this red.
#[test]
fn no_captured_byte_is_echoed() {
    let t = Tmp::new();
    let (_r, d, _rd) = plant_run(&t, "evr-12", "a", &["/bin/sh", "-c", "printf EVMARK-214-synthetic; printf EVMARK-214-synthetic >&2; exit 3"]);
    for o in [show("evr-12", &d), check("evr-12", &d)] {
        assert!(!out(&o).contains("EVMARK"), "stdout echoes a captured byte: {:?}", out(&o));
        assert!(!err(&o).contains("EVMARK"), "stderr echoes a captured byte: {:?}", err(&o));
    }
    let c = check("evr-12", &d);
    assert_eq!(code(&c), 1, "the run itself still refuses Done");
}

// ---------------------------------------------------------------- AC 13

/// AC 13: cannot check is exit 2 for BOTH verbs, stdout empty, exactly one folder-free
/// `harness: error:` stderr line — a missing manifest is NEVER zero runs. Mutant "missing manifest =
/// zero runs" turns this red.
#[test]
fn cannot_check_is_exit_2_never_allow() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("evr-13", &r, &d);
    // a torn tail and a corrupt line, each on its own ticket's manifest
    init_manifest("evr-13t", &r, &d);
    let torn = d.join("evr-13t.jsonl");
    let mut f = fs::OpenOptions::new().append(true).open(&torn).unwrap();
    use std::io::Write as _;
    f.write_all(b"{\"v\":1").unwrap(); // no trailing newline: a torn tail
    init_manifest("evr-13c", &r, &d);
    let corrupt = d.join("evr-13c.jsonl");
    let mut f = fs::OpenOptions::new().append(true).open(&corrupt).unwrap();
    f.write_all(b"garbage\n").unwrap(); // a complete line that is no manifest line
    let ds = d.to_str().unwrap();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("manifest missing", vec!["evr-13z", "--manifest-dir", ds]),
        ("torn tail", vec!["evr-13t", "--manifest-dir", ds]),
        ("corrupt", vec!["evr-13c", "--manifest-dir", ds]),
        ("relative manifest dir", vec!["evr-13", "--manifest-dir", "relative/dir"]),
        ("dotdot manifest dir", vec!["evr-13", "--manifest-dir", "/somewhere/../x"]),
        ("unknown flag", vec!["evr-13", "--manifest-dir", ds, "--wat"]),
        ("no ticket", vec!["--manifest-dir", ds]),
        ("non-token ticket", vec!["BAD-TICKET", "--manifest-dir", ds]),
    ];
    for (name, case) in cases {
        for verb in ["show", "check"] {
            let mut args = vec!["evidence", verb];
            args.extend(case.iter());
            let o = run(&args);
            assert_eq!(code(&o), 2, "{name} ({verb}) refuses with 2: {:?} {:?}", out(&o), err(&o));
            assert_eq!(out(&o), "", "{name} ({verb}) prints nothing on stdout");
            let e = err(&o);
            let lines: Vec<&str> = e.lines().collect();
            assert_eq!(lines.len(), 1, "{name} ({verb}) prints exactly one stderr line: {:?}", err(&o));
            assert!(lines[0].starts_with("harness: error:"), "{name} ({verb}): {:?}", lines[0]);
            assert!(!lines[0].contains('/'), "{name} ({verb}) names a path: {:?}", lines[0]);
        }
    }
}

// ---------------------------------------------------------------- AC 14

/// AC 14: any OTHER `evidence` entry (a screenshot added by hand) is counted `other`, never judged;
/// it changes nothing about Done. Mutant "count `other` as unknown" turns this red.
#[test]
fn other_evidence_is_counted_not_judged() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("evr-14", &r, &d);
    let a = run(&[
        "manifest",
        "add",
        "evr-14",
        "--class",
        "evidence",
        "--id",
        "shot1",
        "--path",
        r.join("shot.png").to_str().unwrap(),
        "--dir",
        d.to_str().unwrap(),
    ]);
    assert_eq!(code(&a), 0, "the hand-added screenshot entry lands: {}", err(&a));
    let (s, pid) = start("evr-14", "a", &r, &d, &["/bin/sh", "-c", "printf hi; exit 0"]);
    assert_eq!(code(&s), 0);
    let _ = pid;
    let rd = run_dir_of(&d, "evr-14", "a");
    wait_exit(&rd, 5000);
    let s = show("evr-14", &d);
    assert_eq!(code(&s), 0);
    assert!(out(&s).contains("other=1"), "the screenshot is counted: {:?}", out(&s));
    assert!(out(&s).contains("unknown=0"), "the screenshot is never judged: {:?}", out(&s));
    assert_eq!(out(&s).lines().count(), 2, "only the run line and the summary");
    let c = check("evr-14", &d);
    assert_eq!(code(&c), 0, "other evidence does not block Done: {:?} {:?}", out(&c), err(&c));
}

// ---------------------------------------------------------------- AC 15

/// A no-follow snapshot of a tree: every name with its size, mode and mtimes — read-only means
/// byte-identical AND untouched (a write-and-restore would change the mtime).
fn snap(p: &Path) -> Vec<(String, u64, u32, i64, i64)> {
    fn walk(p: &Path, rel: String, out: &mut Vec<(String, u64, u32, i64, i64)>) {
        let mut names: Vec<String> = fs::read_dir(p).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for n in names {
            let at = p.join(&n);
            let m = fs::symlink_metadata(&at).unwrap();
            let r = if rel.is_empty() { n.clone() } else { format!("{rel}/{n}") };
            out.push((r, m.len(), m.mode(), m.mtime(), m.mtime_nsec()));
            if m.is_dir() {
                walk(&at, format!("{rel}/{n}"), out);
            }
        }
    }
    let mut v = Vec::new();
    walk(p, String::new(), &mut v);
    v
}

/// AC 15: `show` and `check` are read-only — around an ok run, a failed run and an over-cap run,
/// the manifest bytes, a no-follow snapshot of `evidence`, and a snapshot of the whole manifest dir
/// (the supervisor lock file included: not made, not moved, not touched) are byte-identical before
/// and after. Mutant "open `exit` with write access and touch it" (the
/// mtime moves even when the bytes are rewritten verbatim) turns this red.
#[test]
fn readers_are_read_only() {
    let t = Tmp::new();
    for (ticket, prog) in [
        ("evr-15a", vec!["/bin/sh", "-c", "printf hi; exit 0"]),
        ("evr-15b", vec!["/bin/sh", "-c", "exit 7"]),
    ] {
        let (_r, d, rd) = plant_run(&t, ticket, "a", &prog);
        let manifest_before = fs::read(d.join(format!("{ticket}.jsonl"))).unwrap();
        let ev_before = snap(rd.parent().unwrap().parent().unwrap());
        let dir_before = snap(&d);
        let s = show(ticket, &d);
        let c = check(ticket, &d);
        assert_eq!(code(&s), 0);
        assert!(code(&c) == 0 || code(&c) == 1);
        assert_eq!(fs::read(d.join(format!("{ticket}.jsonl"))).unwrap(), manifest_before, "the manifest is untouched");
        assert_eq!(snap(rd.parent().unwrap().parent().unwrap()), ev_before, "evidence is untouched (sizes, modes, mtimes)");
        assert_eq!(snap(&d), dir_before, "the manifest dir is untouched: no lock file is made or moved");
    }
    // the over-cap shape too: the oversized record is judged by its metadata, never opened for write
    let (_r, d, rd) = plant_run(&t, "evr-15c", "a", &["/bin/sh", "-c", "printf hi; exit 0"]);
    fs::write(rd.join("exit"), vec![b'x'; 5000]).unwrap();
    chmod(&rd.join("exit"), 0o600);
    let manifest_before = fs::read(d.join("evr-15c.jsonl")).unwrap();
    let ev_before = snap(rd.parent().unwrap().parent().unwrap());
    let dir_before = snap(&d);
    let s = show("evr-15c", &d);
    let c = check("evr-15c", &d);
    assert_eq!(code(&s), 0);
    assert_eq!(code(&c), 1);
    assert_eq!(fs::read(d.join("evr-15c.jsonl")).unwrap(), manifest_before);
    assert_eq!(snap(rd.parent().unwrap().parent().unwrap()), ev_before);
    assert_eq!(snap(&d), dir_before);
}
