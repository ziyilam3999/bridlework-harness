//! Process-level smoke for `harness status` (P1A-08). Every check runs the real binary as a child process against scratch repos under the
//! system temp folder, from the folder of a DIFFERENT repo. Variables are set only on the child; this file never reads one. Result notes
//! are built with the merged `GateResult::to_json`, so only the last test (AC 17) needs the real `ticket gate`. Every refusal has a planted
//! failing twin: the same run with one thing changed that a wrong build gets wrong.
//!
//! Fixture repo `r`: commits C0, C1, C2 (full shas known to each test); `Fx::at(n)` puts its head on one of them. `o` is an unrelated repo.
//! `caps` is the caps folder (outside the repo): `put`/`put_note` plant a canonical cap record beside every note they write (the way the real gate
//! leaves one), so only the tests that mean to leave the record out run without it.
#![cfg(unix)]
use harness::gate_result::{GateResult, Status};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const MIB: usize = 1024 * 1024;
const KEYS: [&str; 8] = ["ticket", "repo_id", "head", "last_gate", "gate_head", "gate_current", "failure_class", "fix_rounds"];
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("hstatus-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One git call in a scratch repo: no hooks, no inherited repo-redirecting names, no stdin, fixed identity.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?}", args);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write_file(path: &Path, body: &str, exec: bool) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
}

struct Fx {
    t: Tmp,
    repo: PathBuf,
    other: PathBuf,
    res: PathBuf,
    caps: PathBuf,
    c: [String; 3],
}

impl Fx {
    /// Repo `r` with C0, C1, C2 (head at C2), the unrelated repo `other-repo-zz9`, and an empty result folder.
    fn new() -> Fx {
        let t = Tmp::new();
        let (repo, other, res, caps) = (t.0.join("r"), t.0.join("other-repo-zz9"), t.0.join("res"), t.0.join("caps"));
        for d in [&repo, &other, &res, &caps] {
            fs::create_dir_all(d).unwrap();
        }
        git(&repo, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&repo, &["config", k, v]);
        }
        let mut c: Vec<String> = Vec::new();
        for n in 0..3 {
            write_file(&repo.join("f.txt"), &format!("version {n}"), false);
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "-m", &format!("c{n}")]);
            c.push(git(&repo, &["rev-parse", "HEAD"]));
        }
        git(&other, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&other, &["config", k, v]);
        }
        write_file(&other.join("o.txt"), "other repo", false);
        git(&other, &["add", "-A"]);
        git(&other, &["commit", "-q", "-m", "other"]);
        let c: [String; 3] = [c[0].clone(), c[1].clone(), c[2].clone()];
        Fx { repo: repo.canonicalize().unwrap(), other: other.canonicalize().unwrap(), res: res.canonicalize().unwrap(), caps: caps.canonicalize().unwrap(), c, t }
    }

    /// `r` with its head on commit `n`.
    fn at(n: usize) -> Fx {
        let fx = Fx::new();
        git(&fx.repo, &["checkout", "-q", "--detach", &fx.c[n]]);
        fx
    }

    /// A note, with a canonical record (no rounds, none in flight) planted beside it.
    fn put(&self, id: &str, body: &str) -> PathBuf {
        let p = self.put_bare(id, body);
        self.record(id, 0, false);
        p
    }

    /// A note with NO record beside it (the deleted-record state).
    fn put_bare(&self, id: &str, body: &str) -> PathBuf {
        let p = self.res.join(format!("{id}.json"));
        fs::write(&p, body).unwrap();
        p
    }

    /// The canonical record for ticket `id` on repo `r`.
    fn record(&self, id: &str, fix_rounds: u8, in_flight: bool) {
        fs::write(self.caps.join(format!("{id}.rounds")), record_bytes(id, "r", fix_rounds, in_flight)).unwrap();
    }

    fn put_note(&self, id: &str, r: &GateResult) {
        self.put(id, &r.to_json().unwrap());
    }

    fn ticket(&self, body: &str) -> PathBuf {
        let d = self.t.0.join("tickets");
        fs::create_dir_all(&d).unwrap();
        let f = d.join(format!("t{}.json", SEQ.fetch_add(1, Ordering::SeqCst)));
        fs::write(&f, body).unwrap();
        f
    }
}

/// The canonical record bytes (the P1A-09 format), written here by hand so a status that parsed leniently would still be caught.
fn record_bytes(id: &str, repo_id: &str, fix_rounds: u8, in_flight: bool) -> String {
    format!("{{\"v\":1,\"ticket\":\"{id}\",\"repo_id\":\"{repo_id}\",\"fix_rounds\":{fix_rounds},\"in_flight\":{in_flight}}}\n")
}

fn ticket_json(repo_id: &str, id: &str, skip_repo_id: bool) -> String {
    let mut pairs: Vec<(&str, String)> = vec![
        ("id", format!("\"{id}\"")),
        ("repo_id", format!("\"{repo_id}\"")),
        ("title", "\"A title\"".to_string()),
        ("group", "\"grp\"".to_string()),
        ("size", "\"M\"".to_string()),
        ("objective", "\"An objective\"".to_string()),
        ("acceptance_criteria", "[\"one\"]".to_string()),
        ("dependencies", "[]".to_string()),
        ("risk", "\"normal\"".to_string()),
        ("owner_role", "\"execution_verification\"".to_string()),
        ("evidence_links", "[]".to_string()),
    ];
    if skip_repo_id {
        pairs.retain(|(k, _)| *k != "repo_id");
    }
    let body: Vec<String> = pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{{}}}", body.join(","))
}

fn note(status: Status, head: &str, class: Option<&str>) -> GateResult {
    GateResult { status, gate_id: "ticket-gate".to_string(), head_sha: head.to_string(), failure_class: class.map(|c| c.to_string()), message: "m".to_string(), file: None, line: None }
}

fn fail_note(head: &str) -> GateResult {
    note(Status::Fail, head, Some("check-failed"))
}

fn pass_note(head: &str) -> GateResult {
    note(Status::Pass, head, None)
}

/// Run `harness <sub> <args>` from `cwd`.
fn run_sub(sub: &str, cwd: &Path, args: &[String]) -> Output {
    let mut c = Command::new(BIN);
    c.arg(sub).args(args).current_dir(cwd).stdin(Stdio::null());
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    c.output().unwrap()
}

