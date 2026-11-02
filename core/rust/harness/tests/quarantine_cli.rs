//! Process-level tests for `harness quarantine sweep` and `harness quarantine delete-listed` (P1B-13, ticket #156).
//! Every check runs the real binary as a child process against a scratch workspace under the system temp folder, so
//! the exit-code contract (0 done, 2 refused, 3 cannot proceed), the write-ahead journal, the quarantined events,
//! the never-follow-a-symlink rule and the no-folder-on-the-terminal rule are all exercised end to end. The
//! workspace fixture is built the way `workspace create` builds one: the manifest entries are written BEFORE the
//! things they name exist (the node_modules entry precedes its link). Every refusal test also pins that NOTHING
//! moved (the root is unchanged, the quarantine shelf holds no batch).
#![cfg(unix)]
use harness::quarantine::{self, SweepArgs};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// One scratch layout: the RAW temp path (what a caller would type) and its canonical spelling are both kept,
/// because on this platform they differ and neither may ever appear on stdout or stderr.
struct Tmp {
    raw: PathBuf,
    canon: PathBuf,
}

impl Tmp {
    fn new() -> Tmp {
        let raw = std::env::temp_dir().join(format!("qcli-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&raw);
        fs::create_dir_all(&raw).unwrap();
        let canon = raw.canonicalize().unwrap();
        Tmp { raw, canon }
    }

    fn both(&self, rel: &str) -> (PathBuf, PathBuf) {
        (self.raw.join(rel), self.canon.join(rel))
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.raw);
        let _ = fs::remove_dir_all(&self.canon);
    }
}

/// The full P1b workspace shape for ticket `t-1`: workspace root `ws`, ticket root `ws/t-1` holding `tree` (with a
/// file inside, so "owned is skipped whole" is real), `tmp`, and the `node_modules` LINK inside `tree`; the manifest
/// lists worktree + tmp + node_modules (the link's entry is written BEFORE the link exists, the write-ahead order);
/// `md` (manifests), `q` (quarantine shelf) and `logdir` (event log) sit OUTSIDE the workspace root.
struct Fix {
    t: Tmp,
    root: PathBuf,
    ws: PathBuf,
    md: PathBuf,
    q: PathBuf,
    log: PathBuf,
    outside: PathBuf,
}

fn fixture() -> Fix {
    let t = Tmp::new();
    let (ws, _) = t.both("ws");
    let root = ws.join("t-1");
    let (md, _) = t.both("md");
    let (q, _) = t.both("q");
    let (logdir, _) = t.both("logdir");
    let (outside, _) = t.both("outside");
    fs::create_dir_all(root.join("tree/sub")).unwrap();
    fs::write(root.join("tree/sub/owned.txt"), b"owned-by-tree").unwrap();
    fs::create_dir_all(root.join("tmp")).unwrap();
    fs::write(root.join("tmp/scratch.txt"), b"scratch").unwrap();
    fs::create_dir_all(&md).unwrap();
    fs::create_dir_all(&q).unwrap();
    fs::create_dir_all(&logdir).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), b"outside-payload").unwrap();
    // The manifest entries, written BEFORE the things they name (the node_modules link is made only after its
    // entry exists — a listed half-made thing, never an orphan).
    ok(&["manifest", "init", "t-1", "--root", &s(&root), "--dir", &s(&md)]);
    ok(&["manifest", "add", "t-1", "--class", "worktree", "--id", "worktree", "--path", &s(&root.join("tree")), "--dir", &s(&md)]);
    ok(&["manifest", "add", "t-1", "--class", "temp_dir", "--id", "tmp", "--path", &s(&root.join("tmp")), "--dir", &s(&md)]);
    ok(&["manifest", "add", "t-1", "--class", "temp_dir", "--id", "node_modules", "--path", &s(&root.join("tree/node_modules")), "--dir", &s(&md)]);
    std::os::unix::fs::symlink(&outside, root.join("tree/node_modules")).unwrap();
    Fix { t, root, ws, md, q, log: logdir.join("events.jsonl"), outside }
}

/// Plant one unmanifested dir with one file under the ticket root.
fn plant(f: &Fix, name: &str, payload: &str) -> PathBuf {
    let d = f.root.join(name);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("one.txt"), payload.as_bytes()).unwrap();
    d
}

fn run(args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
    for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL", "HARNESS_CONFIG"] {
        c.env_remove(k);
    }
    c.output().unwrap()
}

fn ok(args: &[&str]) {
    let o = run(args);
    assert!(o.status.success(), "fixture call {:?} failed: {}", args, String::from_utf8_lossy(&o.stderr));
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

fn s(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

/// One `quarantine sweep` command line for the fixture, with optional extra `--flag value` pairs.
fn sweep(f: &Fix, extra: &[(&str, &str)]) -> Output {
    let mut v: Vec<String> = vec![
        "quarantine".into(),
        "sweep".into(),
        "t-1".into(),
        "--manifest-dir".into(),
        s(&f.md),
        "--workspace-root".into(),
        s(&f.ws),
        "--quarantine-dir".into(),
        s(&f.q),
        "--log".into(),
        s(&f.log),
        "--repo".into(),
        "r-1".into(),
        "--role".into(),
        "executor".into(),
        "--model".into(),
        "m-1".into(),
    ];
    for (k, val) in extra {
        v.push(k.to_string());
        v.push(val.to_string());
    }
    let refs: Vec<&str> = v.iter().map(|x| x.as_str()).collect();
    run(&refs)
}

/// One `quarantine sweep` command line for the fixture with ONE base flag's value REPLACED — a single
/// occurrence of the flag, never a second appended one: a duplicate is refused as "an option was given twice"
/// by the parser before any placement rule fires, which is exactly the hollow-test bug of review r1 (F4).
fn sweep_replacing(f: &Fix, flag: &str, value: &str) -> Output {
    let pairs: Vec<(&str, String)> = vec![
        ("--manifest-dir", s(&f.md)),
        ("--workspace-root", s(&f.ws)),
        ("--quarantine-dir", s(&f.q)),
        ("--log", s(&f.log)),
        ("--repo", "r-1".into()),
        ("--role", "executor".into()),
        ("--model", "m-1".into()),
    ];
    let mut v: Vec<String> = vec!["quarantine".into(), "sweep".into(), "t-1".into()];
    for (k, val) in &pairs {
        v.push(k.to_string());
        v.push(if *k == flag { value.to_string() } else { val.clone() });
    }
    let refs: Vec<&str> = v.iter().map(|x| x.as_str()).collect();
    run(&refs)
}

/// Hand-append ONE raw line to the ticket's manifest — a line this module's writer would refuse (the escape
/// fixtures of the delete fence: the reader accepts the shape, so the delete verb must fence it itself).
fn append_manifest_line(f: &Fix, line: &str) {
    let mut file = fs::OpenOptions::new().append(true).open(f.md.join("t-1.jsonl")).unwrap();
    file.write_all(format!("{line}\n").as_bytes()).unwrap();
}

/// The batch directories of the fixture's quarantine shelf, sorted by name.
fn batches(f: &Fix) -> Vec<PathBuf> {
    let shelf = f.q.join("t-1");
    let mut v: Vec<PathBuf> = fs::read_dir(&shelf)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("b-")).unwrap_or(false))
        .collect();
    v.sort();
    v
}

