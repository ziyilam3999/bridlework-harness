//! Process-level tests for `harness quarantine drop-build` (ticket build-output-drop): the third quarantine
//! sub-verb, which removes the cache-tagged build output (`target` folders holding CACHEDIR.TAG) of a CLOSED
//! ticket under roots that sit beside the manifests folder in one state folder, or refuses. Every check runs
//! the real binary as a child process against a scratch state folder under the system temp folder; the one
//! in-process call pins the single-thread guard. Fixtures are synthetic (plain folders and files; the `.git`
//! entries and cache tags are made by the test). No folder path may appear on either stream.
#![cfg(unix)]
use harness::quarantine::{self, DropArgs};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);
/// The cache tag signature line (the cache directory tag standard; the tag cargo writes), newline included.
const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";

/// One scratch layout: the RAW temp path and its canonical spelling are both kept, because on this platform
/// they differ and neither may appear on stdout or stderr.
struct Tmp {
    raw: PathBuf,
    canon: PathBuf,
}

impl Tmp {
    fn new() -> Tmp {
        let raw = std::env::temp_dir().join(format!("qbld-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&raw);
        fs::create_dir_all(&raw).unwrap();
        let canon = raw.canonicalize().unwrap();
        Tmp { raw, canon }
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.raw);
        let _ = fs::remove_dir_all(&self.canon);
    }
}

/// The state folder of one ticket `t-1`: `manifests` (the closed manifest), `caps`, and the two roots `gate`
/// and `ignored`, all siblings inside one state folder.
struct Fix {
    t: Tmp,
    state: PathBuf,
    md: PathBuf,
    caps: PathBuf,
    gate: PathBuf,
    ignored: PathBuf,
}

/// The dirs without the manifest (tests that write their own manifest lines start here).
fn bare() -> Fix {
    let t = Tmp::new();
    let state = t.raw.join("state");
    let md = state.join("manifests");
    let caps = state.join("caps");
    let gate = state.join("gate");
    let ignored = state.join("ignored");
    for d in [&md, &caps, &gate, &ignored] {
        fs::create_dir_all(d).unwrap();
    }
    Fix { t, state, md, caps, gate, ignored }
}

/// Write the ticket's manifest as exactly these raw jsonl lines.
fn write_manifest(f: &Fix, lines: &[String]) {
    let mut text = lines.join("\n");
    text.push('\n');
    fs::write(f.md.join("t-1.jsonl"), text).unwrap();
}

/// The closed manifest of the worked case: one root, a `worktree` add and its remove, nothing live.
fn closed_manifest(f: &Fix) {
    let gate = f.gate.canonicalize().unwrap();
    write_manifest(
        f,
        &[
            format!("{{\"v\":1,\"op\":\"root\",\"ticket\":\"t-1\",\"path\":\"{}\",\"ts_ms\":1}}", s(&gate)),
            format!("{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"worktree\",\"class\":\"worktree\",\"path\":\"{}/t-1/tree\",\"ts_ms\":2}}", s(&gate)),
            "{\"v\":1,\"op\":\"remove\",\"ticket\":\"t-1\",\"id\":\"worktree\",\"ts_ms\":3}".to_string(),
        ],
    );
}

/// The state folder plus the closed manifest.
fn fixture() -> Fix {
    let f = bare();
    closed_manifest(&f);
    f
}

fn run(args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
    for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL", "HARNESS_CONFIG"] {
        c.env_remove(k);
    }
    c.output().unwrap()
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

/// One `quarantine drop-build` command line.
fn drop_cmd(ticket: &str, roots: &[&Path], md: &Path, caps: &Path, extra: &[&str]) -> Output {
    let mut v: Vec<String> = vec!["quarantine".into(), "drop-build".into(), ticket.into()];
    for r in roots {
        v.push("--root".into());
        v.push(s(r));
    }
    v.push("--manifest-dir".into());
    v.push(s(md));
    v.push("--caps-dir".into());
    v.push(s(caps));
    for e in extra {
        v.push((*e).to_string());
    }
    let refs: Vec<&str> = v.iter().map(|x| x.as_str()).collect();
    run(&refs)
}

/// The standard run of the fixture: ticket `t-1`, both roots, the fixture's manifest and caps dirs.
fn drop_both(f: &Fix) -> Output {
    drop_cmd("t-1", &[&f.gate, &f.ignored], &f.md, &f.caps, &[])
}

/// A cargo-shaped build output tree under `folder`: `core/rust/target` holding the cache tag, with a sibling
/// source file. Returns the `target` folder.
fn build_output(folder: &Path) -> PathBuf {
    let t = folder.join("core/rust/target");
    fs::create_dir_all(&t).unwrap();
    fs::write(t.join("CACHEDIR.TAG"), TAG).unwrap();
    fs::create_dir_all(folder.join("core/rust")).unwrap();
    fs::write(folder.join("core/rust/lib.rs"), b"source-bytes").unwrap();
    t
}

/// One tagged `target` folder at `dir` (the caller passes the full path ending in `target`).
fn tagged(dir: &Path) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("CACHEDIR.TAG"), TAG).unwrap();
    dir.to_path_buf()
}