fn args_for(tf: &Path, repo: &Path, res: &str, caps: &str) -> Vec<String> {
    vec![tf.to_str().unwrap().into(), "--repo-map".into(), format!("r={}", repo.to_str().unwrap()), "--result-dir".into(), res.into(), "--caps-dir".into(), caps.into()]
}

/// Status of ticket file `tf` against the fixture, from `cwd`, with result folder `res` and the fixture's caps folder.
fn status_with(fx: &Fx, tf: &Path, res: &str, cwd: &Path) -> Output {
    run_sub("status", cwd, &args_for(tf, &fx.repo, res, fx.caps.to_str().unwrap()))
}

/// Status of ticket file `tf` against the fixture, from the unrelated repo, with the fixture's result folder and the caps folder `caps`.
fn status_caps(fx: &Fx, tf: &Path, caps: &str) -> Output {
    run_sub("status", &fx.other, &args_for(tf, &fx.repo, fx.res.to_str().unwrap(), caps))
}

/// The default run: ticket `id` on repo `r`, from the unrelated repo, with the fixture's result folder.
fn status(fx: &Fx, id: &str) -> Output {
    let tf = fx.ticket(&ticket_json("r", id, false));
    status_with(fx, &tf, fx.res.to_str().unwrap(), &fx.other)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn leaks(o: &Output, needle: &str) -> bool {
    out(o).contains(needle) || err(o).contains(needle)
}

/// A refusal: this exit code, stdout empty.
fn refused(o: &Output, want: i32, label: &str) {
    assert_eq!(code(o), want, "{label}: stderr {}", err(o));
    assert!(out(o).is_empty(), "{label}: stdout must be empty: {}", out(o));
}

/// The success line as (key, value) pairs, in the order printed. Exit 0 and exactly one line are asserted here.
fn pairs(o: &Output) -> Vec<(String, String)> {
    assert_eq!(code(o), 0, "stderr: {}", err(o));
    let text = out(o);
    assert_eq!(text.lines().count(), 1, "exactly one line: {text:?}");
    assert!(text.ends_with('\n'));
    text.trim_end().split(' ').map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).expect("key=value")).collect()
}

fn get(o: &Output, key: &str) -> String {
    pairs(o).into_iter().find(|(k, _)| k == key).map(|(_, v)| v).unwrap_or_else(|| panic!("no key {key}"))
}

/// Path, kind, size and mtime (ns) of every entry under `d`, recursively, links not followed.
fn snapshot(d: &Path) -> Vec<String> {
    fn walk(d: &Path, base: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            let m = fs::symlink_metadata(&p).unwrap();
            let ns = m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            out.push(format!("{} dir={} len={} mtime={}", p.strip_prefix(base).unwrap().to_string_lossy(), m.is_dir(), m.len(), ns));
            if m.is_dir() {
                walk(&p, base, out);
            }
        }
    }
    let mut v = Vec::new();
    walk(d, d, &mut v);
    v.sort();
    v
}

fn file_snapshot(f: &Path) -> String {
    let m = fs::symlink_metadata(f).unwrap();
    format!("len={} mtime={:?}", m.len(), m.modified().unwrap())
}

// The leak helper really does catch an echoed folder (twin of every "not in the output" assertion below).
#[test]
fn the_leak_helper_catches_an_echoed_folder() {
    let fake = Command::new("sh").args(["-c", "echo /synthetic-folder-0xA1 >&2"]).output().unwrap();
    assert!(leaks(&fake, "/synthetic-folder-0xA1"));
    assert!(!leaks(&fake, "/another-folder"));
}

// AC 1: a FAIL note for the head at C1 is shown as a current fail. Twin: a PASS note for the head (AC 2) prints something else.
#[test]
fn ac01_a_fail_note_for_the_head_is_shown_as_a_current_fail() {
    let fx = Fx::at(1);
    fx.put_note("EX-1", &fail_note(&fx.c[1]));
    let o = status(&fx, "EX-1");
    for (k, v) in [("ticket", "EX-1"), ("repo_id", "r"), ("head", &fx.c[1]), ("last_gate", "fail"), ("gate_head", &fx.c[1]), ("gate_current", "yes"), ("failure_class", "check-failed")] {
        assert_eq!(get(&o, k), v, "{k}");
    }
    fx.put_note("EX-1", &pass_note(&fx.c[1]));
    let twin = status(&fx, "EX-1");
    assert_eq!((get(&twin, "last_gate"), get(&twin, "failure_class")), ("pass".to_string(), "none".to_string()), "twin: a pass note reads differently");
}

// AC 2: a PASS note for the head at C2 is a current pass with no class. Twin: AC 1 (a fail note carries a class).
#[test]
fn ac02_a_pass_note_for_the_head_is_shown_as_a_current_pass() {
    let fx = Fx::at(2);
    fx.put_note("EX-1", &pass_note(&fx.c[2]));
    let o = status(&fx, "EX-1");
    for (k, v) in [("head", &fx.c[2]), ("last_gate", &"pass".to_string()), ("gate_head", &fx.c[2]), ("gate_current", &"yes".to_string()), ("failure_class", &"none".to_string())] {
        assert_eq!(get(&o, k), *v, "{k}");
    }
    fx.put_note("EX-1", &fail_note(&fx.c[2]));
    assert_eq!(get(&status(&fx, "EX-1"), "failure_class"), "check-failed", "twin: a fail note carries its class");
}

// AC 3: a PASS note about C1 while the head is C2 is stale, never current. Twin: the note names C2.
#[test]
fn ac03_a_note_for_an_older_commit_is_stale() {
    let fx = Fx::at(2);
    fx.put_note("EX-1", &pass_note(&fx.c[1]));
    let o = status(&fx, "EX-1");
    assert_eq!((get(&o, "last_gate"), get(&o, "gate_head"), get(&o, "gate_current")), ("pass".to_string(), fx.c[1].clone(), "no".to_string()));
    fx.put_note("EX-1", &pass_note(&fx.c[2]));
    assert_eq!(get(&status(&fx, "EX-1"), "gate_current"), "yes", "twin: the note names the head");
}

// AC 4: no note at all is `none` and exit 0, never a pass and never an error. Twin: the same ticket with a note present (AC 1).
#[test]
fn ac04_an_absent_note_is_none() {
    let fx = Fx::at(1);
    let o = status(&fx, "EX-1");
    for k in ["last_gate", "gate_head", "gate_current", "failure_class"] {
        assert_eq!(get(&o, k), "none", "{k}");
    }
    fx.put_note("EX-1", &fail_note(&fx.c[1]));
    assert_eq!(get(&status(&fx, "EX-1"), "last_gate"), "fail", "twin: a note present");
}