/// The `quarantined` lines of the fixture's event log, through the real `events list`.
fn quarantined_lines(f: &Fix) -> Vec<String> {
    let o = run(&["events", "list", "--log", &s(&f.log)]);
    assert!(o.status.success(), "events list failed: {}", err(&o));
    out(&o).lines().filter(|l| l.contains("\"kind\":\"quarantined\"")).map(|l| l.to_string()).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// AC 1: a planted unmanifested dir with a file lands under `<quarantine-dir>/<ticket>/<batch>/i-1`,
/// byte-identical, and is gone from the root. (RED on a delete-instead-of-move build: the payload would not exist
/// anywhere — the quarantine item would be absent.)
#[test]
fn unmanifested_dir_is_moved_and_still_exists() {
    let f = fixture();
    let stray = plant(&f, "stray-1", "payload-one");
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "quarantine=done ticket=t-1 moved=1\n");
    assert!(!stray.exists(), "the stray is gone from the root");
    let bs = batches(&f);
    assert_eq!(bs.len(), 1, "exactly one batch");
    let item = bs[0].join("i-1");
    assert_eq!(fs::read(item.join("one.txt")).unwrap(), b"payload-one", "byte-identical payload on the shelf");
    assert!(item.join("journal.jsonl").exists() || bs[0].join("journal.jsonl").exists(), "the journal exists");
}

/// AC 2: exactly one `quarantined` event per moved item, reason `unmanifested`, `to` naming the item; `events
/// verify` exits 0. (RED on a build that writes no event.)
#[test]
fn quarantined_event_written_per_item() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    plant(&f, "stray-2", "two");
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("moved=2"));
    let lines = quarantined_lines(&f);
    assert_eq!(lines.len(), 2, "exactly one quarantined event per moved item: {lines:?}");
    let mut tos: Vec<&str> = lines.iter().map(|l| l.split("\"to\":\"").nth(1).unwrap().split('"').next().unwrap()).collect();
    tos.sort();
    assert!(tos.iter().all(|t| t.starts_with("t-1/b-") && (t.ends_with("/i-1") || t.ends_with("/i-2"))), "to names the item: {tos:?}");
    assert!(tos[0] != tos[1], "two distinct items");
    assert!(lines.iter().all(|l| l.contains("\"reason\":\"unmanifested\"")));
    assert!(lines.iter().all(|l| l.contains("\"from\":\"t-1/stray-")));
    assert_eq!(code(&run(&["events", "verify", "--log", &s(&f.log)])), 0);
}

/// AC 3: `tree`, `tmp`, a listed file and the listed `node_modules` link stay in place, unchanged. (RED on a build
/// that moves a manifested path: an owned thing would be gone from the root.)
#[test]
fn manifested_paths_are_never_moved() {
    let f = fixture();
    // a listed FILE as well: evidence-class style entry inside the tree's parent
    ok(&["manifest", "add", "t-1", "--class", "log", "--id", "runlog", "--path", &s(&f.root.join("run.log")), "--dir", &s(&f.md)]);
    fs::write(f.root.join("run.log"), b"log-bytes").unwrap();
    plant(&f, "stray-1", "one");
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("moved=1"), "only the stray: {}", out(&o));
    assert_eq!(fs::read(f.root.join("tree/sub/owned.txt")).unwrap(), b"owned-by-tree", "owned is skipped WHOLE");
    assert_eq!(fs::read(f.root.join("tmp/scratch.txt")).unwrap(), b"scratch");
    assert_eq!(fs::read(f.root.join("run.log")).unwrap(), b"log-bytes", "a listed file stays");
    let meta = fs::symlink_metadata(f.root.join("tree/node_modules")).unwrap();
    assert!(meta.file_type().is_symlink(), "the listed node_modules link stays a link");
    assert_eq!(fs::read_link(f.root.join("tree/node_modules")).unwrap(), f.outside, "the link still points where it pointed");
}

/// AC 4: a symlink in the root pointing to a dir outside the roots is moved AS A LINK; the outside target dir and
/// its contents are unchanged. (RED on a build that follows the link: the target's contents would be gone.)
#[test]
fn symlink_moved_as_link_target_untouched() {
    let f = fixture();
    std::os::unix::fs::symlink(&f.outside, f.root.join("link-1")).unwrap();
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("moved=1"));
    let item = batches(&f)[0].join("i-1");
    let meta = fs::symlink_metadata(&item).unwrap();
    assert!(meta.file_type().is_symlink(), "the shelf item is the LINK itself, not the target");
    assert!(!meta.is_dir(), "moved as the link, never descended");
    assert_eq!(fs::read_link(&item).unwrap(), f.outside, "the link text is unchanged");
    assert_eq!(fs::read(f.outside.join("keep.txt")).unwrap(), b"outside-payload", "the target's contents are untouched");
    assert_eq!(fs::read_dir(&f.outside).unwrap().count(), 1, "nothing was added to or removed from the target");
    assert!(!fs::symlink_metadata(f.root.join("link-1")).is_ok(), "the link is gone from the root");
}

/// AC 5: sibling dirs of the ticket root under the workspace root, and a planted dir beside the quarantine folder,
/// are unchanged (same entries, same bytes, same mtimes) after a sweep.
#[test]
fn nothing_outside_roots_touched() {
    let f = fixture();
    let sibling = f.ws.join("t-2");
    fs::create_dir_all(&sibling).unwrap();
    fs::write(sibling.join("sib.txt"), b"sibling-payload").unwrap();
    let beside = f.q.parent().unwrap().join("q-sibling");
    fs::create_dir_all(beside.join("keep")).unwrap();
    fs::write(beside.join("keep/one.txt"), b"beside-payload").unwrap();
    plant(&f, "stray-1", "one");
    let watch = [sibling.join("sib.txt"), beside.join("keep/one.txt")];
    let before: Vec<(Vec<u8>, std::time::SystemTime)> = watch
        .iter()
        .map(|p| (fs::read(p).unwrap(), fs::metadata(p).unwrap().modified().unwrap()))
        .collect();
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("moved=1"));
    for (i, p) in watch.iter().enumerate() {
        assert!(p.exists(), "{} survives", p.file_name().unwrap().to_string_lossy());
        assert_eq!(fs::read(p).unwrap(), before[i].0, "same bytes");
        assert_eq!(fs::metadata(p).unwrap().modified().unwrap(), before[i].1, "same mtime (never written)");
    }
    assert_eq!(fs::read_dir(&sibling).unwrap().count(), 1);
    assert_eq!(fs::read_dir(&beside.join("keep")).unwrap().count(), 1);
}

/// AC 6: two sweeps with a leftover of the same name each time: both copies exist in two different batches, the
/// first unchanged. (Nothing already on the shelf is ever renamed onto, replaced or removed.)
#[test]
fn earlier_quarantine_never_overwritten() {
    let f = fixture();
    plant(&f, "dup", "first-copy");
    let o1 = sweep(&f, &[]);
    assert_eq!(code(&o1), 0, "stderr: {}", err(&o1));
    assert!(out(&o1).contains("moved=1"));
    plant(&f, "dup", "second-copy");
    let o2 = sweep(&f, &[]);
    assert_eq!(code(&o2), 0, "stderr: {}", err(&o2));
    assert!(out(&o2).contains("moved=1"));
    let bs = batches(&f);
    assert_eq!(bs.len(), 2, "two batches, one per sweep");
    assert_eq!(fs::read(bs[0].join("i-1/one.txt")).unwrap(), b"first-copy", "the first copy is unchanged");
    assert_eq!(fs::read(bs[1].join("i-1/one.txt")).unwrap(), b"second-copy", "the second copy sits in a fresh batch");
}

