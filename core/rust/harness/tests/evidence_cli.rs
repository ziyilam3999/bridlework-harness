//! Runtime-evidence tests for `harness process start` (P1B-08 / #209): stdout, stderr, exit code and
//! timing of every started ticket process are captured into `<root>/evidence/proc/<id>.<ts_ms>/` and
//! signed out as three live `evidence` manifest entries written AHEAD of the spawn, so the files are
//! never unlisted orphans. A hidden waiter (the harness binary itself, in its own group) spawns the
//! program, reports the pid, records the ending and dies with the program; a start that refuses after
//! the spawn closes the commit pipe uncommitted and the waiter kills its program, so a refused start
//! leaves nothing running. Every check observes behaviour from OUTSIDE (the real binary, real files,
//! the real manifest); every wait is a bounded poll on observable state; no wall-clock assert measures
//! the TEST's own elapsed time (the one range check, on the record's `duration_ms` field, observes a
//! value the WAITER produced inside one process between its spawn and its wait, exactly what #209 AC 5
//! names). No test reads env vars; the binary is found through `CARGO_BIN_EXE_harness`.
#![cfg(unix)]
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::{DirBuilderExt, MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("ecli-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

/// Is a process alive at this pid? Asked through the system `kill -0 <pid>` (a TEST-side probe).
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

/// `harness manifest init <ticket>` into dir `d` with the single root `r`.
fn init_manifest(ticket: &str, r: &Path, d: &Path) {
    let i = run(&["manifest", "init", ticket, "--dir", d.to_str().unwrap(), "--root", r.to_str().unwrap()]);
    assert_eq!(code(&i), 0, "init: {}", err(&i));
}

/// Run `process start` for `prog` under ticket `ticket`, id `id`, cwd `cwd`; returns (output, pid).
fn start(ticket: &str, id: &str, cwd: &Path, port: Option<u64>, d: &Path, prog: &[&str]) -> (Output, u32) {
    let mut v: Vec<String> = vec![
        "process".into(),
        "start".into(),
        ticket.into(),
        "--id".into(),
        id.into(),
        "--cwd".into(),
        cwd.to_string_lossy().into_owned(),
    ];
    if let Some(p) = port {
        v.push("--port".into());
        v.push(p.to_string());
    }
    v.push("--dir".into());
    v.push(d.to_string_lossy().into_owned());
    v.push("--".into());
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

/// The `exit` record as ordered (key, value) pairs: one newline-terminated line, compact JSON, fixed
/// key order (#209 decision 10). The order IS the assert: a reader may rely on it.
fn exit_pairs(rd: &Path) -> Vec<(String, String)> {
    let text = fs::read_to_string(rd.join("exit")).expect("the exit record is readable");
    assert!(text.ends_with('\n'), "the exit record is newline-terminated: {text:?}");
    assert_eq!(text.lines().count(), 1, "the exit record is ONE line: {text:?}");
    let body = text.trim_end().trim_start_matches('{').trim_end_matches('}');
    body.split(',')
        .map(|kv| {
            let (k, v) = kv.split_once(':').unwrap_or_else(|| panic!("a broken pair {kv:?} in {text:?}"));
            (k.trim_matches('"').to_string(), v.trim_matches('"').to_string())
        })
        .collect()
}

fn pair<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    &pairs.iter().find(|(k, _)| k == key).unwrap_or_else(|| panic!("no key {key}")).1
}

/// The exit-record key order, exactly decision 10's.
const EXIT_KEYS: [&str; 16] = [
    "v",
    "pid",
    "committed",
    "outcome",
    "code",
    "signal",
    "start_ms",
    "end_ms",
    "duration_ms",
    "stdout_bytes",
    "stderr_bytes",
    "stdout_truncated",
    "stderr_truncated",
    "stdout_error",
    "stderr_error",
    "output_open_after_exit",
];

/// Start the canonical short program (stdout `out`, stderr `err`, exit 7) and wait for its record.
fn plant_short_run(t: &Tmp, ticket: &str, id: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (r, _o, d) = t.fence();
    init_manifest(ticket, &r, &d);
    let (s, pid) = start(ticket, id, &r, None, &d, &["/bin/sh", "-c", "printf out; printf err >&2; exit 7"]);
    assert_eq!(code(&s), 0, "start: {} (stderr {:?})", out(&s), err(&s));
    assert!(pid > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    let rd = run_dir_of(&d, ticket, id);
    wait_exit(&rd, 5000);
    (r, d, rd)
}

// ---------------------------------------------------------------- AC 2

/// AC 2: a started program's three evidence entries are live on the manifest with the exact layout
/// `<root>/evidence/proc/<id>.<ts_ms>/{stdout,stderr,exit}`, and the captured streams are byte-exact.
/// Mutant "send stderr to null" turns this red.
#[test]
fn evidence_files_and_entries_exist() {
    let t = Tmp::new();
    let (r, d, rd) = plant_short_run(&t, "ev-2", "a");
    let expect_dir = r.join("evidence/proc");
    let name = rd.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("a."), "the run dir is named <id>.<ts>: {}", name);
    assert!(name["a.".len()..].bytes().all(|b| b.is_ascii_digit()), "the run dir stamp is digits: {}", name);
    assert!(rd.starts_with(&expect_dir), "the run dir sits under the root's evidence/proc: {}", rd.display());

    let l = run(&["manifest", "list", "ev-2", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    let text = out(&l);
    for stream in ["stdout", "stderr", "exit"] {
        let want = format!("{name}.{stream}\tevidence\t{}", rd.join(stream).display());
        assert!(text.lines().any(|x| x == want), "list lacks the evidence line {:?} in {:?}", want, text);
    }
    assert_eq!(fs::read(rd.join("stdout")).unwrap(), b"out", "stdout is byte-exact");
    assert_eq!(fs::read(rd.join("stderr")).unwrap(), b"err", "stderr is byte-exact");
}

// ---------------------------------------------------------------- AC 3

/// AC 3: the exit record is one newline-terminated line with the keys exactly in decision 10's order;
/// `outcome` exited, `code` 7, `committed` true, the byte counts and truncation/error flags honest; and
/// no temp file is left behind. Mutants "write `code` as 0 always" and "write in place and leave the
/// temp name" each turn this red.
#[test]
fn evidence_exit_record_shape() {
    let t = Tmp::new();
    let (_r, _d, rd) = plant_short_run(&t, "ev-3", "a");
    let pairs = exit_pairs(&rd);
    assert_eq!(pairs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), EXIT_KEYS.to_vec(), "the key order is decision 10's");
    assert_eq!(pair(&pairs, "v"), "1");
    assert_eq!(pair(&pairs, "outcome"), "exited");
    assert_eq!(pair(&pairs, "code"), "7");
    assert_eq!(pair(&pairs, "signal"), "0");
    assert_eq!(pair(&pairs, "committed"), "true");
    assert_eq!(pair(&pairs, "stdout_bytes"), "3");
    assert_eq!(pair(&pairs, "stderr_bytes"), "3");
    assert_eq!(pair(&pairs, "stdout_truncated"), "false");
    assert_eq!(pair(&pairs, "stderr_truncated"), "false");
    assert_eq!(pair(&pairs, "stdout_error"), "none");
    assert_eq!(pair(&pairs, "stderr_error"), "none");
    let (start_ms, end_ms) = (pair(&pairs, "start_ms").parse::<u64>().unwrap(), pair(&pairs, "end_ms").parse::<u64>().unwrap());
    assert!(end_ms >= start_ms, "end_ms is not before start_ms");
    assert!(pair(&pairs, "pid").parse::<u32>().unwrap() > 1, "the record names the program pid");
    let names: Vec<String> = fs::read_dir(&rd).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    let mut want = vec!["stdout".to_string(), "stderr".to_string(), "exit".to_string()];
    want.sort();
    let mut names = names;
    names.sort();
    assert_eq!(names, want, "the run dir holds exactly the three files, no temp name left");
}

// ---------------------------------------------------------------- AC 4

/// AC 4: a program ended by `process stop` gets an exit record naming the signal: `outcome` signalled,
/// `signal` 15 (or 9 when KILL was needed), `code` -1, `committed` true; the stop itself answers 0 as
/// today. Mutant "put the waiter in the program's group" kills the waiter with the program, no exit
/// record ever lands, and this turns red.
#[test]
fn evidence_records_the_stop_signal() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-4", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-4", "w", &r, None, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1);
    guard.add(pid);

    let st = run(&["process", "stop", "ev-4", "--id", "w", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&st), 0, "stop answers 0 as today: stdout {:?} stderr {:?}", out(&st), err(&st));
    let rd = run_dir_of(&d, "ev-4", "w");
    wait_exit(&rd, 5000);
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "outcome"), "signalled");
    let signal = pair(&pairs, "signal").parse::<i32>().unwrap();
    assert!(signal == 15 || signal == 9, "the record names the TERM (15) or the KILL (9): {signal}");
    assert_eq!(pair(&pairs, "code"), "-1");
    assert_eq!(pair(&pairs, "committed"), "true");
    // the evidence entries outlive the process (decision 2): still live after the stop
    let l = run(&["manifest", "list", "ev-4", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(out(&l).lines().any(|x| x.starts_with("w.") && x.ends_with("/exit")), "the evidence entry stays live after the stop");
}

// ---------------------------------------------------------------- AC 5

/// AC 5 (the integration half): `duration_ms` is the waiter's own MONOTONIC span, so a `sleep 1`
/// program records a duration in 900..=5000. This observes the RECORD's field (a value produced
/// inside one process between its spawn and its wait), not the test's own elapsed time; the encoder's
/// unit test (src/evidence.rs) pins that the field is not derived from the wall clock. Mutant
/// "duration_ms = end_ms - start_ms" turns that unit test red.
#[test]
fn evidence_duration_is_the_waiters_span() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-5", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-5", "w", &r, None, &d, &["sleep", "1"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let rd = run_dir_of(&d, "ev-5", "w");
    wait_exit(&rd, 10_000);
    let pairs = exit_pairs(&rd);
    let dur = pair(&pairs, "duration_ms").parse::<u64>().unwrap();
    assert!((900..=5000).contains(&dur), "duration_ms is the waiter's monotonic span of a sleep 1: {dur}");
    assert_eq!(pair(&pairs, "outcome"), "exited");
    assert_eq!(pair(&pairs, "code"), "0");
}

// ---------------------------------------------------------------- AC 6

/// AC 6: a stream past the 4 MiB cap keeps exactly the first 4,194,304 bytes plus the exact 46-byte
/// marker, the program was never blocked (the record lands within 10 s), and the record carries the
/// TOTAL bytes seen and the truncation flag. Mutants "stop reading at the cap" and "no marker" turn
/// this red.
#[test]
fn evidence_caps_the_stream_and_marks_the_cut() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-6", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-6", "w", &r, None, &d, &["/bin/sh", "-c", "head -c 5242880 /dev/zero"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let rd = run_dir_of(&d, "ev-6", "w");
    wait_exit(&rd, 10_000);

    let bytes = fs::read(rd.join("stdout")).unwrap();
    assert_eq!(bytes.len(), 4_194_304 + 46, "stdout is the cap plus the 46-byte marker");
    assert!(bytes[..16] == [0u8; 16], "the first kept bytes are the program's own");
    let marker = b"\n[harness: output truncated at 4194304 bytes]\n";
    assert_eq!(&bytes[4_194_304..], marker, "the cut ends with the exact marker");

    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "stdout_bytes"), "5242880", "the record carries the TOTAL bytes seen");
    assert_eq!(pair(&pairs, "stdout_truncated"), "true");
    assert_eq!(pair(&pairs, "outcome"), "exited");
    assert_eq!(pair(&pairs, "code"), "0");
}