// AC 5: a ticket with no repo id, a repo id not in the map, and a mapped folder that is not a repo top are all exit 1 with stdout empty,
// and no folder name appears. Twin: the valid ticket.
#[test]
fn ac05_a_ticket_that_intake_refuses_exits_1() {
    let fx = Fx::at(1);
    let o = status(&fx, "EX-1");
    assert_eq!(code(&o), 0, "twin: the valid ticket");
    let no_repo = fx.ticket(&ticket_json("r", "EX-1", true));
    refused(&status_with(&fx, &no_repo, fx.res.to_str().unwrap(), &fx.other), 1, "no repo id");
    let unmapped = fx.ticket(&ticket_json("zz", "EX-1", false));
    refused(&status_with(&fx, &unmapped, fx.res.to_str().unwrap(), &fx.other), 1, "repo id not in the map");
    let plain = fx.t.0.join("plain-dir-q7");
    fs::create_dir_all(plain.join("sub")).unwrap();
    for (label, dir) in [("not a repo", plain.clone()), ("subfolder of a repo", fx.repo.join("sub-q7")), ("missing folder", fx.t.0.join("no-such-q7"))] {
        if label == "subfolder of a repo" {
            fs::create_dir_all(&dir).unwrap();
        }
        let tf = fx.ticket(&ticket_json("r", "EX-1", false));
        let o = run_sub("status", &fx.other, &args_for(&tf, &dir, fx.res.to_str().unwrap(), fx.caps.to_str().unwrap()));
        refused(&o, 1, label);
        let name = dir.file_name().unwrap().to_str().unwrap().to_string();
        assert!(!leaks(&o, &name) && !leaks(&o, dir.to_str().unwrap()), "{label}: no folder name in a stream: {}", err(&o));
    }
}

// AC 6: a malformed note is exit 2, never `none`. Twin: a valid note.
#[test]
fn ac06_a_malformed_note_is_refused() {
    let fx = Fx::at(1);
    let c1 = fx.c[1].clone();
    let valid = fail_note(&c1).to_json().unwrap();
    fx.put("EX-1", &valid);
    assert_eq!(code(&status(&fx, "EX-1")), 0, "twin: a valid note");
    let raw = |status: &str, class: &str, sha: &str| {
        format!("{{\"status\":\"{status}\",\"gate_id\":\"ticket-gate\",\"head_sha\":\"{sha}\",\"failure_class\":{class},\"message\":\"m\",\"file\":null,\"line\":null}}")
    };
    let cases: Vec<(&str, String)> = vec![
        ("not JSON", "this is not json".to_string()),
        ("unknown extra key", valid.replace("\"line\":null}", "\"line\":null,\"extra\":1}")),
        ("missing key", valid.replace(",\"line\":null", "")),
        ("pass carrying a class", raw("pass", "\"check-failed\"", &c1)),
        ("7-char sha", raw("fail", "\"check-failed\"", &c1[..7])),
        ("uppercase sha", raw("fail", "\"check-failed\"", &c1.to_uppercase())),
    ];
    for (label, body) in &cases {
        fx.put("EX-1", body);
        refused(&status(&fx, "EX-1"), 2, label);
    }
}

// AC 7: a valid note padded to exactly 1 MiB + 1 byte is exit 2; padded to exactly 1 MiB it reads normally.
#[test]
fn ac07_a_note_over_the_size_cap_is_refused() {
    let fx = Fx::at(1);
    let valid = fail_note(&fx.c[1]).to_json().unwrap();
    let padded = |total: usize| format!("{valid}{}", " ".repeat(total - valid.len()));
    fx.put("EX-1", &padded(MIB));
    assert_eq!(code(&status(&fx, "EX-1")), 0, "twin: exactly 1 MiB");
    fx.put("EX-1", &padded(MIB + 1));
    refused(&status(&fx, "EX-1"), 2, "1 MiB + 1");
}

// AC 8: a result folder that is not an absolute, real, outside-the-repo directory is exit 2 and never echoed. Twin: such a directory.
#[test]
fn ac08_a_result_directory_that_is_not_a_real_outside_directory_is_refused() {
    let fx = Fx::at(1);
    fx.put_note("EX-1", &fail_note(&fx.c[1]));
    fs::create_dir_all(fx.other.join("res-rel")).unwrap();
    fs::create_dir_all(fx.repo.join("out")).unwrap();
    fs::create_dir_all(fx.repo.join("out2")).unwrap();
    for d in ["res-rel"] {
        fs::write(fx.other.join(d).join("EX-1.json"), fail_note(&fx.c[1]).to_json().unwrap()).unwrap();
    }
    for d in ["out", "out2"] {
        fs::write(fx.repo.join(d).join("EX-1.json"), fail_note(&fx.c[1]).to_json().unwrap()).unwrap();
    }
    let link = fx.t.0.join("link-res");
    symlink(&fx.res, &link).unwrap();
    let into_repo = fx.t.0.join("lnk");
    symlink(&fx.repo, &into_repo).unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("relative", "res-rel".to_string()),
        ("dot-dot", format!("{}/../res", fx.res.display())),
        ("symlink to a real directory", link.display().to_string()),
        ("inside the repo", fx.repo.join("out").display().to_string()),
        ("parent component is a link into the repo", into_repo.join("out2").display().to_string()),
        ("missing", fx.t.0.join("nope").display().to_string()),
    ];
    for (label, dir) in &cases {
        let tf = fx.ticket(&ticket_json("r", "EX-1", false));
        let o = status_with(&fx, &tf, dir, &fx.other);
        refused(&o, 2, label);
        assert!(!leaks(&o, dir), "{label}: the result folder must not be echoed: {}", err(&o));
    }
    assert_eq!(code(&status(&fx, "EX-1")), 0, "twin: an absolute real directory outside the repo");
}