/// AC 7: a manifest whose root is a symlink, or resolves outside `--workspace-root`, exits 2 and nothing moves.
#[test]
fn root_symlink_or_outside_base_refused() {
    // (a) the ticket root itself is a symlink. The link points at a REAL dir INSIDE the workspace root, so the
    // outside-base rule cannot catch it: only the root-is-symlink rule refuses this (review r1, F5 — a link
    // pointing outside was refused twice over, so the symlink rule itself was never pinned).
    let f = fixture();
    plant(&f, "stray-1", "one");
    let inside = f.ws.join("inside-target");
    fs::create_dir_all(&inside).unwrap();
    fs::write(inside.join("inner.txt"), b"inside-payload").unwrap();
    fs::remove_dir_all(&f.root).unwrap();
    std::os::unix::fs::symlink(&inside, &f.root).unwrap();
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 2, "a root that IS a symlink is refused: {}{}", out(&o), err(&o));
    assert!(err(&o).contains("symlink"), "the refusal names the root-is-symlink rule itself: {}", err(&o));
    assert!(!f.q.join("t-1").exists(), "no shelf was made, nothing moved");
    assert!(inside.join("inner.txt").exists(), "the link target inside the workspace is untouched");
    // (b) the manifest's root resolves outside the workspace root
    let f2 = fixture();
    let (elsewhere, _) = f2.t.both("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let (md2, _) = f2.t.both("md2");
    fs::create_dir_all(&md2).unwrap();
    ok(&["manifest", "init", "t-1", "--root", &s(&elsewhere), "--dir", &s(&md2)]);
    let args = vec![
        "quarantine".to_string(),
        "sweep".to_string(),
        "t-1".to_string(),
        "--manifest-dir".to_string(),
        s(&md2),
        "--workspace-root".to_string(),
        s(&f2.ws),
        "--quarantine-dir".to_string(),
        s(&f2.q),
        "--log".to_string(),
        s(&f2.log),
        "--repo".to_string(),
        "r-1".to_string(),
        "--role".to_string(),
        "executor".to_string(),
        "--model".to_string(),
        "m-1".to_string(),
    ];
    let refs: Vec<&str> = args.iter().map(|x| x.as_str()).collect();
    let o = run(&refs);
    assert_eq!(code(&o), 2, "a root outside the base is refused: {}{}", out(&o), err(&o));
    assert!(!f2.q.join("t-1").exists(), "nothing moved");
}

/// AC 8: a quarantine dir inside a root, equal to the manifest dir, or relative exits 2, the stderr line names
/// the placement problem, and nothing moves. Each case runs the base argv with the ONE `--quarantine-dir`
/// occurrence's value REPLACED (review r1, F4: the first draft appended a second flag, so every case was
/// refused as "an option was given twice" before the placement rule fired — a hollow test that survived the
/// M11 mutation).
#[test]
fn misplaced_quarantine_dir_refused() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    // (a) inside a root: the ticket root itself
    let inner = f.root.join("q-inner");
    fs::create_dir_all(&inner).unwrap();
    let o = sweep_replacing(&f, "--quarantine-dir", &s(&inner));
    assert_eq!(code(&o), 2, "a quarantine dir inside a root is refused: {}{}", out(&o), err(&o));
    assert!(err(&o).contains("outside every root"), "the refusal names the placement problem: {}", err(&o));
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    assert!(!f.q.join("t-1").exists(), "no shelf");
    // (b) equal to the manifest dir
    let o = sweep_replacing(&f, "--quarantine-dir", &s(&f.md));
    assert_eq!(code(&o), 2, "a quarantine dir equal to the manifest dir is refused: {}{}", out(&o), err(&o));
    assert!(err(&o).contains("outside each other"), "the refusal names the placement problem: {}", err(&o));
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    // (c) relative
    let o = sweep_replacing(&f, "--quarantine-dir", "just-a-name");
    assert_eq!(code(&o), 2, "a relative quarantine dir is refused: {}{}", out(&o), err(&o));
    assert!(err(&o).contains("absolute path"), "the refusal names the placement problem: {}", err(&o));
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    assert!(!f.q.join("t-1").exists(), "no shelf");
}

/// AC 9: with `--max-items 3` and 4 leftovers, exit 3, a stderr line naming the cap, and all 4 are still in the
/// root. A malformed `--max-items` is a usage refusal (exit 2) — every user-supplied number is validated before
/// anything is written.
#[test]
fn over_cap_fails_closed() {
    let f = fixture();
    for n in 1..=4 {
        plant(&f, &format!("stray-{n}"), "payload");
    }
    let o = sweep(&f, &[("--max-items", "3")]);
    assert_eq!(code(&o), 3, "over the cap is exit 3: {}{}", out(&o), err(&o));
    assert!(err(&o).contains("max-items"), "the stderr line names the cap: {}", err(&o));
    for n in 1..=4 {
        assert!(f.root.join(format!("stray-{n}/one.txt")).exists(), "stray-{n} is still in the root");
    }
    assert!(!f.q.join("t-1").exists(), "no shelf was made");
    let bad = sweep(&f, &[("--max-items", "3x")]);
    assert_eq!(code(&bad), 2, "a malformed number is a usage refusal: {}", err(&bad));
}

/// AC 10: a second sweep exits 0, moves nothing, writes no event; a planted journal intent whose item is in the
/// batch and source gone, with no event, gets exactly one event written; an intent with both source and item
/// present exits 3. The journal is the crash trace: one intent per moved item, written before the move (a torn
/// journal line never precedes a move, and reconcile decides from intents, not from guesses).
#[test]
fn rerun_is_idempotent_and_recovers_crash() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    let o1 = sweep(&f, &[]);
    assert_eq!(code(&o1), 0, "stderr: {}", err(&o1));
    assert_eq!(quarantined_lines(&f).len(), 1);
    // the journal holds one intent per moved item
    let batch = batches(&f)[0].clone();
    let journal = fs::read_to_string(batch.join("journal.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 1);
    assert!(journal.contains("\"item\":\"i-1\""));
    // idempotent re-run: nothing moves, no event
    let o2 = sweep(&f, &[]);
    assert_eq!(code(&o2), 0, "stderr: {}", err(&o2));
    assert_eq!(out(&o2), "quarantine=done ticket=t-1 moved=0\n");
    assert_eq!(quarantined_lines(&f).len(), 1, "no new event on the re-run");
    assert_eq!(batches(&f).len(), 1, "no new batch on the re-run");
    // crash recovery: an intent whose item exists and whose source is gone, with no matching event
    let shelf = f.q.join("t-1");
    let crash_batch = shelf.join("b-99-9-77");
    fs::create_dir_all(crash_batch.join("i-1")).unwrap();
    fs::write(crash_batch.join("i-1/gone.txt"), b"gone-payload").unwrap();
    fs::write(
        crash_batch.join("journal.jsonl"),
        format!("{{\"item\":\"i-1\",\"src_hex\":\"{}\",\"reason\":\"unmanifested\"}}\n", hex(b"t-1/vanished")),
    )
    .unwrap();
    let o3 = sweep(&f, &[]);
    assert_eq!(code(&o3), 0, "stderr: {}", err(&o3));
    let lines = quarantined_lines(&f);
    assert_eq!(lines.len(), 2, "exactly one event written for the recovered intent: {lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("\"to\":\"t-1/b-99-9-77/i-1\"") && l.contains("\"from\":\"t-1/vanished\"")),
        "the recovered event names the item and the source: {lines:?}"
    );
    // undecidable: an intent whose source AND item both exist
    plant(&f, "amb", "still-here");
    let amb_batch = shelf.join("b-99-9-78");
    fs::create_dir_all(amb_batch.join("i-1")).unwrap();
    fs::write(
        amb_batch.join("journal.jsonl"),
        format!("{{\"item\":\"i-1\",\"src_hex\":\"{}\",\"reason\":\"unmanifested\"}}\n", hex(b"t-1/amb")),
    )
    .unwrap();
    let o4 = sweep(&f, &[]);
    assert_eq!(code(&o4), 3, "an undecidable intent is exit 3: {}{}", out(&o4), err(&o4));
    assert!(f.root.join("amb/one.txt").exists(), "nothing moved after the undecidable intent");
}

