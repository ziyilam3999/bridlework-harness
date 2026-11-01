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
    // (a) the ticket root itself is a symlink
    let f = fixture();
    plant(&f, "stray-1", "one");
    fs::remove_dir_all(&f.root).unwrap();
    std::os::unix::fs::symlink(&f.outside, &f.root).unwrap();
    let o = sweep(&f, &[]);
    assert_eq!(code(&o), 2, "a root that IS a symlink is refused: {}{}", out(&o), err(&o));
    assert!(!f.q.join("t-1").exists(), "no shelf was made, nothing moved");
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

/// AC 8: a quarantine dir inside a root, equal to the manifest dir, or relative exits 2 and nothing moves.
#[test]
fn misplaced_quarantine_dir_refused() {
    let f = fixture();
    plant(&f, "stray-1", "one");
    // (a) inside a root: the ticket root itself
    let inner = f.root.join("q-inner");
    fs::create_dir_all(&inner).unwrap();
    let o = sweep(&f, &[("--quarantine-dir", &s(&inner))]);
    assert_eq!(code(&o), 2, "a quarantine dir inside a root is refused: {}{}", out(&o), err(&o));
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    assert!(!f.q.join("t-1").exists(), "no shelf");
    // (b) equal to the manifest dir
    let o = sweep(&f, &[("--quarantine-dir", &s(&f.md))]);
    assert_eq!(code(&o), 2, "a quarantine dir equal to the manifest dir is refused: {}{}", out(&o), err(&o));
    assert!(f.root.join("stray-1/one.txt").exists(), "nothing moved");
    // (c) relative
    let o = sweep(&f, &[("--quarantine-dir", "just-a-name")]);
    assert_eq!(code(&o), 2, "a relative quarantine dir is refused: {}{}", out(&o), err(&o));
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
    let o = run(&["quarantine", "delete-listed", "t-1", "--id", "tmp", "--manifest-dir", &s(&f.md)]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "delete=done ticket=t-1 id=tmp\n");
    assert!(!f.root.join("tmp").exists(), "the listed temp dir is gone");
    assert!(f.outside.join("keep.txt").exists(), "the link's TARGET survives the unlink");
    assert_eq!(fs::read(f.outside.join("keep.txt")).unwrap(), b"outside-payload");
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
    outputs.push(sweep(&f, &[("--quarantine-dir", "relative-q")]));
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