// AC 9: an id outside the allow-list is exit 2 with stdout empty (no forged line), even with a valid decoy note at the escaped place.
// Twin: ids `EX-1.v2` and `EX-1` read normally.
#[test]
fn ac09_a_ticket_id_outside_the_allow_list_is_refused() {
    let fx = Fx::at(1);
    let decoy = fail_note(&fx.c[1]).to_json().unwrap();
    fs::write(fx.t.0.join("escape.json"), &decoy).unwrap();
    fs::create_dir_all(fx.res.join("a")).unwrap();
    fs::write(fx.res.join("a").join("b.json"), &decoy).unwrap();
    fx.put(".hidden", &decoy);
    let long = "a".repeat(65);
    let cases: Vec<(&str, String)> = vec![
        ("parent escape", "../escape".to_string()),
        ("separator", "a/b".to_string()),
        ("leading dot", ".hidden".to_string()),
        ("65 bytes", long),
        ("newline (forged line)", "EX-1\\nlast_gate=pass".to_string()),
        ("escape character", "EX-1\\u001b[0m".to_string()),
        ("space", "EX 1".to_string()),
        ("equals sign", "a=b".to_string()),
    ];
    for (label, id) in &cases {
        let tf = fx.ticket(&ticket_json("r", id, false));
        refused(&status_with(&fx, &tf, fx.res.to_str().unwrap(), &fx.other), 2, label);
    }
    fx.put("EX-1.v2", &decoy);
    fx.put("EX-1", &decoy);
    for id in ["EX-1.v2", "EX-1"] {
        assert_eq!(get(&status(&fx, id), "ticket"), id, "twin: {id} reads normally");
    }
}

// AC 10: a note that is a link to a valid note is exit 2. Twin: the same note as a regular file.
#[test]
fn ac10_a_note_that_is_a_symlink_is_refused() {
    let fx = Fx::at(1);
    let real = fx.t.0.join("real-note.json");
    fs::write(&real, fail_note(&fx.c[1]).to_json().unwrap()).unwrap();
    symlink(&real, fx.res.join("EX-1.json")).unwrap();
    fx.record("EX-1", 0, false);
    refused(&status(&fx, "EX-1"), 2, "symlink");
    fs::remove_file(fx.res.join("EX-1.json")).unwrap();
    fs::copy(&real, fx.res.join("EX-1.json")).unwrap();
    assert_eq!(get(&status(&fx, "EX-1"), "last_gate"), "fail", "twin: a regular file");
}

// AC 11: only `ex-1.json` present for ticket `EX-1` is exit 2, whatever the filesystem. Twin: the exact name.
#[test]
fn ac11_a_case_variant_of_the_note_name_is_refused() {
    let fx = Fx::at(1);
    fs::write(fx.res.join("ex-1.json"), fail_note(&fx.c[1]).to_json().unwrap()).unwrap();
    fx.record("EX-1", 0, false);
    refused(&status(&fx, "EX-1"), 2, "case variant");
    fs::remove_file(fx.res.join("ex-1.json")).unwrap();
    fx.put_note("EX-1", &fail_note(&fx.c[1]));
    assert_eq!(get(&status(&fx, "EX-1"), "last_gate"), "fail", "twin: the exact name");
}

// AC 12: a pure observation. The repo (with .git), result folder, caps folder and ticket file are identical before and after (listings taken BEFORE
// this test's own `git status`, which could refresh the index), and no `index.lock` is left. The index is made stale first, so a status that
// ran `git status` would rewrite it. The caps folder gets no lock file and no record from a read.
#[test]
fn ac12_a_run_changes_nothing() {
    let fx = Fx::at(2);
    fx.put_note("EX-1", &pass_note(&fx.c[2]));
    std::thread::sleep(std::time::Duration::from_millis(30));
    fs::write(fx.repo.join("f.txt"), "version 2").unwrap();
    let tf = fx.ticket(&ticket_json("r", "EX-1", false));
    assert!(!fx.caps.join(".EX-1.lock").exists(), "precondition: no lock file yet");
    let before = (snapshot(&fx.repo), snapshot(&fx.res), file_snapshot(&tf), snapshot(&fx.other), snapshot(&fx.caps));
    let o = status_with(&fx, &tf, fx.res.to_str().unwrap(), &fx.other);
    let after = (snapshot(&fx.repo), snapshot(&fx.res), file_snapshot(&tf), snapshot(&fx.other), snapshot(&fx.caps));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(before.0, after.0, "the repo, .git included");
    assert_eq!(before.1, after.1, "the result folder");
    assert_eq!(before.2, after.2, "the ticket file");
    assert_eq!(before.3, after.3, "the unrelated repo");
    assert_eq!(before.4, after.4, "the caps folder: no lock file, no record, nothing touched");
    assert!(!fx.caps.join(".EX-1.lock").exists(), "no lock file was created");
    assert!(!fx.repo.join(".git").join("index.lock").exists(), "no index.lock");
    assert_eq!(git(&fx.repo, &["status", "--porcelain"]), "", "git status shows no change");
}