/// AC 11: a torn-tail manifest exits 2, nothing moves.
#[test]
fn damaged_manifest_refused() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    let mfile = f.md.join("t-1.jsonl");
    let mut text = fs::read_to_string(&mfile).unwrap();
    text.push_str("{\"v\":1");
    fs::write(&mfile, text).unwrap();
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 2, "a torn manifest is refused: {}{}", out(&o), err(&o));
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    assert!(!f.q.join("t-1").exists(), "no shelf");
}

/// AC 12: `delete-listed --id tmp` removes the listed temp dir and writes a manifest `remove`; a planted symlink
/// inside it pointing out of the roots is unlinked and its target survives; `--id worktree` (class `worktree`) and
/// an unknown id exit 2 with nothing deleted. A removal that fails leaves the manifest untouched (the `remove`
/// line is written only AFTER the path is gone: a crash between leaves a listed half-deleted thing, never an orphan).
#[test]
fn delete_listed_temp_dir() {
    let f = fixture();
    std::os::unix::fs::symlink(&f.outside, f.root.join("tmp/out-link")).unwrap();
    fs::write(f.root.join("tmp/inside.txt"), b"temp-payload").unwrap();
    // a nested subtree, three levels deep with files at every level and a link to an outside folder at the
    // middle level: the removal is POST-ORDER (a dir only after its children), so the whole tree goes in ONE
    // run (review r1, F1: the first draft removed a parent while its children were still queued — ENOTEMPTY,
    // exit 3, no manifest remove, wedged on every re-run)
    fs::write(f.root.join("tmp/l0.txt"), b"l0").unwrap();
    fs::create_dir_all(f.root.join("tmp/mid/lower")).unwrap();
    fs::write(f.root.join("tmp/mid/l1.txt"), b"l1").unwrap();
    fs::write(f.root.join("tmp/mid/lower/l2.txt"), b"l2").unwrap();
    std::os::unix::fs::symlink(&f.outside, f.root.join("tmp/mid/out-link")).unwrap();
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "tmp", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "delete=done ticket=t-1 id=tmp\n");
    assert!(!f.root.join("tmp").exists(), "the listed temp dir is gone, subtree and all");
    assert!(f.outside.join("keep.txt").exists(), "the link's TARGET survives the unlink");
    assert_eq!(fs::read(f.outside.join("keep.txt")).unwrap(), b"outside-payload");
    assert_eq!(fs::read_dir(&f.outside).unwrap().count(), 1, "nothing was added to or removed from the target");
    // the manifest remove line was written: tmp is no longer live, the others still are
    let listed = out(&run(&["manifest", "list", "t-1", "--dir", &s(&f.md)]));
    assert!(!listed.lines().any(|l| l.starts_with("tmp\t")), "tmp is no longer live: {listed}");
    assert!(listed.lines().any(|l| l.starts_with("worktree\t")));
    assert!(listed.lines().any(|l| l.starts_with("node_modules\t")));
    // wrong class
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "worktree", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 2, "a worktree entry is not deletable here: {}{}", out(&o), err(&o));
    assert!(f.root.join("tree").exists(), "nothing deleted");
    // unknown id
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "nope", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 2, "an unknown id is refused: {}{}", out(&o), err(&o));
    assert!(f.root.join("tree").exists(), "nothing deleted");
    // order pin: a deletion that CANNOT proceed leaves the manifest untouched (remove written after the path is gone)
    let f2 = fixture();
    fs::write(f2.root.join("tmp/inside.txt"), b"locked").unwrap();
    fs::set_permissions(f2.root.join("tmp"), fs::Permissions::from_mode(0o555)).unwrap();
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "tmp", "--manifest-dir", &s(&f2.md)]);
    assert_eq!(code(&o), 3, "an undeletable listed folder is exit 3: {}{}", out(&o), err(&o));
    let listed = out(&run(&["manifest", "list", "t-1", "--dir", &s(&f2.md)]));
    assert!(listed.lines().any(|l| l.starts_with("tmp\t")), "the remove line was NOT written before the deletion: {listed}");
    fs::set_permissions(f2.root.join("tmp"), fs::Permissions::from_mode(0o755)).unwrap();
    // an EMPTY listed temp dir deletes cleanly (exit 0, the remove line written)
    let f3 = fixture();
    fs::create_dir_all(f3.root.join("empty-tmp")).unwrap();
    ok(&["manifest", "add", "t-1", "--class", "temp_dir", "--id", "empty", "--path", &s(&f3.root.join("empty-tmp")), "--dir", &s(&f3.md)]);
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "empty", "--manifest-dir", &s(&f3.md)]);
    assert_eq!(code(&o), 0, "an empty listed dir deletes cleanly: {}{}", out(&o), err(&o));
    assert!(!f3.root.join("empty-tmp").exists());
    let listed = out(&run(&["manifest", "list", "t-1", "--dir", &s(&f3.md)]));
    assert!(!listed.lines().any(|l| l.starts_with("empty\t")), "the remove line was written: {listed}");
    // a re-run on an ALREADY-ABSENT path (listed again) writes the remove and exits 0: the clean no-op
    ok(&["manifest", "add", "t-1", "--class", "temp_dir", "--id", "gone", "--path", &s(&f3.root.join("gone-tmp")), "--dir", &s(&f3.md)]);
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "gone", "--manifest-dir", &s(&f3.md)]);
    assert_eq!(code(&o), 0, "an absent listed path is a clean no-op: {}{}", out(&o), err(&o));
    let listed = out(&run(&["manifest", "list", "t-1", "--dir", &s(&f3.md)]));
    assert!(!listed.lines().any(|l| l.starts_with("gone\t")), "the remove line was written: {listed}");
}

/// AC 13: the device check refuses with exit 3 and moves nothing, exercised through a test seam that reports
/// differing device ids (the REAL two-filesystem case is not reproducible in CI — a real cross-device rename error
/// is still covered fail-closed by construction: any rename error stops the sweep with exit 3). The equal-ids arm
/// of the same seam proves the check passes a same-device sweep through.
#[test]
fn cross_device_refused() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    let args = SweepArgs {
        ticket: "t-1".to_string(),
        manifest_dir: f.md.clone(),
        workspace_root: f.ws.clone(),
        quarantine_dir: f.q.clone(),
        log: f.log.clone(),
        repo: "r-1".to_string(),
        role: "executor".to_string(),
        model: "m-1".to_string(),
        max_items: 1000,
        ts_ms: 1731000000123,
    };
    let q_canon = f.q.canonicalize().unwrap();
    let differing = |p: &Path| Some(if *p == q_canon { 1u64 } else { 2u64 });
    match quarantine::sweep_with_device_probe(&args, &differing) {
        Err(quarantine::Stop(3, line)) => assert!(line.contains("cross-device"), "names the cause: {line}"),
        other => panic!("expected exit 3, got {:?}", other.map_err(|st| st.1)),
    }
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    assert!(!f.q.join("t-1").exists(), "no shelf was made");
    // the same seam with equal ids lets the sweep through
    let equal = |_p: &Path| Some(7u64);
    match quarantine::sweep_with_device_probe(&args, &equal) {
        Ok(line) => assert!(line.contains("moved=1"), "{line}"),
        other => panic!("expected success, got {:?}", other.map_err(|s| s.1)),
    }
    assert!(!f.root.join("stray-1").exists(), "the stray moved");
}