// ---------------------------------------------------------------- AC 7

/// AC 7: binary and non-UTF-8 output is copied as raw bytes, never decoded or rewritten. Mutant
/// "lossy-UTF-8 the bytes" turns this red.
#[test]
fn evidence_keeps_raw_non_utf8_bytes() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-7", &r, &d);
    let (s, _pid) = start("ev-7", "w", &r, None, &d, &["/bin/sh", "-c", "printf '\\000\\377\\376abc\\n'"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    let rd = run_dir_of(&d, "ev-7", "w");
    wait_exit(&rd, 5000);
    assert_eq!(fs::read(rd.join("stdout")).unwrap(), b"\x00\xff\xfeabc\n", "the raw 7 bytes, byte-for-byte");
}

// ---------------------------------------------------------------- AC 8

/// AC 8: `evidence/`, `evidence/proc/` and the run dir are mode 0700; the three files are 0600.
/// Mutant "create files with the default mode" turns this red.
#[test]
fn evidence_dirs_and_files_have_private_modes() {
    let t = Tmp::new();
    let (r, _d, rd) = plant_short_run(&t, "ev-8", "a");
    let mode = |p: &Path| fs::metadata(p).unwrap().mode() & 0o777;
    assert_eq!(mode(&r.join("evidence")), 0o700, "evidence/ is 0700");
    assert_eq!(mode(&r.join("evidence/proc")), 0o700, "evidence/proc/ is 0700");
    assert_eq!(mode(&rd), 0o700, "the run dir is 0700");
    for stream in ["stdout", "stderr", "exit"] {
        assert_eq!(mode(&rd.join(stream)), 0o600, "{stream} is 0600");
    }
}

// ---------------------------------------------------------------- AC 9

/// AC 9: a hostile evidence dir refuses the start FAIL CLOSED — exit 2, the stderr line starts with
/// `harness: error: evidence refused: ` and names the reason, the manifest is byte-identical, the
/// program never ran (its marker is absent), and no absolute path is printed. Four plants: `evidence`
/// a symlink, `evidence/proc` a symlink, `evidence` a regular file, `evidence` mode 0755. Dropping the
/// checked branch turns each plant's half red (the per-plant mutants).
#[test]
fn evidence_hostile_dirs_refuse_closed() {
    struct Plant {
        reason: &'static str,
        set_up: fn(&Path),
    }
    let plants = [
        Plant {
            reason: "evidence is a symlink",
            set_up: |r: &Path| {
                let outside = r.join("../evaded");
                fs::create_dir_all(&outside).unwrap();
                std::os::unix::fs::symlink(&outside, r.join("evidence")).unwrap();
            },
        },
        Plant {
            reason: "evidence/proc is a symlink",
            set_up: |r: &Path| {
                let outside = r.join("../evaded-proc");
                fs::create_dir_all(&outside).unwrap();
                // `evidence` itself must be a WELL-FORMED 0700 dir: this plant judges `evidence/proc`.
                fs::DirBuilder::new().mode(0o700).create(r.join("evidence")).unwrap();
                std::os::unix::fs::symlink(&outside, r.join("evidence/proc")).unwrap();
            },
        },
        Plant {
            reason: "evidence is not a directory",
            set_up: |r: &Path| {
                fs::write(r.join("evidence"), b"not a dir").unwrap();
            },
        },
        Plant {
            reason: "evidence is readable by group or others",
            set_up: |r: &Path| {
                fs::create_dir_all(r.join("evidence")).unwrap();
                let mut perm = fs::metadata(r.join("evidence")).unwrap().permissions();
                perm.set_mode(0o755);
                fs::set_permissions(r.join("evidence"), perm).unwrap();
            },
        },
    ];
    for (n, p) in plants.iter().enumerate() {
        let t = Tmp::new();
        let (r, o, d) = t.fence();
        init_manifest("ev-9", &r, &d);
        (p.set_up)(&r);
        let marker = o.join("marker");
        let script = format!("echo ran > {}", marker.to_string_lossy());
        let file = d.join("ev-9.jsonl");
        let before = fs::read(&file).unwrap();

        let (s, _pid) = start("ev-9", "w", &r, None, &d, &["/bin/sh", "-c", &script]);
        assert_eq!(code(&s), 2, "plant {n} refuses: stdout {:?} stderr {:?}", out(&s), err(&s));
        let e = err(&s);
        assert!(e.starts_with("harness: error: evidence refused: "), "plant {n} stderr: {e:?}");
        assert!(e.contains(p.reason), "plant {n} names {:?}: {e:?}", p.reason);
        assert!(!e.contains(&r.to_string_lossy().to_string()), "plant {n} prints no absolute path: {e:?}");
        assert!(!marker.exists(), "plant {n} ran the program");
        assert_eq!(fs::read(&file).unwrap(), before, "plant {n} changed the manifest");
        // the symlink plants' targets stay empty: nothing was written through them
        if n < 2 {
            let evaded = if n == 0 { r.join("../evaded") } else { r.join("../evaded-proc") };
            assert_eq!(fs::read_dir(&evaded).unwrap().count(), 0, "plant {n} touched the symlink target");
        }
    }
}

// ---------------------------------------------------------------- AC 11

/// AC 11: a spawn failure is RECORDED and refused — exit 2 with today's `cannot start <program>: <kind>`
/// message shape, no process line, the three evidence entries stay live (they record the failed
/// attempt), and the waiter's own exit record says `spawn_failed` / `committed` false. Mutant "answer
/// 0" turns this red.
#[test]
fn evidence_spawn_failure_is_recorded_and_refused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-11", &r, &d);
    let (s, _pid) = start("ev-11", "w", &r, None, &d, &["/nonexistent-harness-prog"]);
    assert_eq!(code(&s), 2, "a spawn failure refuses: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert!(
        err(&s).starts_with("harness: error: cannot start /nonexistent-harness-prog:"),
        "the spawn failure keeps today's message shape: stderr {:?}",
        err(&s)
    );
    let l = run(&["manifest", "list", "ev-11", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    let text = out(&l);
    assert!(!text.lines().any(|x| x.starts_with("w\t")), "no process line is listed: {text}");
    for stream in ["stdout", "stderr", "exit"] {
        assert!(text.lines().any(|x| x.starts_with("w.") && x.ends_with(stream)), "the {stream} evidence entry stays live: {text}");
    }
    let rd = run_dir_of(&d, "ev-11", "w");
    wait_exit(&rd, 5000);
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "outcome"), "spawn_failed");
    assert_eq!(pair(&pairs, "committed"), "false");
    assert_eq!(pair(&pairs, "code"), "-1");
}