// AC 7(b): another process holds the ticket's lock (a gate run in flight): status neither waits nor reports busy; the run counts. A hang is a
// failure, not a stuck `cargo test`. Twin: the same world with no lock held gives the same line.
#[test]
fn ac12b_a_held_lock_neither_blocks_nor_refuses_status() {
    let fx = Fx::at(1);
    fx.put_note("EX-1", &fail_note(&fx.c[1]));
    fx.record("EX-1", 1, true);
    let tf = fx.ticket(&ticket_json("r", "EX-1", false));
    let free = status_with(&fx, &tf, fx.res.to_str().unwrap(), &fx.other);
    assert_eq!(get(&free, "fix_rounds"), "2", "twin: no lock held");
    let lock = fs::OpenOptions::new().write(true).create(true).truncate(false).open(fx.caps.join(".EX-1.lock")).unwrap();
    lock.try_lock().unwrap();
    let before = snapshot(&fx.caps);
    let mut child = Command::new(BIN)
        .arg("status")
        .args(args_for(&tf, &fx.repo, fx.res.to_str().unwrap(), fx.caps.to_str().unwrap()))
        .current_dir(&fx.other)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    let done = loop {
        if child.try_wait().unwrap().is_some() {
            break true;
        }
        if start.elapsed() > std::time::Duration::from_secs(10) {
            let _ = child.kill();
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let o = child.wait_with_output().unwrap();
    assert!(done, "status must not wait on a held lock");
    assert_eq!(get(&o, "fix_rounds"), "2", "a run in flight counts, and the lock is not an error");
    assert_eq!(before, snapshot(&fx.caps), "the caps folder is untouched");
    drop(lock);
}

// AC 13: a note's message, file and line are never printed, and nothing is on stderr. Twin: a note with an empty-ish message.
#[test]
fn ac13_a_note_cannot_forge_or_leak_through_its_text() {
    let fx = Fx::at(1);
    let mut n = fail_note(&fx.c[1]);
    n.message = "see /var/forged-folder-77 done=allowed\nlast_gate=pass".to_string();
    n.file = Some("src/forged-file-77.rs".to_string());
    n.line = Some(77);
    fx.put_note("EX-1", &n);
    let o = status(&fx, "EX-1");
    assert_eq!(get(&o, "last_gate"), "fail");
    for needle in ["forged-folder-77", "done=allowed", "forged-file-77", "last_gate=pass", "line"] {
        assert!(!leaks(&o, needle), "{needle} must not appear: {}", out(&o));
    }
    assert!(err(&o).is_empty(), "stderr must be empty on success: {}", err(&o));
    let mut clean = fail_note(&fx.c[1]);
    clean.message = "x".to_string();
    fx.put_note("EX-1", &clean);
    assert_eq!(out(&status(&fx, "EX-1")), out(&o), "twin: the output does not depend on the text");
}

// AC 14: from the folder of an unrelated repo the output is the same as from inside `r`; the unrelated folder is not named and not changed.
#[test]
fn ac14_the_repo_comes_from_the_map_not_the_current_folder() {
    let fx = Fx::at(1);
    fx.put_note("EX-1", &fail_note(&fx.c[1]));
    let before = snapshot(&fx.other);
    let tf = fx.ticket(&ticket_json("r", "EX-1", false));
    let from_other = status_with(&fx, &tf, fx.res.to_str().unwrap(), &fx.other);
    let from_r = status_with(&fx, &tf, fx.res.to_str().unwrap(), &fx.repo);
    assert_eq!(out(&from_other), out(&from_r), "same output from either folder");
    assert_eq!(get(&from_other, "head"), fx.c[1]);
    assert!(!leaks(&from_other, "other-repo-zz9"), "the unrelated folder is not named");
    assert_eq!(before, snapshot(&fx.other), "the unrelated repo is unchanged");
    assert_ne!(git(&fx.other, &["rev-parse", "HEAD"]), fx.c[1], "the unrelated repo has a different head");
}

// AC 15: usage errors are exit 2 with stdout empty and nothing created. Twin: a complete command line.
#[test]
fn ac15_usage_errors_exit_2_and_create_nothing() {
    let fx = Fx::at(1);
    fx.put_note("EX-1", &fail_note(&fx.c[1]));
    let tf = fx.ticket(&ticket_json("r", "EX-1", false));
    let (t, r, res, cp) = (tf.to_str().unwrap().to_string(), fx.repo.to_str().unwrap().to_string(), fx.res.to_str().unwrap().to_string(), fx.caps.to_str().unwrap().to_string());
    let map = format!("r={r}");
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    assert_eq!(code(&run_sub("status", &fx.other, &s(&[&t, "--repo-map", &map, "--result-dir", &res, "--caps-dir", &cp]))), 0, "twin: a complete command line");
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("no args", s(&[])),
        ("no ticket file", s(&["--repo-map", &map, "--result-dir", &res, "--caps-dir", &cp])),
        ("missing --result-dir", s(&[&t, "--repo-map", &map, "--caps-dir", &cp])),
        ("repeated --result-dir", s(&[&t, "--repo-map", &map, "--result-dir", &res, "--result-dir", &res, "--caps-dir", &cp])),
        ("unknown option", s(&[&t, "--repo-map", &map, "--result-dir", &res, "--caps-dir", &cp, "--nope"])),
        ("two ticket files", s(&[&t, &t, "--repo-map", &map, "--result-dir", &res, "--caps-dir", &cp])),
        ("--repo-map with no value", s(&[&t, "--result-dir", &res, "--caps-dir", &cp, "--repo-map"])),
        ("missing --caps-dir (everything else correct)", s(&[&t, "--repo-map", &map, "--result-dir", &res])),
        ("repeated --caps-dir", s(&[&t, "--repo-map", &map, "--result-dir", &res, "--caps-dir", &cp, "--caps-dir", &cp])),
        ("--caps-dir with no value", s(&[&t, "--repo-map", &map, "--result-dir", &res, "--caps-dir"])),
    ];
    let tree = || (snapshot(&fx.t.0), fx.t.0.exists());
    let before = tree();
    for (label, args) in &cases {
        let o = run_sub("status", &fx.other, args);
        refused(&o, 2, label);
        if label.contains("caps") {
            assert!(err(&o).contains("--caps-dir"), "{label}: the usage error names the option: {}", err(&o));
        }
    }
    assert_eq!(before, tree(), "no file created or changed (the caps folder included)");
}

// AC 16: stdout parses into exactly the eight keys, in order, with `fix_rounds` a single digit 0-4 and never the old placeholder, for every success row.
#[test]
fn ac16_the_line_has_exactly_the_eight_keys_and_no_invented_round() {
    let check = |o: &Output, label: &str| {
        let p = pairs(o);
        let keys: Vec<&str> = p.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, KEYS.to_vec(), "{label}: keys and order");
        assert!(matches!(p[7].1.as_str(), "0" | "1" | "2" | "3" | "4"), "{label}: fix_rounds is one digit 0-4: {:?}", p[7].1);
        assert!(p.iter().all(|(_, v)| v != "unrecorded"), "{label}: the placeholder is gone");
    };
    let fx = Fx::at(1);
    check(&status(&fx, "EX-1"), "no note");
    fx.put_note("EX-1", &fail_note(&fx.c[1]));
    check(&status(&fx, "EX-1"), "fail");
    fx.put_note("EX-1", &pass_note(&fx.c[1]));
    check(&status(&fx, "EX-1"), "pass");
    let mut n = fail_note(&fx.c[1]);
    n.message = "text".to_string();
    n.file = Some("src/a.rs".to_string());
    n.line = Some(3);
    fx.put_note("EX-1", &n);
    check(&status(&fx, "EX-1"), "note with text");
    fx.put_note("EX-1", &pass_note(&fx.c[0]));
    check(&status(&fx, "EX-1"), "stale");
    fx.put_note("EX-1.v2", &pass_note(&fx.c[1]));
    check(&status(&fx, "EX-1.v2"), "dotted id");
    let tf = fx.ticket(&ticket_json("r", "EX-1", false));
    check(&status_with(&fx, &tf, fx.res.to_str().unwrap(), &fx.repo), "run from inside r");
}

/// A world for the real gate: repo `r` with a failing test at head C1 (C0 is the base), an unrelated repo, empty result and caps folders.
struct Gw {
    /// Held only so the scratch world is removed when the world is dropped.
    _t: Tmp,
    repo: PathBuf,
    other: PathBuf,
    res: PathBuf,
    caps: PathBuf,
    tf: PathBuf,
    c0: String,
    c1: String,
}