/// AC 14 (Linux only — the macOS filesystem refuses such names): a non-UTF-8 named leftover is moved, the event
/// uses the `untokenable` form, the journal holds its hex bytes, exit 0.
#[test]
#[cfg(target_os = "linux")]
fn non_utf8_name_no_panic() {
    use std::os::unix::ffi::OsStrExt;
    let f = fixture();
    let name = std::ffi::OsStr::from_bytes(b"\xff\xfe-dir");
    let d = f.root.join(name);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("one.txt"), b"raw-name").unwrap();
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("moved=1"));
    assert!(!d.exists(), "the non-utf8 leftover is gone from the root");
    let lines = quarantined_lines(&f);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("\"from\":\"t-1/untokenable/i-1\""), "the event uses the untokenable form: {}", lines[0]);
    let batch = batches(&f)[0].clone();
    let journal = fs::read_to_string(batch.join("journal.jsonl")).unwrap();
    let want = hex(b"t-1/\xff\xfe-dir");
    assert!(journal.contains(&format!("\"src_hex\":\"{want}\"")), "the journal holds the exact hex bytes: {journal}");
    assert!(batch.join("i-1/one.txt").exists(), "the item itself is on the shelf");
}

/// AC 15: on success and on each refusal, neither stream contains the workspace root, quarantine dir or manifest
/// dir string (given or canonical spelling).
#[test]
fn no_path_on_stdout_or_stderr() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    let mut outputs = vec![sweep(&f, &[])];
    // one occurrence of the flag, its value REPLACED: a second `--quarantine-dir` would be refused as "an
    // option was given twice" before the placement rule ever fires (the hollow-test bug of review r1, F4)
    outputs.push(sweep_replacing(&f, "--quarantine-dir", "relative-q"));
    // a root symlink refusal
    {
        let g = fixture();
        plant(&g, "stray-1", "one");
        fs::remove_dir_all(g.root.join("tree")).unwrap();
        fs::remove_dir_all(&g.root).unwrap();
        std::os::unix::fs::symlink(&g.outside, &g.root).unwrap();
        outputs.push(sweep(&g, &[]));
    }
    // a damaged manifest refusal
    {
        let g = fixture();
        plant(&g, "stray-1", "one");
        let mfile = g.md.join("t-1.jsonl");
        let mut text = fs::read_to_string(&mfile).unwrap();
        text.push_str("{\"v\":1");
        fs::write(&mfile, text).unwrap();
        outputs.push(sweep(&g, &[]));
    }
    // an over-cap exit 3
    {
        let g = fixture();
        for n in 1..=4 {
            plant(&g, &format!("stray-{n}"), "payload");
        }
        outputs.push(sweep(&g, &[("--max-items", "3")]));
    }
    for (i, o) in outputs.iter().enumerate() {
        for stream in [out(o), err(o)] {
            for secret in [
                f.ws.to_string_lossy().to_string(),
                f.ws.canonicalize().unwrap().to_string_lossy().to_string(),
                f.q.to_string_lossy().to_string(),
                f.q.canonicalize().unwrap().to_string_lossy().to_string(),
                f.md.to_string_lossy().to_string(),
                f.md.canonicalize().unwrap().to_string_lossy().to_string(),
            ] {
                assert!(!stream.contains(&secret), "output {i} leaks a folder path: {stream}");
            }
        }
    }
    // the success line is fixed fields only
    assert_eq!(out(&outputs[0]), "quarantine=done ticket=t-1 moved=1\n");
}

/// Review r1, F2: the delete fence is made on the RESOLVED path, never on its spelling. A `..` or `.` component
/// (raw text, because `Path` hides interior `.`), a symlinked intermediate component, and a root that is itself
/// a symlink are each refused with exit 2 and NOTHING outside the roots is touched. (Measured before the fix:
/// every case here escaped the lexical `starts_with` fence and deleted outside data with exit 0.)
#[test]
fn delete_listed_escaped_paths_refused() {
    // (R2) a hand-appended `<root>/../victim` line: a `..` component is refused before anything is examined.
    // The appended path uses the manifest's CANONICAL root spelling, exactly what the writer stores, so the
    // reader accepts the line and the delete verb faces the escape itself.
    let f = fixture();
    let canon_root = f.root.canonicalize().unwrap();
    let victim = f.ws.join("victim");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("v.txt"), b"victim-payload").unwrap();
    append_manifest_line(
        &f,
        &format!(
            "{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"esc\",\"class\":\"temp_dir\",\"path\":\"{}/../victim\",\"ts_ms\":1731000000123}}",
            s(&canon_root)
        ),
    );
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "esc", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 2, "a `..` component in a listed path is refused: {}{}", out(&o), err(&o));
    assert!(victim.join("v.txt").exists(), "nothing outside the roots was touched");
    assert!(f.root.join("tmp/scratch.txt").exists(), "nothing inside the roots was deleted either");
    // (must-fire) a trailing `/.`: `Path` normalizes it away, so the fence scans the RAW TEXT
    append_manifest_line(
        &f,
        &format!(
            "{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"dot\",\"class\":\"temp_dir\",\"path\":\"{}/tmp/.\",\"ts_ms\":1731000000124}}",
            s(&canon_root)
        ),
    );
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "dot", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 2, "a `.` component in a listed path is refused: {}{}", out(&o), err(&o));
    assert!(f.root.join("tmp/scratch.txt").exists(), "the spelled-inside tree is not deleted on a dotted spelling");
    // (R3) listed `tmp/deep` through a normal `manifest add`; `tmp` then swapped for a symlink to an outside dir
    let f2 = fixture();
    fs::create_dir_all(f2.root.join("tmp/deep")).unwrap();
    fs::write(f2.root.join("tmp/deep/p.txt"), b"deep").unwrap();
    ok(&["manifest", "add", "t-1", "--class", "temp_dir", "--id", "deep", "--path", &s(&f2.root.join("tmp/deep")), "--dir", &s(&f2.md)]);
    fs::remove_dir_all(f2.root.join("tmp")).unwrap();
    fs::create_dir_all(f2.outside.join("deep")).unwrap();
    fs::write(f2.outside.join("deep/p.txt"), b"outside-deep").unwrap();
    std::os::unix::fs::symlink(&f2.outside, f2.root.join("tmp")).unwrap();
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "deep", "--manifest-dir", &s(&f2.md)]);
    assert_eq!(code(&o), 2, "a listed path through a symlinked parent is refused: {}{}", out(&o), err(&o));
    assert!(f2.outside.join("deep/p.txt").exists(), "the outside data behind the swapped parent is intact");
    // (R4) the ticket root itself swapped for a symlink to an outside dir
    let f3 = fixture();
    fs::remove_dir_all(&f3.root).unwrap();
    fs::create_dir_all(f3.outside.join("tmp")).unwrap();
    fs::write(f3.outside.join("tmp/p.txt"), b"outside-root").unwrap();
    std::os::unix::fs::symlink(&f3.outside, &f3.root).unwrap();
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "tmp", "--manifest-dir", &s(&f3.md)]);
    assert_eq!(code(&o), 2, "a root that is a symlink is refused before any delete: {}{}", out(&o), err(&o));
    assert!(f3.outside.join("tmp/p.txt").exists(), "the outside data behind the swapped root is intact");
    // (must-fire, variant 3) a listed path that IS a symlink with a REAL parent is unlinked as the link only
    // (the entry is written BEFORE the link exists, the write-ahead order every P1b fixture uses — the add
    // itself would refuse a link that already resolves outside)
    let f4 = fixture();
    ok(&["manifest", "add", "t-1", "--class", "temp_dir", "--id", "lnk", "--path", &s(&f4.root.join("lnk")), "--dir", &s(&f4.md)]);
    std::os::unix::fs::symlink(&f4.outside, f4.root.join("lnk")).unwrap();
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "lnk", "--manifest-dir", &s(&f4.md)]);
    assert_eq!(code(&o), 0, "a listed link with a real parent is unlinked: {}{}", out(&o), err(&o));
    assert!(!f4.root.join("lnk").exists(), "the link is gone");
    assert!(f4.outside.join("keep.txt").exists(), "the link's target survives");
    // ... and the same shape reached through a parent swapped for a symlink is refused
    ok(&["manifest", "add", "t-1", "--class", "temp_dir", "--id", "lnk2", "--path", &s(&f4.root.join("tmp/lnk2")), "--dir", &s(&f4.md)]);
    fs::remove_dir_all(f4.root.join("tmp")).unwrap();
    std::os::unix::fs::symlink(&f4.outside, f4.root.join("tmp")).unwrap();
    fs::create_dir_all(f4.outside.join("lnk2")).unwrap();
    fs::write(f4.outside.join("lnk2/x.txt"), b"payload").unwrap();
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "lnk2", "--manifest-dir", &s(&f4.md)]);
    assert_eq!(code(&o), 2, "a listed link through a swapped parent is refused: {}{}", out(&o), err(&o));
    assert!(f4.outside.join("lnk2/x.txt").exists(), "nothing behind the swapped parent was touched");
}