// ---------------------------------------------------------------- AC 12

/// AC 12: the waiter alone — launched directly through the hidden subcommand — reports `ok <pid>`,
/// and an end-of-input WITHOUT the commit line makes it std-kill its program: the pid is provably dead
/// within 5 s and the record says `committed` false, `outcome` signalled. Mutant "ignore end-of-input"
/// leaves the program alive and turns this red.
#[test]
fn evidence_uncommitted_waiter_kills_its_program() {
    let t = Tmp::new();
    let (r, _o, _d) = t.fence();
    let rd = r.join("evidence/proc/w.1");
    fs::DirBuilder::new().mode(0o700).create(r.join("evidence")).unwrap();
    fs::DirBuilder::new().mode(0o700).create(r.join("evidence/proc")).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&rd).unwrap();
    let mut guard = Guard::default();

    let mut child: Child = Command::new(BIN)
        .args([
            "process", "waiter",
            rd.to_str().unwrap(), r.to_str().unwrap(), "0",
            "--", "sleep", "30",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    guard.add(child.id());

    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let pid: u32 = line
        .trim_end()
        .strip_prefix("ok ")
        .and_then(|rest| rest.split(' ').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("the waiter reported garbage: {line:?}"));
    assert!(pid > 1, "the handshake names the program pid");
    guard.add(pid);
    assert!(alive(pid), "the program runs before the pipe closes");

    // close the commit pipe WITHOUT the commit line
    drop(child.stdin.take());
    assert!(poll(5000, || !alive(pid)), "the uncommitted waiter left its program running");
    wait_exit(&rd, 5000);
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "committed"), "false");
    assert_eq!(pair(&pairs, "outcome"), "signalled");
    let _ = child.wait();
}