/// Neither stream holds the temp folder path, raw or canonical.
fn no_leak(f: &Fix, o: &Output) {
    for stream in [out(o), err(o)] {
        for secret in [f.t.raw.to_string_lossy().to_string(), f.t.canon.to_string_lossy().to_string()] {
            assert!(!stream.contains(&secret), "a folder path leaked: {stream}");
        }
    }
}

/// A full recursive listing: sorted relative paths, and file bytes for files (None for folders).
fn snap(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    fn walk(dir: &Path, base: &Path, v: &mut Vec<(String, Option<Vec<u8>>)>) {
        let mut rd: Vec<PathBuf> = fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        rd.sort();
        for p in rd {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            if p.is_dir() {
                v.push((rel, None));
                walk(&p, base, v);
            } else {
                v.push((rel, Some(fs::read(&p).unwrap())));
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v
}

/// A refusal with the layout byte-unchanged, stdout empty, exit 2, and no leaked path.
fn refused_2_unchanged(f: &Fix, o: &Output) {
    assert_eq!(code(o), 2, "exit 2: {}{}", out(o), err(o));
    assert_eq!(out(o), "", "stdout stays empty on a refusal");
    no_leak(f, o);
}

/// True when folder modes are enforced for this user (false when the tests run as root).
fn modes_enforced() -> bool {
    let t = Tmp::new();
    let d = t.raw.join("m");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("f"), b"x").unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o000)).unwrap();
    let enforced = fs::read_dir(&d).is_err();
    let _ = fs::set_permissions(&d, fs::Permissions::from_mode(0o755));
    enforced
}

/// Make a FIFO with `mkfifo(1)` (the crate itself stays std-only).
fn mkfifo(p: &Path) {
    let o = Command::new("mkfifo").arg(p).output().unwrap();
    assert!(o.status.success(), "mkfifo {:?} failed: {}", p, String::from_utf8_lossy(&o.stderr));
}

// ---------------------------------------------------------------- the worked case

/// The worked case: one state folder, two roots, three ticket folders under the three name forms, each holding
/// `core/rust/target` with the cache tag and a sibling source file. removed=3 kept=0, the three targets gone,
/// the source files byte-unchanged, stderr empty.
#[test]
fn the_worked_case() {
    let f = fixture();
    let t1 = build_output(&f.gate.join("t-1"));
    let t2 = build_output(&f.gate.join("t-1.old.1.2"));
    let t3 = build_output(&f.ignored.join("t-1.3.4"));
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=3 kept=0\n");
    assert_eq!(err(&o), "", "stderr stays empty on success");
    for t in [&t1, &t2, &t3] {
        assert!(!t.exists(), "the tagged target {:?} is gone", t.file_name());
    }
    for lib in [f.gate.join("t-1/core/rust/lib.rs"), f.gate.join("t-1.old.1.2/core/rust/lib.rs"), f.ignored.join("t-1.3.4/core/rust/lib.rs")] {
        assert_eq!(fs::read(&lib).unwrap(), b"source-bytes", "the source file is byte-unchanged");
    }
    no_leak(&f, &o);
}

/// A second run after a full one removes nothing: removed=0.
#[test]
fn a_second_run_removes_nothing() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let o1 = drop_both(&f);
    assert_eq!(code(&o1), 0, "stderr: {}", err(&o1));
    let o2 = drop_both(&f);
    assert_eq!(code(&o2), 0, "stderr: {}", err(&o2));
    assert_eq!(out(&o2), "drop-build=done ticket=t-1 removed=0 kept=0\n");
}