/// Review r1, F3: the shelf `<quarantine-dir>/<ticket>` is examined with a no-follow stat BEFORE it is read
/// (reconcile) or made (make_batch). A symlink there would carry every batch, journal and item OUTSIDE the
/// quarantine dir; a regular file is refused the same way. Exit 2, nothing moved, nothing created outside.
#[test]
fn shelf_symlink_or_file_refused() {
    // (R5) the shelf is a symlink to a folder outside the quarantine dir
    let f = fixture();
    plant(&f, "stray-1", "one");
    let (elsewhere, _) = f.t.both("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(&f.q).unwrap();
    std::os::unix::fs::symlink(&elsewhere, f.q.join("t-1")).unwrap();
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 2, "a symlinked shelf is refused: {}{}", out(&o), err(&o));
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing was created outside the quarantine dir");
    assert!(!fs::symlink_metadata(&f.q.join("t-1/b-1-1-0")).is_ok(), "no batch was made through the link");
    // (must-fire, variant 4) the shelf is a regular file, not a directory
    let f2 = fixture();
    plant(&f2, "stray-1", "one");
    fs::write(f2.q.join("t-1"), b"not-a-dir").unwrap();
    let o = sweep(&f2, &[]);
    assert_eq!(code(&o), 2, "a shelf that is a regular file is refused: {}{}", out(&o), err(&o));
    assert!(f2.root.join("stray-1/one.txt").exists(), "nothing moved");
    assert_eq!(fs::read(f2.q.join("t-1")).unwrap(), b"not-a-dir", "the file was not replaced by a shelf");
}

/// Review r1, F6: an event log inside a root is refused exactly like a misplaced quarantine dir — a log written
/// inside a root is itself an unmanifested leftover the NEXT sweep would carry off.
#[test]
fn log_inside_root_refused() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    let o = sweep_replacing(&f, "--log", &s(&f.root.join("events.jsonl")));
    assert_eq!(code(&o), 2, "a log inside a root is refused: {}{}", out(&o), err(&o));
    assert!(err(&o).contains("outside every root"), "the refusal names the placement problem: {}", err(&o));
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    assert!(!f.root.join("events.jsonl").exists(), "no log was created inside the root");
    assert!(!f.q.join("t-1").exists(), "no shelf was made");
    // the parent folder need not exist yet either: a would-be log dir inside a root is refused the same way
    let o = sweep_replacing(&f, "--log", &s(&f.root.join("logdir-yet-to-be/events.jsonl")));
    assert_eq!(code(&o), 2, "a log dir to be created inside a root is refused: {}{}", out(&o), err(&o));
    assert!(!f.root.join("logdir-yet-to-be").exists(), "the log dir was not created inside the root");
}

/// Must-not-fire: a REAL, pre-existing shelf directory is ordinary — the sweep still moves a leftover into it
/// (exit 0, moved=1). The F3 refusal is about links and files, never about the shelf already existing.
#[test]
fn sweep_into_preexisting_shelf_dir() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    fs::create_dir_all(f.q.join("t-1")).unwrap();
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 0, "a pre-existing real shelf is fine: {}{}", out(&o), err(&o));
    assert!(out(&o).contains("moved=1"));
    assert_eq!(batches(&f).len(), 1, "one batch in the pre-existing shelf");
    assert!(batches(&f)[0].join("i-1/one.txt").exists(), "the payload is on the shelf");
}


// ---------------------------------------------------------------- review r2, fix round 2