// ---------------------------------------------------------------- AC 13

/// AC 13: a waiter killed by SIGKILL writes NO exit record — unknown is never fabricated as success.
/// The program is stopped afterwards, and no `exit` file ever appears in the run dir. Mutant "start
/// pre-writes a placeholder exit at spawn" turns this red.
#[test]
fn evidence_dead_waiter_writes_no_exit_record() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-13", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-13", "w", &r, None, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1);
    guard.add(pid);

    // the waiter is the program's parent
    let ppid = Command::new("ps")
        .arg("-o")
        .arg("ppid=")
        .arg("-p")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let waiter: u32 = String::from_utf8_lossy(&ppid.stdout).trim().parse().expect("ps prints the waiter pid");
    assert!(waiter > 1 && waiter != pid, "the waiter pid is neither 0 nor the program's");
    let k = Command::new("kill").arg("-9").arg(waiter.to_string()).stdin(Stdio::null()).output().unwrap();
    assert!(k.status.success(), "kill -9 of the waiter failed: {}", String::from_utf8_lossy(&k.stderr));

    let st = run(&["process", "stop", "ev-13", "--id", "w", "--dir", d.to_str().unwrap()]);
    assert!(code(&st) == 0 || code(&st) == 3, "the stop answers 0 or 3: stdout {:?} stderr {:?}", out(&st), err(&st));
    let rd = run_dir_of(&d, "ev-13", "w");
    assert!(!poll(5000, || rd.join("exit").exists()), "a dead waiter must leave no exit record (unknown, never fabricated)");
    assert!(!rd.join("exit.tmp").exists(), "no temp record either");
}