impl Gw {
    fn new() -> Gw {
        let t = Tmp::new();
        let (repo, other, res, caps) = (t.0.join("r"), t.0.join("other-repo-zz9"), t.0.join("res"), t.0.join("caps"));
        for d in [&repo, &other, &res, &caps] {
            fs::create_dir_all(d).unwrap();
        }
        git(&repo, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&repo, &["config", k, v]);
        }
        write_file(&repo.join("gates/t.json"), "{\"id\":\"t\",\"program\":\"ci/t.sh\",\"args\":[],\"timeout_ms\":20000,\"max_output_bytes\":4096,\"both_ends\":false}", false);
        write_file(&repo.join("gates/path-map.json"), "{\"src\":[\"t\"]}", false);
        write_file(&repo.join("ci/t.sh"), "#!/bin/sh\n[ \"$(cat src/value.txt)\" = ok ]\n", true);
        write_file(&repo.join("src/value.txt"), "v0", false);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c0"]);
        let c0 = git(&repo, &["rev-parse", "HEAD"]);
        write_file(&repo.join("src/value.txt"), "bad", false);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c1"]);
        let c1 = git(&repo, &["rev-parse", "HEAD"]);
        git(&other, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&other, &["config", k, v]);
        }
        write_file(&other.join("o.txt"), "other", false);
        git(&other, &["add", "-A"]);
        git(&other, &["commit", "-q", "-m", "other"]);
        let (repo, other, res, caps) = (repo.canonicalize().unwrap(), other.canonicalize().unwrap(), res.canonicalize().unwrap(), caps.canonicalize().unwrap());
        let tf = t.0.join("ticket.json");
        fs::write(&tf, ticket_json("r", "EX-1", false)).unwrap();
        Gw { _t: t, repo, other, res, caps, tf, c0, c1 }
    }

    fn gate(&self) -> Output {
        let mut a = args_for(&self.tf, &self.repo, self.res.to_str().unwrap(), self.caps.to_str().unwrap());
        a.insert(3, self.c0.clone());
        a.insert(3, "--base".to_string());
        run_sub("ticket", &self.other, &[vec!["gate".to_string()], a].concat())
    }

    fn st(&self) -> Output {
        run_sub("status", &self.other, &args_for(&self.tf, &self.repo, self.res.to_str().unwrap(), self.caps.to_str().unwrap()))
    }

    fn fix(&self) -> String {
        write_file(&self.repo.join("src/value.txt"), "ok", false);
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", "fix"]);
        git(&self.repo, &["rev-parse", "HEAD"])
    }
}

// AC 17: the real gate writes the note, status reads it. A planted failing test, then a fix commit. The rounds are the real gate's: one FAIL is 1,
// the PASS after it keeps 1 (a PASS does not erase a spent round). Twin: before any run, no note and no record is 0.
#[test]
fn ac17_status_reads_what_the_real_gate_wrote() {
    let w = Gw::new();
    let get_from = |o: &Output, k: &str| pairs(o).into_iter().find(|(key, _)| key == k).unwrap().1;
    let pre = w.st();
    assert_eq!((get_from(&pre, "last_gate"), get_from(&pre, "fix_rounds")), ("none".to_string(), "0".to_string()), "twin: no run, no note, no record");
    let g = w.gate();
    assert_eq!(code(&g), 1, "the planted failing test fails the gate; stderr {}", err(&g));
    let o = w.st();
    assert_eq!((get_from(&o, "last_gate"), get_from(&o, "head"), get_from(&o, "gate_head"), get_from(&o, "gate_current")), ("fail".to_string(), w.c1.clone(), w.c1.clone(), "yes".to_string()));
    assert_eq!(get_from(&o, "fix_rounds"), "1", "one FAIL run is one round");
    let c2 = w.fix();
    let stale = w.st();
    assert_eq!((get_from(&stale, "last_gate"), get_from(&stale, "gate_current")), ("fail".to_string(), "no".to_string()), "the old note is stale after the fix commit");
    assert_eq!(get_from(&stale, "fix_rounds"), "1");
    let g2 = w.gate();
    assert_eq!(code(&g2), 0, "the fix passes the gate; stderr {}", err(&g2));
    let o2 = w.st();
    assert_eq!((get_from(&o2, "last_gate"), get_from(&o2, "head"), get_from(&o2, "gate_head"), get_from(&o2, "gate_current")), ("pass".to_string(), c2.clone(), c2, "yes".to_string()));
    assert_eq!(get_from(&o2, "fix_rounds"), "1", "FAIL then PASS is still 1");
}

// AC 1: three real FAIL runs are three rounds. Twin: the same command with no `--caps-dir` is a usage error.
#[test]
fn ac17b_three_real_fail_runs_are_three_rounds() {
    let w = Gw::new();
    for want in ["1", "2", "3"] {
        let g = w.gate();
        assert_eq!(code(&g), 1, "stderr {}", err(&g));
        assert_eq!(get(&w.st(), "fix_rounds"), want, "after {want} FAIL run(s)");
    }
    let no_caps = run_sub("status", &w.other, &args_for(&w.tf, &w.repo, w.res.to_str().unwrap(), w.caps.to_str().unwrap())[..5]);
    refused(&no_caps, 2, "twin: no --caps-dir");
    assert!(err(&no_caps).contains("--caps-dir is required"), "{}", err(&no_caps));
}

// AC 20: a ticket file the strict reader refuses is exit 1 (as `ticket intake`); a missing ticket file is exit 2. Twin: a valid ticket file.
#[test]
fn ac20_a_malformed_ticket_file_is_exit_1_as_in_intake() {
    let fx = Fx::at(1);
    let valid = ticket_json("r", "EX-1", false);
    let padded = |total: usize| format!("{valid}{}", " ".repeat(total - valid.len()));
    let tf = fx.ticket(&padded(MIB));
    assert_eq!(code(&status_with(&fx, &tf, fx.res.to_str().unwrap(), &fx.other)), 0, "twin: a valid ticket file, padded to exactly 1 MiB");
    let bad: Vec<(&str, Vec<u8>)> = vec![
        ("not JSON", b"nope".to_vec()),
        ("unknown key", valid.replace("\"risk\"", "\"extra\":\"x\",\"risk\"").into_bytes()),
        ("over 1 MiB", padded(MIB + 1).into_bytes()),
        ("non-UTF-8", vec![0xff, 0xfe, b'{']),
    ];
    for (label, body) in &bad {
        let f = fx.t.0.join(format!("bad{}.json", SEQ.fetch_add(1, Ordering::SeqCst)));
        fs::write(&f, body).unwrap();
        let o = status_with(&fx, &f, fx.res.to_str().unwrap(), &fx.other);
        refused(&o, 1, label);
        let intake = run_sub("ticket", &fx.other, &["intake".to_string(), f.to_str().unwrap().to_string(), "--repo-map".to_string(), format!("r={}", fx.repo.display())]);
        assert_eq!(code(&intake), 1, "{label}: `ticket intake` agrees");
    }
    let missing = fx.t.0.join("no-such-ticket.json");
    refused(&status_with(&fx, &missing, fx.res.to_str().unwrap(), &fx.other), 2, "missing ticket file");
}