/// Review r2, F1 (RACE-D): a folder inside a listed temp dir that is swapped for a symlink to an
/// OUTSIDE folder while the delete is running must never be followed — every outside file survives.
/// Part (a) is deterministic (a planted link is unlinked as the link); part (b) drives the swap
/// with a helper thread once one subdir starts emptying, three trials, 3000 files per subdir so
/// the emptying window is tens of milliseconds. (Measured before the fix: the hand-rolled walk
/// classified a child with a no-follow stat and opened it LATER by path with `read_dir`, which
/// follows a swapped-in link — 3 of 3 trials deleted all 50 outside files. `std::fs::remove_dir_all`
/// recurses by openat and never follows a link; a swapped-in link is unlinked as the link.)
#[test]
fn delete_listed_race_swapped_subdir_never_followed() {
    // (a) deterministic: a planted symlink subdir is unlinked as the link, its target intact
    let f = fixture();
    fs::create_dir_all(f.outside.join("ext-a")).unwrap();
    fs::write(f.outside.join("ext-a/keep.txt"), b"ext-a").unwrap();
    std::os::unix::fs::symlink(f.outside.join("ext-a"), f.root.join("tmp/planted-link")).unwrap();
    fs::write(f.root.join("tmp/x.txt"), b"x").unwrap();
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "tmp", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(!f.root.join("tmp").exists(), "the listed temp dir is gone");
    assert_eq!(fs::read(f.outside.join("ext-a/keep.txt")).unwrap(), b"ext-a", "the planted link's target is intact");

    // (b) the driven swap, three trials
    for trial in 1..=3 {
        let f = fixture();
        let (d1, d2) = (f.root.join("tmp/d1"), f.root.join("tmp/d2"));
        for d in [&d1, &d2] {
            fs::create_dir_all(d).unwrap();
            for i in 0..3000 {
                fs::write(d.join(format!("f{i:04}.txt")), b"x").unwrap();
            }
        }
        let ext = f.outside.join(format!("ext-race-{trial}"));
        fs::create_dir_all(&ext).unwrap();
        for i in 0..50 {
            fs::write(ext.join(format!("keep{i:02}.txt")), b"outside-payload").unwrap();
        }
        // the watcher: once ONE subdir starts emptying (count below full), swap the OTHER for a
        // symlink to the outside folder — exactly the RACE-D shape
        let w1 = d1.clone();
        let w2 = d2.clone();
        let wext = ext.clone();
        let h = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while std::time::Instant::now() < deadline {
                let c1 = fs::read_dir(&w1).map(|r| r.count()).unwrap_or(0);
                let c2 = fs::read_dir(&w2).map(|r| r.count()).unwrap_or(0);
                let victim = if c1 < 3000 && c2 == 3000 {
                    Some(w2.clone())
                } else if c2 < 3000 && c1 == 3000 {
                    Some(w1.clone())
                } else {
                    None
                };
                if let Some(v) = victim {
                    let old = v.with_file_name(format!("{}-old", v.file_name().unwrap().to_string_lossy()));
                    if fs::rename(&v, &old).is_ok() && std::os::unix::fs::symlink(&wext, &v).is_ok() {
                        return true;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            false
        });
        let o = run(&["quarantine", "delete-listed", "t-1", "--id", "tmp", "--manifest-dir", &s(&f.md)]);
        let swapped = h.join().unwrap();
        assert!(swapped, "trial {trial}: the watcher drove the swap (one subdir emptying, the other still full)");
        let c = code(&o);
        assert!(c == 0 || c == 3, "trial {trial}: exit 0 or 3, got {c}: {}", err(&o));
        for i in 0..50 {
            let p = ext.join(format!("keep{i:02}.txt"));
            assert!(p.exists(), "trial {trial}: outside file {i} was DELETED through the swapped link");
            assert_eq!(fs::read(&p).unwrap(), b"outside-payload", "trial {trial}: outside file {i} intact");
        }
        if c == 0 {
            assert!(!f.root.join("tmp").exists(), "trial {trial}: a successful delete removed the whole tree");
        }
    }
}

/// Review r2, F2 (RACE-S): a leftover's ancestor chain (the root and every ancestor below it,
/// with the dev/ino each had at walk time) is re-checked immediately before the item's rename; an
/// ancestor swapped for a symlink mid-run makes the sweep SKIP that item — nothing moved for it,
/// no journal line, no event — and the run ends exit 3 naming the item token. 50 ancestor groups
/// (each a live entry's parent holding one leftover file `c`) sit among 900 plain root leftovers
/// so the move batch is long; once the batch directory appears (it is minted only AFTER the
/// read-only walk), every group is swapped for a symlink to one outside folder holding a file
/// NAMED `c`. The outside file must never land on the shelf. Three trials.
#[test]
fn sweep_ancestor_swap_never_moves_from_outside() {
    for trial in 1..=3 {
        let f = fixture();
        let ext = f.outside.join(format!("ext-s-{trial}"));
        fs::create_dir_all(&ext).unwrap();
        fs::write(ext.join("c"), b"outside-c").unwrap();
        for i in 0..900 {
            let d = f.root.join(format!("stray-{i:03}"));
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("one.txt"), b"p").unwrap();
        }
        let mut groups: Vec<PathBuf> = Vec::new();
        for g in 0..50 {
            let a = f.root.join(format!("a{g:02}"));
            fs::create_dir_all(&a).unwrap();
            fs::write(a.join("c"), b"inside-c").unwrap();
            ok(&[
                "manifest", "add", "t-1", "--class", "temp_dir", "--id", &format!("live{g:02}"),
                "--path", &s(&a.join("b")), "--dir", &s(&f.md),
            ]);
            groups.push(a);
        }
        let wshelf = f.q.join("t-1");
        let wroot = f.root.clone();
        let wext = ext.clone();
        let h = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while std::time::Instant::now() < deadline {
                let batch_seen = fs::read_dir(&wshelf)
                    .map(|r| r.flatten().any(|e| e.file_name().to_string_lossy().starts_with("b-")))
                    .unwrap_or(false);
                if batch_seen {
                    // swap every group for a symlink to the outside folder
                    let mut n = 0;
                    for g in 0..50 {
                        let a = wroot.join(format!("a{g:02}"));
                        let old = wroot.join(format!("a{g:02}-old"));
                        if fs::rename(&a, &old).is_ok() && std::os::unix::fs::symlink(&wext, &a).is_ok() {
                            n += 1;
                        }
                    }
                    return Some(n);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            None
        });
        let o = sweep(&f, &[]);
        let swapped = h.join().unwrap();
        assert_eq!(swapped, Some(50), "trial {trial}: every group was swapped once the batch appeared");
        let c = code(&o);
        assert!(c == 0 || c == 3, "trial {trial}: exit 0 or 3, got {c}: {}", err(&o));
        if c == 3 {
            assert!(err(&o).contains("parent chain"), "trial {trial}: the refusal names the changed parent chain: {}", err(&o));
        }
        // the outside file never moved: still in ext, never on the shelf
        assert!(ext.join("c").exists(), "trial {trial}: ext/c is gone from outside the roots");
        assert_eq!(fs::read(ext.join("c")).unwrap(), b"outside-c", "trial {trial}: ext/c intact");
        let mut outside_on_shelf = 0;
        for b in batches(&f) {
            if let Ok(rd) = fs::read_dir(&b) {
                for ent in rd.flatten() {
                    let p = ent.path();
                    if fs::symlink_metadata(&p).map(|m| !m.is_dir()).unwrap_or(false) {
                        if fs::read(&p).map(|b| b == b"outside-c").unwrap_or(false) {
                            outside_on_shelf += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(outside_on_shelf, 0, "trial {trial}: a file from OUTSIDE the roots landed on the shelf");
        // no journal line and no event for a skipped item: journal lines == events, always
        let mut journal_lines = 0usize;
        for b in batches(&f) {
            if let Ok(t) = fs::read_to_string(b.join("journal.jsonl")) {
                journal_lines += t.lines().count();
            }
        }
        assert_eq!(journal_lines, quarantined_lines(&f).len(), "trial {trial}: one journal line per event, none for a skipped item");
    }
}

/// Review r2, F3 (A8): a listed temp dir DEEPER than the filesystem's path limit (PATH_MAX) is
/// still deleted — the fence walks the components one at a time from the validated root and the
/// real-directory removal is `std::fs::remove_dir_all`, which recurses by openat with no path
/// length limit. A symlink to an outside folder planted at the deepest level is unlinked as the
/// link and the outside folder is untouched. Before the fix: ENAMETOOLONG, exit 3, nothing
/// deleted, wedged on every re-run. The re-run (the id listed again) is the clean no-op.
#[test]
fn delete_listed_deeper_than_path_max() {
    let f = fixture();
    let canon_root = f.root.canonicalize().unwrap();
    let seg = "d".repeat(240);
    // 15 levels x 241 bytes ~ 3.7 KB: past PATH_MAX on macOS (1024), where the r2 A8 reproduction
    // was measured, and still under the manifest reader's 4096-byte line cap so the entry can be
    // LISTED at all (a path past Linux's PATH_MAX of 4096 cannot fit a manifest line: the reader
    // fails closed there, and no delete is reachable through the manifest for it).
    let depth = 15;
    let mut deep = canon_root.clone();
    for _ in 0..depth {
        deep.push(&seg);
    }
    assert!(deep.as_os_str().len() > 1100, "the fixture path must exceed macOS PATH_MAX");
    assert!(deep.as_os_str().len() < 3900, "the fixture path must fit a manifest line (4096-byte cap)");
    // build the deep tree with relative steps (an absolute mkdir would exceed the path limit);
    // plant an outside symlink at the deepest level (held-out variant: the outside folder survives)
    fs::create_dir_all(f.outside.join("ext-deep")).unwrap();
    fs::write(f.outside.join("ext-deep/keep.txt"), b"ext-deep").unwrap();
    let script = format!(
        "d={}; i=0; while [ $i -lt {d} ]; do mkdir \"$d\" || exit 1; cd \"$d\" || exit 1; i=$((i+1)); done; echo deep > f.txt; ln -s {} ext-link",
        seg,
        f.outside.join("ext-deep").to_str().unwrap(),
        d = depth
    );
    let o = Command::new("/bin/sh")
        .arg("-c")
        .arg(&script)
        .current_dir(&f.root)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success(), "the deep build failed: {}", String::from_utf8_lossy(&o.stderr));
    append_manifest_line(
        &f,
        &format!(
            "{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"deep\",\"class\":\"temp_dir\",\"path\":\"{}\",\"ts_ms\":1731000000125}}",
            s(&deep)
        ),
    );
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "deep", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 0, "a listed tree deeper than PATH_MAX deletes cleanly: {}", err(&o));
    // gone: walk down depth-1 levels, the deepest name must be absent
    let check = format!("d={}; i=0; while [ $i -lt {} ]; do cd \"$d\" || exit 1; i=$((i+1)); done; [ ! -e \"$d\" ]", seg, depth - 1);
    let o = Command::new("/bin/sh").arg("-c").arg(&check).current_dir(&f.root).stdin(Stdio::null()).output().unwrap();
    assert!(o.status.success(), "the deepest level is gone");
    assert_eq!(fs::read(f.outside.join("ext-deep/keep.txt")).unwrap(), b"ext-deep", "the outside folder behind the deepest link is untouched");
    let listed = out(&run(&["manifest", "list", "t-1", "--dir", &s(&f.md)]));
    assert!(!listed.lines().any(|l| l.starts_with("deep\t")), "the remove line was written: {listed}");
    // the clean no-op re-run: the id listed again, the path absent, the remove written, exit 0
    append_manifest_line(
        &f,
        &format!(
            "{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"deep\",\"class\":\"temp_dir\",\"path\":\"{}\",\"ts_ms\":1731000000126}}",
            s(&deep)
        ),
    );
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "deep", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 0, "the re-run on the absent deep path is a clean no-op: {}", err(&o));
    let listed = out(&run(&["manifest", "list", "t-1", "--dir", &s(&f.md)]));
    assert!(!listed.lines().any(|l| l.starts_with("deep\t")), "the remove line was written again: {listed}");
}

/// Review r2, F5: delete-listed's ROOT RE-VALIDATION pinned on its own — a root that was real
/// when the manifest was written but is swapped for a symlink to an OUTSIDE folder afterwards is
/// refused by the re-validation arm itself (the stderr line names the symlink rule), and nothing
/// outside the roots is touched. (With the re-validation removed and the raw stored root used as
/// the walk anchor, the component walk would step THROUGH the swapped root and delete the outside
/// tree — this is the test that goes RED then.)
#[test]
fn delete_root_revalidation_pinned() {
    let f = fixture();
    fs::create_dir_all(f.outside.join("tmp")).unwrap();
    fs::write(f.outside.join("tmp/p.txt"), b"outside-root").unwrap();
    fs::remove_dir_all(&f.root).unwrap();
    std::os::unix::fs::symlink(&f.outside, &f.root).unwrap();
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "tmp", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 2, "a root swapped for a symlink is refused: {}{}", out(&o), err(&o));
    assert!(err(&o).contains("symlink"), "the refusal names the root-is-symlink rule itself: {}", err(&o));
    assert!(f.outside.join("tmp/p.txt").exists(), "nothing behind the swapped root was deleted");
    assert!(fs::symlink_metadata(&f.root).unwrap().file_type().is_symlink(), "the swapped root link itself is untouched");
}

/// Review r2, F6: a symlink loop in a listed path's PARENT chain is a parent that IS a link: the
/// listed path does not resolve inside a root — exit 2, nothing deleted, the loop link itself
/// untouched. (Before the fix this exited 3 as "the listed path cannot be examined"; the doc now
/// names a link parent — a loop included — under the exit-2 refusals.)
#[test]
fn delete_listed_parent_loop_refused_exit_2() {
    let f = fixture();
    std::os::unix::fs::symlink("loop", f.root.join("loop")).unwrap(); // a link that resolves to itself
    let canon_root = f.root.canonicalize().unwrap();
    append_manifest_line(
        &f,
        &format!(
            "{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"lp\",\"class\":\"temp_dir\",\"path\":\"{}/loop/x\",\"ts_ms\":1731000000127}}",
            s(&canon_root)
        ),
    );
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "lp", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 2, "a symlink loop in the parent chain is a refusal: {}{}", out(&o), err(&o));
    assert!(err(&o).contains("does not resolve inside a root"), "the refusal names the rule: {}", err(&o));
    assert!(fs::symlink_metadata(f.root.join("loop")).is_ok(), "the loop link itself is untouched");
    assert!(f.root.join("tmp/scratch.txt").exists(), "nothing inside the roots was deleted either");
}