// ---------------------------------------------------------------- fix round 1, F1: the drain bound is a bound in every path

/// Launch the hidden waiter directly (the AC 12 shape) against `prog` and hand back the child plus the
/// PROGRAM's pid from the handshake. The caller owns the commit pipe (the child's stdin).
fn spawn_waiter(rd: &Path, cwd: &Path, prog: &[&str]) -> (Child, u32) {
    let mut v: Vec<String> = vec![
        "process".into(),
        "waiter".into(),
        rd.to_string_lossy().into_owned(),
        cwd.to_string_lossy().into_owned(),
        "0".into(),
        "--".into(),
    ];
    for a in prog {
        v.push((*a).into());
    }
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    let mut child: Child = Command::new(BIN)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let pid: u32 = line
        .trim_end()
        .strip_prefix("ok ")
        .and_then(|rest| rest.split(' ').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("the waiter reported garbage: {line:?}"));
    assert!(pid > 1, "the handshake names the program pid");
    (child, pid)
}

/// A real run dir under `r` (evidence, evidence/proc and `<name>`), the layout the waiter expects.
fn plant_run_dir(r: &Path, name: &str) -> PathBuf {
    let rd = r.join("evidence/proc").join(name);
    fs::DirBuilder::new().mode(0o700).create(r.join("evidence")).unwrap();
    fs::DirBuilder::new().mode(0o700).create(r.join("evidence/proc")).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&rd).unwrap();
    rd
}