// AC 21: with two faults at once the first in the stated order wins. Each fault alone is covered by AC 5, 8, 9, 15.
#[test]
fn ac21_the_first_fault_in_order_wins() {
    let fx = Fx::at(1);
    let res = fx.res.to_str().unwrap().to_string();
    // id rule (exit 2) before intake (exit 1).
    let tf = fx.ticket(&ticket_json("zz", "../escape", false));
    refused(&status_with(&fx, &tf, &res, &fx.other), 2, "bad id on an unmapped repo");
    // intake (exit 1) before the result directory (exit 2).
    let tf = fx.ticket(&ticket_json("zz", "EX-1", false));
    refused(&status_with(&fx, &tf, "relative-dir", &fx.other), 1, "unmapped repo with a relative result dir");
    // the command line (exit 2) before parsing the ticket (exit 1).
    let bad = fx.t.0.join("bad.json");
    fs::write(&bad, "nope").unwrap();
    let o = run_sub("status", &fx.other, &[bad.to_str().unwrap().to_string(), "--repo-map".to_string(), format!("r={}", fx.repo.display()), "--result-dir".to_string()]);
    refused(&o, 2, "malformed ticket and a missing --result-dir value");
}

// AC 8: the caps folder joins the order after the result folder and before the note, and the record comes after the note. Each pair of faults gives
// the FIRST one's message, and the folder's SHAPE is judged after intake (not in the command line), as for the result folder.
#[test]
fn ac21b_the_caps_faults_keep_their_place_in_the_order() {
    let fx = Fx::at(1);
    let res = fx.res.to_str().unwrap().to_string();
    let cp = fx.caps.to_str().unwrap().to_string();
    let rel_res = status_caps_res(&fx, "relative-res", "relative-caps");
    refused(&rel_res, 2, "bad result dir and bad caps dir");
    assert!(err(&rel_res).contains("--result-dir must be"), "the result dir wins: {}", err(&rel_res));
    // bad caps dir + malformed note: the caps message.
    fx.put_bare("EX-1", "not json");
    let tf = fx.ticket(&ticket_json("r", "EX-1", false));
    let o = status_caps(&fx, &tf, "relative-caps");
    refused(&o, 2, "bad caps dir and malformed note");
    assert!(err(&o).contains("--caps-dir must be"), "the caps dir wins: {}", err(&o));
    // malformed note + refused record: the note message.
    fs::write(fx.caps.join("EX-1.rounds"), "not a record").unwrap();
    let o = status_caps(&fx, &tf, &cp);
    refused(&o, 2, "malformed note and refused record");
    assert!(err(&o).contains("the result file is malformed"), "the note wins: {}", err(&o));
    // twin: a valid note and the same refused record gives the record message.
    fx.put_bare("EX-1", &fail_note(&fx.c[1]).to_json().unwrap());
    let o = status_caps(&fx, &tf, &cp);
    refused(&o, 2, "refused record");
    assert!(err(&o).contains("cap record"), "{}", err(&o));
    // intake (exit 1) before the caps dir (exit 2).
    let unmapped = fx.ticket(&ticket_json("zz", "EX-1", false));
    refused(&status_caps(&fx, &unmapped, "relative-caps"), 1, "unmapped repo and bad caps dir");
    // the shape is judged after the ticket parse: a malformed ticket with a RELATIVE --caps-dir is exit 1 (a parse-time shape check would be 2).
    let bad = fx.t.0.join("bad.json");
    fs::write(&bad, "nope").unwrap();
    refused(&status_caps(&fx, &bad, "relative-caps"), 1, "malformed ticket and a relative caps dir");
    // a command-line fault (repeated --caps-dir) beats a malformed ticket.
    let o = run_sub("status", &fx.other, &[bad.to_str().unwrap().to_string(), "--repo-map".to_string(), format!("r={}", fx.repo.display()), "--result-dir".to_string(), res, "--caps-dir".to_string(), cp.clone(), "--caps-dir".to_string(), cp]);
    refused(&o, 2, "malformed ticket and a repeated --caps-dir");
}

/// Status with a given result dir argument and caps dir argument, for the ticket `EX-1`.
fn status_caps_res(fx: &Fx, res: &str, caps: &str) -> Output {
    let tf = fx.ticket(&ticket_json("r", "EX-1", false));
    run_sub("status", &fx.other, &args_for(&tf, &fx.repo, res, caps))
}

// AC 1/2: the printed rounds are the record's `rounds` over every valid state; a state no writer produces is refused, never printed.
#[test]
fn ac22_the_rounds_are_fix_rounds_plus_a_run_in_flight() {
    let fx = Fx::at(1);
    assert_eq!(get(&status(&fx, "EX-1"), "fix_rounds"), "0", "no note and no record");
    let mut seen = 0;
    for fr in 0..=4u8 {
        for fl in [false, true] {
            if fr == 4 && fl {
                continue;
            }
            seen += 1;
            fx.record("EX-1", fr, fl);
            assert_eq!(get(&status(&fx, "EX-1"), "fix_rounds"), (fr + u8::from(fl)).to_string(), "fix_rounds {fr}, in flight {fl}");
        }
    }
    assert_eq!(seen, 9);
    fx.record("EX-1", 4, true);
    refused(&status(&fx, "EX-1"), 2, "fix_rounds 4 with a run in flight is not a valid state");
    fx.record("EX-1", 4, false);
    assert_eq!(get(&status(&fx, "EX-1"), "fix_rounds"), "4", "twin: the same record without a run in flight");
}