/// --dry-run: the same walk, would_remove=3, and the tree byte-unchanged.
#[test]
fn dry_run_removes_nothing() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    build_output(&f.gate.join("t-1.old.1.2"));
    build_output(&f.ignored.join("t-1.3.4"));
    let before = snap(&f.gate);
    let before_ign = snap(&f.ignored);
    let o = drop_cmd("t-1", &[&f.gate, &f.ignored], &f.md, &f.caps, &["--dry-run"]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=dry-run ticket=t-1 would_remove=3 kept=0\n");
    assert_eq!(snap(&f.gate), before, "the roots are byte-unchanged by a dry run");
    assert_eq!(snap(&f.ignored), before_ign, "the roots are byte-unchanged by a dry run");
    no_leak(&f, &o);
}

// ---------------------------------------------------------------- usage

/// An unknown flag, a missing value, a repeated single flag, a 9th --root, no ticket and a second ticket are
/// each exit 2 with the USAGE line on stderr and nothing removed.
#[test]
fn usage_errors_exit_2_with_the_usage() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let before = snap(&f.state);
    let mut cases: Vec<Vec<String>> = vec![
        // an unknown flag
        vec!["quarantine".into(), "drop-build".into(), "t-1".into(), "--bogus".into()],
        // a flag missing its value
        vec!["quarantine".into(), "drop-build".into(), "t-1".into(), "--root".into()],
        // a single-valued flag given twice
        vec!["quarantine".into(), "drop-build".into(), "t-1".into(), "--root".into(), s(&f.gate), "--manifest-dir".into(), s(&f.md), "--manifest-dir".into(), s(&f.md), "--caps-dir".into(), s(&f.caps)],
        // no ticket
        vec!["quarantine".into(), "drop-build".into(), "--root".into(), s(&f.gate), "--manifest-dir".into(), s(&f.md), "--caps-dir".into(), s(&f.caps)],
        // a second ticket
        vec!["quarantine".into(), "drop-build".into(), "t-1".into(), "t-2".into(), "--root".into(), s(&f.gate), "--manifest-dir".into(), s(&f.md), "--caps-dir".into(), s(&f.caps)],
    ];
    // a 9th --root
    let mut nine: Vec<String> = vec!["quarantine".into(), "drop-build".into(), "t-1".into()];
    for _ in 0..9 {
        nine.push("--root".into());
        nine.push(s(&f.gate));
    }
    nine.push("--manifest-dir".into());
    nine.push(s(&f.md));
    nine.push("--caps-dir".into());
    nine.push(s(&f.caps));
    cases.push(nine);
    for (i, c) in cases.iter().enumerate() {
        let refs: Vec<&str> = c.iter().map(|x| x.as_str()).collect();
        let o = run(&refs);
        assert_eq!(code(&o), 2, "case {i} exits 2: {}{}", out(&o), err(&o));
        assert!(err(&o).starts_with("harness: error: "), "case {i} names the problem");
        assert!(err(&o).contains("usage: harness quarantine sweep"), "case {i} prints the USAGE line: {}", err(&o));
        assert!(err(&o).contains("drop-build <ticket> --root <abs>"), "the USAGE holds the drop-build synopsis: {}", err(&o));
        assert_eq!(out(&o), "", "case {i}: stdout stays empty");
    }
    assert_eq!(snap(&f.state), before, "nothing was removed by any usage refusal");
}

// ---------------------------------------------------------------- ticket id and roots

/// A ticket id with a slash is refused at step 2, exit 2, nothing removed.
#[test]
fn a_ticket_id_with_a_slash_exits_2() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let before = snap(&f.state);
    let o = drop_cmd("t/1", &[&f.gate], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert_eq!(snap(&f.state), before);
}

/// An uppercase ticket id is refused, exit 2.
#[test]
fn an_uppercase_ticket_id_exits_2() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let before = snap(&f.state);
    let o = drop_cmd("T-1", &[&f.gate], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert_eq!(snap(&f.state), before);
}

/// A ticket id of `..` alone is refused, exit 2.
#[test]
fn a_ticket_id_of_dot_dot_exits_2() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let before = snap(&f.state);
    let o = drop_cmd("..", &[&f.gate], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert_eq!(snap(&f.state), before);
}