/// Fix round 1, F1 (decision 5, and mutant OWN4's red test): a grandchild that keeps BOTH pipes open
/// must not defeat the 2000 ms drain bound. The program exits at once and leaves `sleep 8` behind
/// holding stdout and stderr; the waiter still writes its `exit` record within about 4 s of the exit
/// (a bounded poll, never an elapsed assert) with `output_open_after_exit` TRUE. Before the fix each
/// copier held the stat lock across its whole copy, the main thread blocked on that lock past the
/// bound, and the record landed only when the grandchild died (8 s) — or never.
#[test]
fn evidence_grandchild_holding_both_pipes_still_gets_a_bounded_record() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-f1a", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-f1a", "g", &r, None, &d, &["/bin/sh", "-c", "sleep 8 & exit 0"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    guard.add(pid); // the whole program group (the sleeping grandchild included) is cleaned on drop
    let rd = run_dir_of(&d, "ev-f1a", "g");
    wait_exit(&rd, 4000);
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "output_open_after_exit"), "true", "the pipes were still open past the bound");
    assert_eq!(pair(&pairs, "outcome"), "exited");
    assert_eq!(pair(&pairs, "code"), "0");
    assert_eq!(pair(&pairs, "committed"), "true");
}

/// Fix round 1, F1's second required case: the program is STOPPED while an escaped (setsid) grandchild
/// holds both pipes — the waiter must still END and leave an `exit` record naming the stop signal, with
/// the open-output flag TRUE. Before the fix the waiter stayed alive with no record for as long as the
/// grandchild lived. The escape needs python3 (no `setsid` helper exists on every host); skipped with
/// a printed note where the host has none. The program is a python script whose child setsids, touches
/// a marker and sleeps 12 — the program waits for the marker, so the escape is PROVABLE before the
/// stop fires (a stop that races the setsid kills the grandchild too, and the flag would read false).
#[test]
fn evidence_stopped_program_with_an_escaped_grandchild_still_records() {
    if !Path::new("/usr/bin/python3").exists() {
        println!("SKIP: no /usr/bin/python3 on this host, so the setsid grandchild cannot be spawned");
        return;
    }
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-f1b", &r, &d);
    let mut guard = Guard::default();
    // the grandchild leaves the program's group (setsid) AND inherits both pipes; the program itself
    // waits for the escape marker, then stays alive until the stop
    let escaped = r.join("escaped");
    let script_path = r.join("escape.py");
    let script = format!(
        "import os, time\n\
         pid = os.fork()\n\
         if pid == 0:\n\
         \x20   os.setsid()\n\
         \x20   open({escaped:?}, 'w').write('x')\n\
         \x20   time.sleep(12)\n\
         else:\n\
         \x20   while not os.path.exists({escaped:?}):\n\
         \x20       time.sleep(0.02)\n\
         \x20   time.sleep(30)\n"
    );
    fs::write(&script_path, script).unwrap();
    let (s, pid) = start("ev-f1b", "g", &r, None, &d, &["/usr/bin/python3", script_path.to_str().unwrap()]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1);
    guard.add(pid);
    // the escape is provable BEFORE the stop: the marker exists, so the grandchild is out of the
    // program's group (the stop's TERM cannot reach it) and holds both pipes
    assert!(poll(10_000, || escaped.exists()), "the grandchild never escaped its group");
    // the waiter is the program's parent, found while the program still runs
    let ppid = Command::new("ps").arg("-o").arg("ppid=").arg("-p").arg(pid.to_string()).stdin(Stdio::null()).output().unwrap();
    let waiter: u32 = String::from_utf8_lossy(&ppid.stdout).trim().parse().expect("ps prints the waiter pid");
    assert!(waiter > 1 && waiter != pid, "the waiter pid is neither 0 nor the program's");

    let st = run(&["process", "stop", "ev-f1b", "--id", "g", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&st), 0, "stop answers 0: stdout {:?} stderr {:?}", out(&st), err(&st));
    let rd = run_dir_of(&d, "ev-f1b", "g");
    wait_exit(&rd, 6000);
    assert!(poll(4000, || !alive(waiter)), "the waiter ended past its bounded drain while the grandchild still holds the pipes");
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "output_open_after_exit"), "true");
    assert_eq!(pair(&pairs, "outcome"), "signalled");
    let signal = pair(&pairs, "signal").parse::<i32>().unwrap();
    assert!(signal == 15 || signal == 9, "the record names the stop's TERM or KILL: {signal}");
    assert_eq!(pair(&pairs, "committed"), "true");
}

/// Held-out v1 (must-fire): the program exits at once and a background child holds STDERR only — the
/// record still lands within the bound with the open-output flag TRUE (stdout's pipe closed at the
/// exit, stderr's is held). Mutant "hold the stat lock across the copy" (the F1 defect) turns this red.
#[test]
fn evidence_stderr_only_holder_is_bounded() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-v1", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-v1", "g", &r, None, &d, &["/bin/sh", "-c", "sleep 8 >&2 & exit 0"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let rd = run_dir_of(&d, "ev-v1", "g");
    wait_exit(&rd, 4000);
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "output_open_after_exit"), "true", "stderr was still open past the bound");
    assert_eq!(pair(&pairs, "outcome"), "exited");
    assert_eq!(pair(&pairs, "code"), "0");
    assert_eq!(pair(&pairs, "committed"), "true");
}