// AC 3: no record + a note is exit 2 with the gate's meaning; no record and no note is 0. Twin: the record beside the note.
#[test]
fn ac23_a_note_without_its_record_is_refused() {
    let fx = Fx::at(1);
    fx.put_bare("EX-1", &fail_note(&fx.c[1]).to_json().unwrap());
    let o = status(&fx, "EX-1");
    refused(&o, 2, "note without record");
    assert!(err(&o).contains("a result file exists but the cap record is missing"), "{}", err(&o));
    fx.record("EX-1", 1, false);
    assert_eq!(get(&status(&fx, "EX-1"), "fix_rounds"), "1", "twin: the record beside the note");
    fs::remove_file(fx.res.join("EX-1.json")).unwrap();
    fs::remove_file(fx.caps.join("EX-1.rounds")).unwrap();
    assert_eq!(get(&status(&fx, "EX-1"), "fix_rounds"), "0", "no note and no record");
}

// AC 4: every record the public reader refuses is exit 2, stdout empty and no folder text. No note is planted, so a build that read a refused record
// as 0 would exit 0 here instead of hiding behind the missing-record refusal. Twin: the canonical record.
#[test]
fn ac24_a_record_the_reader_refuses_is_refused() {
    let fx = Fx::at(1);
    let good = record_bytes("EX-1", "r", 2, false);
    let rec = fx.caps.join("EX-1.rounds");
    fs::write(&rec, &good).unwrap();
    assert_eq!(get(&status(&fx, "EX-1"), "fix_rounds"), "2", "twin: the canonical record");
    let bodies: Vec<(&str, Vec<u8>)> = vec![
        ("over 4 KiB", vec![b'x'; 4097]),
        ("for another ticket", record_bytes("EX-2", "r", 2, false).into_bytes()),
        ("for another repo id", record_bytes("EX-1", "q", 2, false).into_bytes()),
        ("reordered keys", b"{\"v\":1,\"repo_id\":\"r\",\"ticket\":\"EX-1\",\"fix_rounds\":2,\"in_flight\":false}\n".to_vec()),
        ("padded with spaces", format!("{{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\", \"fix_rounds\":2,\"in_flight\":false}}\n").into_bytes()),
        ("fix_rounds 5", record_bytes("EX-1", "r", 5, false).into_bytes()),
        ("fix_rounds 256", record_bytes("EX-1", "r", 0, false).replace("\"fix_rounds\":0", "\"fix_rounds\":256").into_bytes()),
        ("fix_rounds negative", record_bytes("EX-1", "r", 0, false).replace("\"fix_rounds\":0", "\"fix_rounds\":-1").into_bytes()),
        ("no trailing newline", good.trim_end().as_bytes().to_vec()),
        ("empty", Vec::new()),
    ];
    for (label, body) in &bodies {
        fs::write(&rec, body).unwrap();
        let o = status(&fx, "EX-1");
        refused(&o, 2, label);
        assert!(!leaks(&o, fx.caps.to_str().unwrap()), "{label}: the caps folder must not be echoed: {}", err(&o));
    }
    // a link to a valid record, and a directory in its place.
    fs::remove_file(&rec).unwrap();
    let real = fx.t.0.join("real-record");
    fs::write(&real, &good).unwrap();
    symlink(&real, &rec).unwrap();
    refused(&status(&fx, "EX-1"), 2, "record is a symlink");
    fs::remove_file(&rec).unwrap();
    fs::create_dir(&rec).unwrap();
    refused(&status(&fx, "EX-1"), 2, "record is a directory");
    fs::remove_dir(&rec).unwrap();
    // a differently-cased twin of the record name (by name, on every filesystem).
    fs::write(fx.caps.join("ex-1.rounds"), &good).unwrap();
    refused(&status(&fx, "EX-1"), 2, "case-twin record name");
}

// AC 5: the caps folder rules. The relative and the `..` rows RESOLVE to the real caps folder (holding a canonical record), so only the shape rule
// can refuse them. Twin: the absolute real folder outside the repo. A caps folder equal to the result folder is ACCEPTED (parity with the gate).
#[test]
fn ac25_a_caps_directory_that_is_not_a_real_outside_directory_is_refused() {
    let fx = Fx::at(1);
    fx.record("EX-1", 3, false);
    let tf = fx.ticket(&ticket_json("r", "EX-1", false));
    let cp = fx.caps.to_str().unwrap().to_string();
    assert_eq!(get(&status_caps(&fx, &tf, &cp), "fix_rounds"), "3", "twin: an absolute real directory outside the repo");
    // a relative path that really reaches a caps folder from the working folder (the unrelated repo).
    fs::create_dir_all(fx.other.join("caps-rel")).unwrap();
    fs::write(fx.other.join("caps-rel").join("EX-1.rounds"), record_bytes("EX-1", "r", 3, false)).unwrap();
    fs::create_dir_all(fx.repo.join("cin")).unwrap();
    fs::write(fx.repo.join("cin").join("EX-1.rounds"), record_bytes("EX-1", "r", 3, false)).unwrap();
    let link = fx.t.0.join("link-caps");
    symlink(&fx.caps, &link).unwrap();
    let into_repo = fx.t.0.join("lnk");
    symlink(&fx.repo, &into_repo).unwrap();
    let file = fx.t.0.join("caps-is-a-file");
    fs::write(&file, "x").unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("relative", "caps-rel".to_string()),
        ("dot-dot", format!("{}/../caps", fx.caps.display())),
        ("symlink to a real directory", link.display().to_string()),
        ("inside the repo", fx.repo.join("cin").display().to_string()),
        ("parent component is a link into the repo", into_repo.join("cin").display().to_string()),
        ("missing", fx.t.0.join("nope").display().to_string()),
        ("a file", file.display().to_string()),
    ];
    for (label, dir) in &cases {
        let o = status_caps(&fx, &tf, dir);
        refused(&o, 2, label);
        assert!(!leaks(&o, dir), "{label}: the caps folder must not be echoed: {}", err(&o));
    }
    // Decision 2: the caps folder may be the result folder (parity with the gate): the note and the record sit side by side.
    fx.put_bare("EX-1", &fail_note(&fx.c[1]).to_json().unwrap());
    fs::write(fx.res.join("EX-1.rounds"), record_bytes("EX-1", "r", 2, false)).unwrap();
    let same = status_caps(&fx, &tf, fx.res.to_str().unwrap());
    assert_eq!((code(&same), get(&same, "fix_rounds")), (0, "2".to_string()), "a caps folder equal to the result folder is accepted");
}