/// A --root that is a symlink to a good root is refused (no-follow stat); the target is byte-unchanged.
#[test]
fn a_root_that_is_a_link_exits_2() {
    let f = fixture();
    let t1 = build_output(&f.gate.join("t-1"));
    let link = f.state.join("gate-link");
    std::os::unix::fs::symlink(&f.gate, &link).unwrap();
    let o = drop_cmd("t-1", &[&link], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(err(&o).contains("link"), "the refusal names the link rule: {}", err(&o));
    assert!(t1.join("CACHEDIR.TAG").exists(), "the target behind the link is byte-unchanged");
}

/// A relative --root is refused, exit 2.
#[test]
fn a_relative_root_exits_2() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let o = drop_cmd("t-1", &[Path::new("relative-gate")], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
}

/// `--root /` is refused (the filesystem root has no parent, so the beside-the-manifests rule refuses it).
#[test]
fn the_root_slash_exits_2() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let o = drop_cmd("t-1", &[Path::new("/")], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(err(&o).contains("--root 1"), "the refusal names the root position: {}", err(&o));
}

/// A root in another folder (not beside the manifests folder) is refused, the line names --root 1, and the
/// tagged target under it is byte-unchanged.
#[test]
fn a_root_outside_the_state_folder_exits_2() {
    let f = fixture();
    let other = Tmp::new();
    let gate2 = other.raw.join("gate");
    let t1 = build_output(&gate2.join("t-1"));
    let o = drop_cmd("t-1", &[&gate2], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(err(&o).contains("--root 1"), "the refusal names the root position: {}", err(&o));
    assert!(t1.join("CACHEDIR.TAG").exists(), "the target under the refused root is byte-unchanged");
}

/// A root that lies inside another root is refused, exit 2.
#[test]
fn a_root_inside_another_root_exits_2() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let sub = f.gate.join("sub");
    fs::create_dir_all(&sub).unwrap();
    let o = drop_cmd("t-1", &[&f.gate, &sub], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(err(&o).contains("--root 2"), "the refusal names the root position: {}", err(&o));
}

/// A --caps-dir that lies inside a root is refused, exit 2.
#[test]
fn a_caps_dir_inside_a_root_exits_2() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let inner = f.gate.join("caps-inner");
    fs::create_dir_all(&inner).unwrap();
    let o = drop_cmd("t-1", &[&f.gate], &f.md, &inner, &[]);
    refused_2_unchanged(&f, &o);
    assert!(f.gate.join("t-1/core/rust/target").exists(), "nothing was removed");
}

/// A --manifest-dir that lies inside a root is refused, exit 2.
#[test]
fn a_manifest_dir_inside_a_root_exits_2() {
    let f = fixture();
    build_output(&f.gate.join("t-1"));
    let inner = f.gate.join("md-inner");
    fs::create_dir_all(&inner).unwrap();
    let o = drop_cmd("t-1", &[&f.gate], &inner, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(f.gate.join("t-1/core/rust/target").exists(), "nothing was removed");
}

// ---------------------------------------------------------------- the closed proof and the lock

/// A missing manifest is refused, exit 2, nothing removed.
#[test]
fn a_missing_manifest_exits_2() {
    let f = bare();
    let t1 = build_output(&f.gate.join("t-1"));
    let o = drop_cmd("t-1", &[&f.gate], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(t1.join("CACHEDIR.TAG").exists(), "nothing was removed");
}

/// A torn manifest is refused, exit 2, nothing removed.
#[test]
fn a_torn_manifest_exits_2() {
    let f = bare();
    let t1 = build_output(&f.gate.join("t-1"));
    let gate = f.gate.canonicalize().unwrap();
    fs::write(
        f.md.join("t-1.jsonl"),
        format!(
            "{{\"v\":1,\"op\":\"root\",\"ticket\":\"t-1\",\"path\":\"{}\",\"ts_ms\":1}}\n{{\"v\":1",
            s(&gate)
        ),
    )
    .unwrap();
    let o = drop_cmd("t-1", &[&f.gate], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(err(&o).contains("damaged"), "the refusal names the damage: {}", err(&o));
    assert!(t1.join("CACHEDIR.TAG").exists(), "nothing was removed");
}

/// A manifest with a live entry is refused (the worktree entry has its remove line, the tmp entry is still
/// live, so only the live-entry check refuses it), exit 2.
#[test]
fn a_live_entry_exits_2() {
    let f = bare();
    let t1 = build_output(&f.gate.join("t-1"));
    let gate = f.gate.canonicalize().unwrap();
    write_manifest(
        &f,
        &[
            format!("{{\"v\":1,\"op\":\"root\",\"ticket\":\"t-1\",\"path\":\"{}\",\"ts_ms\":1}}", s(&gate)),
            format!("{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"worktree\",\"class\":\"worktree\",\"path\":\"{}/t-1/tree\",\"ts_ms\":2}}", s(&gate)),
            "{\"v\":1,\"op\":\"remove\",\"ticket\":\"t-1\",\"id\":\"worktree\",\"ts_ms\":3}".to_string(),
            format!("{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"tmp\",\"class\":\"temp_dir\",\"path\":\"{}/t-1/tmp\",\"ts_ms\":4}}", s(&gate)),
        ],
    );
    let o = drop_cmd("t-1", &[&f.gate], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(err(&o).contains("closed"), "the refusal says the ticket is not closed: {}", err(&o));
    assert!(t1.join("CACHEDIR.TAG").exists(), "nothing was removed");
}

/// A manifest whose only entry (`tmp`) was added and removed, with no worktree entry at all, is refused by
/// the worktree-remove check alone, exit 2.
#[test]
fn a_manifest_without_a_worktree_remove_exits_2() {
    let f = bare();
    let t1 = build_output(&f.gate.join("t-1"));
    let gate = f.gate.canonicalize().unwrap();
    write_manifest(
        &f,
        &[
            format!("{{\"v\":1,\"op\":\"root\",\"ticket\":\"t-1\",\"path\":\"{}\",\"ts_ms\":1}}", s(&gate)),
            format!("{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"tmp\",\"class\":\"temp_dir\",\"path\":\"{}/t-1/tmp\",\"ts_ms\":2}}", s(&gate)),
            "{\"v\":1,\"op\":\"remove\",\"ticket\":\"t-1\",\"id\":\"tmp\",\"ts_ms\":3}".to_string(),
        ],
    );
    let o = drop_cmd("t-1", &[&f.gate], &f.md, &f.caps, &[]);
    refused_2_unchanged(&f, &o);
    assert!(err(&o).contains("worktree"), "the refusal names the worktree remove line: {}", err(&o));
    assert!(t1.join("CACHEDIR.TAG").exists(), "nothing was removed");
}

/// A ticket lock held by another process is Busy: exit 3, one stderr line, nothing removed. The lock file is
/// held with File::try_lock while the child runs.
#[test]
fn a_held_ticket_lock_exits_3() {
    let f = fixture();
    let t1 = build_output(&f.gate.join("t-1"));
    let holder = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(f.caps.join(".t-1.lock"))
        .unwrap();
    holder.try_lock().unwrap();
    let o = drop_cmd("t-1", &[&f.gate], &f.md, &f.caps, &[]);
    drop(holder);
    assert_eq!(code(&o), 3, "a held lock is exit 3: {}{}", out(&o), err(&o));
    assert_eq!(out(&o), "", "stdout stays empty");
    assert!(err(&o).contains("in progress"), "the refusal names the in-progress run: {}", err(&o));
    assert!(t1.join("CACHEDIR.TAG").exists(), "nothing was removed");
    no_leak(&f, &o);
}

/// The single-thread guard, pinned IN PROCESS: with a second thread parked alive, `drop_build` itself refuses
/// with Stop(3), the line holds the word thread and no slash, nothing is removed, and the working directory
/// is unchanged.
#[test]
fn drop_build_refuses_when_another_thread_is_alive() {
    let f = fixture();
    let t1 = build_output(&f.gate.join("t-1"));
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let parked = std::thread::spawn(move || {
        let _ = rx.recv();
    });
    let args = DropArgs {
        ticket: "t-1".to_string(),
        roots: vec![f.gate.clone()],
        manifest_dir: f.md.clone(),
        caps_dir: f.caps.clone(),
        dry_run: false,
    };
    let before = std::env::current_dir().unwrap();
    match quarantine::drop_build(&args) {
        Err(quarantine::Stop(3, line)) => {
            assert!(line.contains("thread"), "the refusal names threads: {line}");
            assert!(!line.contains('/'), "the refusal line is folder-free: {line}");
        }
        other => panic!("expected a Stop(3) refusal, got {:?}", other.err().map(|st| st.1)),
    }
    assert!(t1.join("CACHEDIR.TAG").exists(), "the build output was NOT removed");
    assert!(f.gate.join("t-1/core/rust/lib.rs").exists(), "nothing else was touched either");
    assert_eq!(std::env::current_dir().unwrap(), before, "the process working directory is unchanged");
    drop(tx);
    let _ = parked.join();
}

// ---------------------------------------------------------------- what belongs to the ticket

/// A `target` without the cache tag is kept: kept=1, removed=0, its contents intact.
#[test]
fn an_untagged_target_is_kept() {
    let f = fixture();
    let t1 = f.gate.join("t-1/core/rust/target");
    fs::create_dir_all(&t1).unwrap();
    fs::write(t1.join("build.bin"), b"bin").unwrap();
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=1\n");
    assert_eq!(fs::read(t1.join("build.bin")).unwrap(), b"bin", "the untagged target is intact");
}

/// A `target` whose tag is wrong is kept: a wrong first line, an empty tag and a tag that is a folder —
/// kept=1 each.
#[test]
fn a_target_with_a_wrong_tag_line_is_kept() {
    let f = fixture();
    let wrong = f.gate.join("t-1.1.1/target");
    fs::create_dir_all(&wrong).unwrap();
    fs::write(wrong.join("CACHEDIR.TAG"), "Signature: deadbeef\n").unwrap();
    let empty = f.gate.join("t-1.2.2/target");
    fs::create_dir_all(&empty).unwrap();
    fs::write(empty.join("CACHEDIR.TAG"), "").unwrap();
    let folder = f.gate.join("t-1.3.3/target");
    fs::create_dir_all(folder.join("CACHEDIR.TAG")).unwrap();
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=3\n");
    assert!(wrong.join("CACHEDIR.TAG").exists() && empty.join("CACHEDIR.TAG").exists());
    assert!(folder.join("CACHEDIR.TAG").is_dir(), "the folder tag is intact");
}

/// A CACHEDIR.TAG that is a link to a good tag file is kept (the open refuses the link); the tag file
/// outside is byte-unchanged.
#[test]
fn a_linked_tag_is_kept() {
    let f = fixture();
    let good = f.t.raw.join("good-tag");
    fs::write(&good, TAG).unwrap();
    let t1 = tagged(&f.gate.join("t-1/core/rust/target"));
    fs::remove_file(t1.join("CACHEDIR.TAG")).unwrap();
    std::os::unix::fs::symlink(&good, t1.join("CACHEDIR.TAG")).unwrap();
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=1\n");
    assert!(t1.exists(), "the target is kept");
    assert_eq!(fs::read(&good).unwrap(), TAG.as_bytes(), "the tag file behind the link is byte-unchanged");
}

/// A CACHEDIR.TAG that is a FIFO neither hangs the child nor removes the target: the child returns within
/// 10 seconds (killed and failed then) with kept=1.
#[test]
fn a_fifo_tag_is_kept_without_a_hang() {
    let f = fixture();
    let t1 = tagged(&f.gate.join("t-1/core/rust/target"));
    fs::remove_file(t1.join("CACHEDIR.TAG")).unwrap();
    mkfifo(&t1.join("CACHEDIR.TAG"));
    let mut c = Command::new(BIN);
    c.args(["quarantine", "drop-build", "t-1", "--root", &s(&f.gate), "--manifest-dir", &s(&f.md), "--caps-dir", &s(&f.caps)])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL", "HARNESS_CONFIG"] {
        c.env_remove(k);
    }
    let mut child = c.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut killed = false;
    let status = loop {
        match child.try_wait().unwrap() {
            Some(st) => break st,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                killed = true;
                break child.wait().unwrap();
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    assert!(!killed, "the child blocked on a FIFO tag: the tag open must refuse the pipe without waiting");
    let o = child.wait_with_output().unwrap();
    assert_eq!(status.code(), o.status.code());
    assert_eq!(code(&o), 0, "the FIFO tag is a kept target, not a refusal: {}{}", out(&o), err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=1\n");
    assert!(t1.exists(), "the target holding the FIFO tag is kept");
}

/// A link named `target` pointing at a real folder with a good tag is not followed: kept=0, removed=0, the
/// link and the folder it points to byte-unchanged.
#[test]
fn a_linked_target_is_kept() {
    let f = fixture();
    let real = tagged(&f.gate.join("t-1/real-target"));
    let link = f.gate.join("t-1/target");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=0\n");
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link is still a link");
    assert!(real.join("CACHEDIR.TAG").exists(), "the folder behind the link is byte-unchanged");
}

/// A ticket folder that is a link is not followed: removed=0 kept=0, the outside tree it points to
/// byte-unchanged, the link itself intact.
#[test]
fn a_linked_ticket_folder_is_not_followed() {
    let f = fixture();
    let outside = f.t.raw.join("outside-tree");
    let t1 = build_output(&outside);
    std::os::unix::fs::symlink(&outside, f.gate.join("t-1")).unwrap();
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=0\n");
    assert!(t1.join("CACHEDIR.TAG").exists(), "the tree behind the linked ticket folder is byte-unchanged");
    assert!(fs::symlink_metadata(f.gate.join("t-1")).unwrap().file_type().is_symlink(), "the link is intact");
}

/// Folders of other tickets next to `<t>` (`<t>x`, `<t>-2`) are kept; only the ticket's own target goes.
#[test]
fn another_tickets_folder_is_kept() {
    let f = fixture();
    let mine = tagged(&f.gate.join("t-1/target"));
    let other1 = tagged(&f.gate.join("t-1x/target"));
    let other2 = tagged(&f.gate.join("t-1-2/target"));
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=1 kept=0\n");
    assert!(!mine.exists(), "the ticket's own target is gone");
    assert!(other1.join("CACHEDIR.TAG").exists(), "t-1x is another ticket's folder");
    assert!(other2.join("CACHEDIR.TAG").exists(), "t-1-2 is another ticket's folder");
}

/// A child that also matches another id under the three forms (`t-1.old.1.2` is also `t-1.old` plus `.1.2`)
/// is skipped when a manifest file for that other id exists in the manifest dir.
#[test]
fn an_ambiguous_child_with_its_own_manifest_is_kept() {
    let f = fixture();
    let amb = build_output(&f.gate.join("t-1.old.1.2"));
    fs::write(
        f.md.join("t-1.old.jsonl"),
        format!("{{\"v\":1,\"op\":\"root\",\"ticket\":\"t-1.old\",\"path\":\"{}\",\"ts_ms\":1}}\n", s(&f.gate.canonicalize().unwrap())),
    )
    .unwrap();
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=0\n");
    assert!(amb.join("CACHEDIR.TAG").exists(), "the ambiguous child is skipped, not visited");
    assert!(f.gate.join("t-1.old.1.2/core/rust/lib.rs").exists(), "nothing inside it was touched");
}

/// A target inside `.git` is kept: `.git` is not entered, so kept=0 and the target is intact.
#[test]
fn a_target_inside_git_is_kept() {
    let f = fixture();
    let inner = tagged(&f.gate.join("t-1/.git/x/target"));
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=0\n");
    assert!(inner.join("CACHEDIR.TAG").exists(), "the target inside .git is intact");
}

/// A folder named like the ticket in a deeper level is not a ticket folder: `root/other/<t>/target` stays.
#[test]
fn a_folder_named_like_the_ticket_in_a_deeper_level_is_not_a_ticket_folder() {
    let f = fixture();
    let deep = tagged(&f.gate.join("other/t-1/target"));
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=0 kept=0\n");
    assert!(deep.join("CACHEDIR.TAG").exists(), "the deeper same-named folder is intact");
}

/// An unreadable folder (mode 000) inside a ticket folder is a walk error: exit 3, one stderr line, nothing
/// removed. The mode is restored before cleanup; the assertion is skipped with a note when the tests run as
/// root.
#[test]
fn an_unreadable_folder_in_a_ticket_folder_exits_3_and_removes_nothing() {
    let enforced = modes_enforced();
    let f = fixture();
    let t1 = build_output(&f.gate.join("t-1"));
    let locked = f.gate.join("t-1/locked");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("f.txt"), b"x").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let o = drop_both(&f);
    let _ = fs::set_permissions(&locked, fs::Permissions::from_mode(0o755));
    if !enforced {
        eprintln!("note: running as root, the mode-000 walk error is not enforced; assertion skipped");
        return;
    }
    assert_eq!(code(&o), 3, "an unreadable folder is exit 3: {}{}", out(&o), err(&o));
    assert_eq!(out(&o), "", "stdout stays empty");
    assert!(err(&o).contains("cannot be read"), "the refusal names the walk error: {}", err(&o));
    assert!(t1.join("CACHEDIR.TAG").exists(), "nothing was removed");
    no_leak(&f, &o);
}

/// 1001 tagged targets is over the 1000-match cap: exit 3, one stderr line naming the cap, nothing removed.
#[test]
fn over_the_match_cap_exits_3_and_removes_nothing() {
    let f = fixture();
    for i in 0..1001 {
        tagged(&f.gate.join(format!("t-1/i{:04}/target", i)));
    }
    let o = drop_both(&f);
    assert_eq!(code(&o), 3, "over the match cap is exit 3: {}{}", out(&o), err(&o));
    assert_eq!(out(&o), "", "stdout stays empty");
    assert!(err(&o).contains("matches cap"), "the refusal names the cap: {}", err(&o));
    let left = fs::read_dir(f.gate.join("t-1")).unwrap().count();
    assert_eq!(left, 1001, "not one target folder was removed");
    no_leak(&f, &o);
}

/// A tagged target at depth 17 (the cap is 16 below the root) is over the depth cap: exit 3, one stderr
/// line naming the cap, nothing removed (the depth-3 target included).
#[test]
fn over_the_depth_cap_exits_3_and_removes_nothing() {
    let f = fixture();
    let shallow = tagged(&f.gate.join("t-1/a/target"));
    let mut deep = f.gate.join("t-1");
    for _ in 0..15 {
        deep = deep.join("d");
    }
    let deep_target = tagged(&deep.join("target"));
    let o = drop_both(&f);
    assert_eq!(code(&o), 3, "over the depth cap is exit 3: {}{}", out(&o), err(&o));
    assert_eq!(out(&o), "", "stdout stays empty");
    assert!(err(&o).contains("depth cap"), "the refusal names the cap: {}", err(&o));
    assert!(shallow.join("CACHEDIR.TAG").exists(), "the depth-3 target was not removed either");
    assert!(deep_target.join("CACHEDIR.TAG").exists(), "the depth-17 target is intact");
    no_leak(&f, &o);
}

// ---------------------------------------------------------------- the removal

/// Two tagged targets, the second holding a 0555 subfolder with a file: the run stops at that match —
/// exit 3, stdout empty, one stderr line holding removed=1 and neither temp path, the first target gone, the
/// second target's tag still there. Both mode changes are restored; the assertion is skipped with a note
/// when the tests run as root.
#[test]
fn a_read_only_folder_inside_target_exits_3() {
    let enforced = modes_enforced();
    let f = fixture();
    let a = tagged(&f.gate.join("t-1/a-target/target"));
    fs::write(a.join("f.txt"), b"a").unwrap();
    let b = tagged(&f.gate.join("t-1/b-target/target"));
    let ro = b.join("ro");
    fs::create_dir_all(&ro).unwrap();
    fs::write(ro.join("g.txt"), b"g").unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let o = drop_both(&f);
    let _ = fs::set_permissions(&ro, fs::Permissions::from_mode(0o755));
    if !enforced {
        eprintln!("note: running as root, the 0555 removal failure is not enforced; assertion skipped");
        return;
    }
    assert_eq!(code(&o), 3, "a half-removable target is exit 3: {}{}", out(&o), err(&o));
    assert_eq!(out(&o), "", "stdout stays empty on the mid-removal stop");
    assert!(err(&o).contains("removed=1"), "the line holds the removed count: {}", err(&o));
    assert!(err(&o).contains("could not be removed completely"), "the line names the failure: {}", err(&o));
    no_leak(&f, &o);
    assert!(!a.exists(), "the first target was removed before the failure");
    assert!(b.join("CACHEDIR.TAG").exists(), "the half-removed target keeps its tag (removed last)");
    assert!(ro.join("g.txt").exists(), "the read-only folder's file is intact");
}

/// After that run, the mode restored, the next run removes the half-removed target: removed=1, the target
/// gone (the tag left from the stopped run lets the walk find it again).
#[test]
fn a_half_removed_target_is_removed_by_the_next_run() {
    let enforced = modes_enforced();
    let f = fixture();
    let a = tagged(&f.gate.join("t-1/a-target/target"));
    fs::write(a.join("f.txt"), b"a").unwrap();
    let b = tagged(&f.gate.join("t-1/b-target/target"));
    let ro = b.join("ro");
    fs::create_dir_all(&ro).unwrap();
    fs::write(ro.join("g.txt"), b"g").unwrap();
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    let o1 = drop_both(&f);
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    if !enforced {
        eprintln!("note: running as root, the 0555 removal failure is not enforced; assertion skipped");
        return;
    }
    assert_eq!(code(&o1), 3, "the first run stops at the read-only target: {}{}", out(&o1), err(&o1));
    let o2 = drop_both(&f);
    assert_eq!(code(&o2), 0, "the second run succeeds: {}", err(&o2));
    assert_eq!(out(&o2), "drop-build=done ticket=t-1 removed=1 kept=0\n");
    assert!(!b.exists(), "the half-removed target is gone");
}

/// A link inside `target` pointing at a folder outside the roots is unlinked, not followed: the target is
/// removed and the outside folder is byte-unchanged.
#[test]
fn a_link_inside_target_is_unlinked_not_followed() {
    let f = fixture();
    let outside = f.t.raw.join("outside-keep");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), b"outside-payload").unwrap();
    let t1 = tagged(&f.gate.join("t-1/target"));
    std::os::unix::fs::symlink(&outside, t1.join("out-link")).unwrap();
    let o = drop_both(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "drop-build=done ticket=t-1 removed=1 kept=0\n");
    assert!(!t1.exists(), "the target holding the link is gone");
    assert_eq!(fs::read(outside.join("keep.txt")).unwrap(), b"outside-payload", "the outside folder is byte-unchanged");
}