/// Must-not-fire (r2 held-out variant 3): a sweep whose leftovers' ancestors are all REAL and
/// unchanged moves every one of them — 900 plain root leftovers plus a leftover nested under an
/// ancestor of a live entry — exit 0, moved=901; the ancestor re-check never refuses a stable
/// tree, and the ancestor of the live entry stays in place.
#[test]
fn sweep_moves_all_when_ancestors_stable() {
    let f = fixture();
    for i in 0..900 {
        let d = f.root.join(format!("stray-{i:03}"));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("one.txt"), b"p").unwrap();
    }
    fs::create_dir_all(f.root.join("a/c")).unwrap();
    fs::write(f.root.join("a/c/one.txt"), b"nested").unwrap();
    ok(&["manifest", "add", "t-1", "--class", "temp_dir", "--id", "live-a", "--path", &s(&f.root.join("a/b")), "--dir", &s(&f.md)]);
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 0, "a stable tree sweeps clean: {}", err(&o));
    assert!(out(&o).contains("moved=901"), "every leftover moved: {}", out(&o));
    assert!(f.root.join("a").exists(), "the ancestor of the live entry stays");
    assert!(!f.root.join("a/c").exists(), "the nested leftover moved");
    for i in 0..900 {
        assert!(!f.root.join(format!("stray-{i:03}")).exists(), "stray-{i:03} moved");
    }
}

// ---------------------------------------------------------------- doc contract (#175)

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The `delete-listed` doc section must state the caller rule (#175): `delete_listed` changes the process
/// working directory while it walks (saved and restored before it returns), so it is not safe to call while
/// another thread is using the working directory or relative paths — P1B-14 teardown must call it from a
/// single thread only. Word-presence contract in the `manifest.rs` precedent style: if the wording is ever
/// dropped this fails; if the section heading is renamed the slice fails loud too (a false RED, never a
/// false GREEN).
#[test]
fn the_doc_states_the_delete_working_directory_caller_rule() {
    let doc = std::fs::read_to_string(repo().join("docs/quarantine.md")).expect("the doc is committed and utf-8");
    let start = doc.find("## `harness quarantine delete-listed`").expect("the delete-listed section heading exists");
    let end = doc[start..].find("\n## ").map(|i| start + i).expect("a section follows the delete-listed section");
    let section = &doc[start..end];
    let lower = section.to_lowercase();
    assert!(lower.contains("working directory"), "the delete section states the working-directory rule");
    assert!(lower.contains("thread"), "the delete section states the not-thread-safe rule");
    assert!(section.contains("P1B-14"), "the delete section ties the rule to P1B-14 teardown");
}