/// Held-out v2 (must-fire): BOTH streams are held by a grandchild that RELEASES them inside the bound
/// (`sleep 1`) — exactly ONE exit record lands, once, with the open-output flag FALSE (both copiers
/// finished inside the 2000 ms bound). This is the deterministic killer for mutant OWN4
/// (`DRAIN_BOUND_MS = 0`): a zero bound answers "still open" while the 1 s holder lives, the flag
/// turns TRUE and this goes red.
#[test]
fn evidence_short_holder_inside_the_bound_closes_both_streams() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-v2", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-v2", "g", &r, None, &d, &["/bin/sh", "-c", "sleep 1 & exit 3"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let rd = run_dir_of(&d, "ev-v2", "g");
    wait_exit(&rd, 5000);
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "output_open_after_exit"), "false", "both holders released inside the bound");
    assert_eq!(pair(&pairs, "outcome"), "exited");
    assert_eq!(pair(&pairs, "code"), "3");
    // one record, once: a single line (exit_pairs asserts that) and no temp name left or kept
    assert!(!rd.join("exit.tmp").exists(), "the record was written once, atomically");
    assert_eq!(fs::read_dir(&rd).unwrap().count(), 3, "exactly stdout, stderr and exit exist");
}

/// Held-out v3 (must-fire): a program that dies BY SIGNAL while a grandchild holds the pipes — the
/// record names the signal and the open-output flag. `kill -TERM $$` ends the shell itself; the
/// background child (same group, never signalled) keeps both pipes open past the bound.
#[test]
fn evidence_signal_death_with_holder_records_signal_and_flag() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-v3", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-v3", "g", &r, None, &d, &["/bin/sh", "-c", "sleep 8 & kill -TERM $$"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let rd = run_dir_of(&d, "ev-v3", "g");
    wait_exit(&rd, 4000);
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "outcome"), "signalled");
    assert_eq!(pair(&pairs, "signal"), "15");
    assert_eq!(pair(&pairs, "code"), "-1");
    assert_eq!(pair(&pairs, "output_open_after_exit"), "true");
    assert_eq!(pair(&pairs, "committed"), "true");
}

/// Held-out v4 (must-not-fire): a plain exit with no grandchild changes nothing — the flag is FALSE,
/// the streams are byte-exact and the record keeps its shape. With the lock-holding defect (or a zero
/// drain bound) the flag would turn TRUE here and the byte counts could snapshot early.
#[test]
fn evidence_plain_exit_closes_both_streams() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-v4", &r, &d);
    let (s, pid) = start("ev-v4", "g", &r, None, &d, &["/bin/sh", "-c", "printf out; printf err >&2; exit 7"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1);
    let rd = run_dir_of(&d, "ev-v4", "g");
    wait_exit(&rd, 5000);
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "output_open_after_exit"), "false", "both pipes closed at the exit");
    assert_eq!(pair(&pairs, "stdout_bytes"), "3");
    assert_eq!(pair(&pairs, "stderr_bytes"), "3");
    assert_eq!(pair(&pairs, "stdout_truncated"), "false");
    assert_eq!(pair(&pairs, "stderr_truncated"), "false");
    assert_eq!(fs::read(rd.join("stdout")).unwrap(), b"out", "stdout is byte-exact, not an early snapshot");
    assert_eq!(fs::read(rd.join("stderr")).unwrap(), b"err", "stderr is byte-exact, not an early snapshot");
}

/// Held-out v5 (must-not-fire): a large but UNDER-CAP output is copied fully — byte-identical, no
/// truncation marker, no flag. 1 MiB of zeros against the 4 MiB cap.
#[test]
fn evidence_under_cap_output_is_byte_identical() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ev-v5", &r, &d);
    let mut guard = Guard::default();
    let (s, pid) = start("ev-v5", "g", &r, None, &d, &["/bin/sh", "-c", "head -c 1048576 /dev/zero"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let rd = run_dir_of(&d, "ev-v5", "g");
    wait_exit(&rd, 10_000);
    let bytes = fs::read(rd.join("stdout")).unwrap();
    assert_eq!(bytes.len(), 1_048_576, "every byte is kept under the cap");
    assert!(bytes.iter().all(|b| *b == 0), "byte-identical, no marker inserted");
    let pairs = exit_pairs(&rd);
    assert_eq!(pair(&pairs, "stdout_bytes"), "1048576");
    assert_eq!(pair(&pairs, "stdout_truncated"), "false");
    assert_eq!(pair(&pairs, "output_open_after_exit"), "false");
}

// ---------------------------------------------------------------- fix round 1, F4: the waiter-side re-verify

/// Fix round 1, F4 (mutant OWN1's red test): the WAITER re-verifies the run dir before creating
/// anything — a run dir swapped into a symlink between start and waiter is refused (`fail evidence
/// refused: ... does not verify`), nothing is created through the link, and the program never runs.
/// Mutant "the waiter skips verify_run_dir" creates both capture files through the symlink and runs
/// the program, and this goes red on both counts.
#[test]
fn the_waiter_refuses_a_swapped_run_dir_before_creating_anything() {
    let t = Tmp::new();
    let (r, _o, _d) = t.fence();
    let real = r.join("real-run");
    fs::DirBuilder::new().mode(0o700).create(&real).unwrap();
    let linked = plant_run_dir(&r, "w.1");
    let _ = fs::remove_dir(&linked); // plant the NAME as a symlink instead of a dir
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let marker = r.join("ran");
    let script = format!("echo ran > {}", marker.to_string_lossy());

    let o = Command::new(BIN)
        .args([
            "process", "waiter",
            linked.to_str().unwrap(), r.to_str().unwrap(), "0",
            "--", "/bin/sh", "-c", &script,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    let line = out(&o);
    assert!(line.starts_with("fail evidence refused: "), "the waiter refuses the swapped run dir: {line:?}");
    assert!(line.contains("does not verify"), "the refusal names the re-verify: {line:?}");
    assert!(line.contains("symlink"), "the refusal names the shape: {line:?}");
    assert!(!line.contains(&r.to_string_lossy().to_string()), "no absolute path is printed: {line:?}");
    assert_eq!(fs::read_dir(&real).unwrap().count(), 0, "nothing was created through the swapped run dir");
    assert!(!marker.exists(), "the program never ran");
}

// ---------------------------------------------------------------- fix round 1, F2: the rollback's close-before-signal

/// Fix round 1, F2: on a refused HANDSHAKE the rollback signals the WAITER's own group, and the
/// program runs in a group of its own — so the uncommitted commit-pipe close (which makes a LIVE
/// waiter kill its program itself) must reach the waiter BEFORE any signal aimed at the waiter, or
/// the signal kills the waiter and its watcher while the program keeps running. The real `start`
/// handshake cannot be forced to fail slowly, so both halves are replayed here against the REAL
/// waiter subcommand:
///   (1) the hazard, pinned: a TERM to the waiter's group kills the waiter (no record) and the
///       program KEEPS RUNNING — exactly what the old order did to a handshake-failure rollback;
///   (2) the fix's order: the uncommitted close reaches the live waiter FIRST; it kills its program
///       itself, writes the record (`committed:false`, signalled) and ends with no signal at all.
#[test]
fn the_uncommitted_close_beats_a_signal_at_the_waiters_group() {
    // (1) the hazard: the group signal at the waiter cannot reach the program
    {
        let t = Tmp::new();
        let (r, _o, _d) = t.fence();
        let rd = plant_run_dir(&r, "w.1");
        let mut guard = Guard::default();
        let (mut child, pid) = spawn_waiter(&rd, &r, &["sleep", "30"]);
        guard.add(child.id());
        guard.add(pid);
        assert!(alive(pid), "the program runs before any signal");
        let k = Command::new("kill").arg("-TERM").arg("--").arg(format!("-{}", child.id())).stdin(Stdio::null()).output().unwrap();
        assert!(k.status.success(), "the TERM at the waiter's group ran: {}", String::from_utf8_lossy(&k.stderr));
        assert!(poll(5000, || child.try_wait().map(|s| s.is_some()).unwrap_or(false)), "the waiter died on its own group's TERM");
        assert!(alive(pid), "the program outlived the waiter: the group signal never reached it — the close must come first");
        assert!(!poll(2000, || rd.join("exit").exists()), "a waiter killed by its group's TERM leaves no record");
    }
    // (2) the fix: the uncommitted close reaches the LIVE waiter first, and that alone is enough
    {
        let t = Tmp::new();
        let (r, _o, _d) = t.fence();
        let rd = plant_run_dir(&r, "w.2");
        let mut guard = Guard::default();
        let (mut child, pid) = spawn_waiter(&rd, &r, &["sleep", "30"]);
        guard.add(child.id());
        guard.add(pid);
        drop(child.stdin.take()); // the uncommitted end-of-input, BEFORE any signal
        assert!(poll(5000, || !alive(pid)), "the live waiter killed its program itself");
        assert!(poll(5000, || child.try_wait().map(|s| s.is_some()).unwrap_or(false)), "the waiter ended on its own once its program was dead");
        wait_exit(&rd, 5000);
        let pairs = exit_pairs(&rd);
        assert_eq!(pair(&pairs, "committed"), "false");
        assert_eq!(pair(&pairs, "outcome"), "signalled");
        assert_eq!(pair(&pairs, "signal"), "9", "the waiter's own kill is the absolute-path KILL");
    }
}
