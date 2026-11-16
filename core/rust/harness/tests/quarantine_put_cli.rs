//! Process-level tests for `harness quarantine put` (ticket quarantine-put): one item moves onto the shelf under
//! `<quarantine-dir>/@shelf` with a label written before the move, or put refuses. Most checks run the real binary
//! against a scratch layout under the system temp folder; the seam tests call the library (`put_with_step`,
//! `put_with_device_probe`). Fixtures are synthetic: the `.git` entries and cache tags are plain folders and files
//! made by the test. No folder path may appear on either stream.
#![cfg(unix)]
use harness::quarantine::put::{self, Point, PutArgs};
use harness::quarantine::Stop;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);
/// 2026-01-01T00:00:00Z in milliseconds: the entry day folder is 20260101.
const TS: &str = "1767225600000";
const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";

/// One scratch layout: the RAW temp path and its canonical spelling are both kept, because on some platforms they
/// differ and neither may appear on stdout or stderr.
struct Tmp {
    raw: PathBuf,
    canon: PathBuf,
}

impl Tmp {
    fn new() -> Tmp {
        let raw = std::env::temp_dir().join(format!("qput-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

/// `w/qh/q` (the quarantine dir), `w/mdh/md` (the manifest dir) and `w/src` (where the items live).
struct Fix {
    t: Tmp,
    q: PathBuf,
    md: PathBuf,
    src: PathBuf,
}

fn fixture() -> Fix {
    let t = Tmp::new();
    let w = t.raw.join("w");
    let (q, md, src) = (w.join("qh/q"), w.join("mdh/md"), w.join("src"));
    for d in [&q, &md, &src] {
        fs::create_dir_all(d).unwrap();
    }
    Fix { t, q, md, src }
}

/// A plain folder with two files and a sub-folder: 4 entries, 11 bytes.
fn plain_item(f: &Fix, name: &str) -> PathBuf {
    let d = f.src.join(name);
    fs::create_dir_all(d.join("sub")).unwrap();
    fs::write(d.join("a.txt"), b"hello").unwrap();
    fs::write(d.join("sub/b.txt"), b"world!").unwrap();
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

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The flags of one put line, with `over` replacing a base flag's value (a repeat is refused by the parser) or
/// adding a flag.
fn put_args(f: &Fix, path: Option<&str>, over: &[(&str, &str)]) -> Vec<String> {
    let mut pairs: Vec<(String, String)> = vec![
        ("--quarantine-dir".into(), s(&f.q)),
        ("--manifest-dir".into(), s(&f.md)),
        ("--kind".into(), "hand".into()),
        ("--why".into(), "a test item".into()),
        ("--by".into(), "tester".into()),
        ("--ts-ms".into(), TS.into()),
    ];
    for (k, v) in over {
        match pairs.iter_mut().find(|p| p.0 == *k) {
            Some(p) => p.1 = v.to_string(),
            None => pairs.push((k.to_string(), v.to_string())),
        }
    }
    let mut v: Vec<String> = vec!["quarantine".into(), "put".into()];
    v.extend(path.map(|p| p.to_string()));
    for (k, val) in pairs {
        v.push(k);
        v.push(val);
    }
    v
}

fn run_vec(v: &[String]) -> Output {
    let refs: Vec<&str> = v.iter().map(|x| x.as_str()).collect();
    run(&refs)
}

fn put_str(f: &Fix, path: &str, over: &[(&str, &str)]) -> Output {
    run_vec(&put_args(f, Some(path), over))
}

fn put_cmd(f: &Fix, path: &Path, over: &[(&str, &str)]) -> Output {
    put_str(f, &s(path), over)
}

fn pargs(f: &Fix, path: &Path) -> PutArgs {
    PutArgs {
        path: path.to_path_buf(),
        quarantine_dir: f.q.clone(),
        manifest_dir: f.md.clone(),
        kind: "hand".into(),
        why: "a test item".into(),
        by: "tester".into(),
        ticket: String::new(),
        repo: String::new(),
        ts_ms: TS.parse().unwrap(),
    }
}

fn dev_of(p: &Path) -> Option<u64> {
    fs::symlink_metadata(p).ok().map(|m| m.dev())
}

/// Neither stream holds the temp folder path, raw or canonical.
fn no_leak(f: &Fix, o: &Output) {
    for stream in [out(o), err(o)] {
        for secret in [f.t.raw.to_string_lossy().to_string(), f.t.canon.to_string_lossy().to_string()] {
            assert!(!stream.contains(&secret), "a folder path leaked: {stream}");
        }
    }
}

/// A full listing: sorted relative paths with file bytes, folders as None, links as their text.
fn snap(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    fn walk(dir: &Path, base: &Path, v: &mut Vec<(String, Option<Vec<u8>>)>) {
        let mut rd: Vec<PathBuf> = fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        rd.sort();
        for p in rd {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
            let m = fs::symlink_metadata(&p).unwrap();
            if m.file_type().is_symlink() {
                v.push((rel, Some(format!("->{}", fs::read_link(&p).unwrap().display()).into_bytes())));
            } else if m.is_dir() {
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

/// A refusal at `want`: stdout empty, stderr starting `harness: error:`, no leaked path.
fn refused(f: &Fix, o: &Output, want: i32) {
    assert_eq!(code(o), want, "exit {want}: {}{}", out(o), err(o));
    assert_eq!(out(o), "", "stdout stays empty on a refusal");
    assert!(err(o).starts_with("harness: error: "), "stderr: {}", err(o));
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

fn entry_dir(f: &Fix, name: &str) -> PathBuf {
    f.q.join("@shelf/entries/20260101").join(name)
}

fn read(p: &Path) -> String {
    String::from_utf8_lossy(&fs::read(p).unwrap()).to_string()
}

/// The manifest of `ticket` with `root` and one live path entry per path in `entries`.
fn write_manifest(f: &Fix, ticket: &str, root: &Path, entries: &[PathBuf]) {
    let mut lines = vec![format!("{{\"v\":1,\"op\":\"root\",\"ticket\":\"{ticket}\",\"path\":\"{}\",\"ts_ms\":1}}", s(root))];
    for (i, e) in entries.iter().enumerate() {
        lines.push(format!(
            "{{\"v\":1,\"op\":\"add\",\"ticket\":\"{ticket}\",\"id\":\"e{i}\",\"class\":\"temp_dir\",\"path\":\"{}\",\"ts_ms\":2}}",
            s(e)
        ));
    }
    fs::write(f.md.join(format!("{ticket}.jsonl")), lines.join("\n") + "\n").unwrap();
}

// A small sha256 (FIPS 180-4) so the journal test can check the label digest without a tool.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
        0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
        0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for (&k, &wi) in K.iter().zip(w.iter()) {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(k).wrapping_add(wi);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip(v) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

// ---------------------------------------------------------------- the worked cases

#[test]
fn the_worked_case() {
    let f = fixture();
    let item = plain_item(&f, "proj");
    let before = snap(&item);
    let o = put_cmd(&f, &item, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "put=done entry=20260101/hand-1 files=4 bytes=11\n");
    assert_eq!(err(&o), "", "stderr stays empty on success");
    assert!(!item.exists(), "the source is gone");
    let entry = entry_dir(&f, "hand-1");
    assert_eq!(snap(&entry.join("item")), before, "the item is byte-identical under the entry");
    let label = read(&entry.join("label.json"));
    assert!(label.starts_with("{\"v\":1,\"entry\":\"20260101/hand-1\",\"kind\":\"hand\",\"from_hex\":\""), "label: {label}");
    let from = format!("{}/proj", s(&f.t.canon.join("w/src")));
    assert!(label.contains(&format!("\"from_hex\":\"{}\"", hex(from.as_bytes()))), "label: {label}");
    assert!(label.contains("\"state\":\"moved\"") && label.contains("\"judgeable\":\"yes\""), "label: {label}");
    assert!(label.contains("\"files\":4,\"bytes\":11,\"skipped_build\":0"), "label: {label}");
    assert!(label.ends_with("\"git\":\"none\"}\n"), "label: {label}");
    let journal = read(&f.q.join("@shelf/journal/20260101-1.jsonl"));
    let lines: Vec<&str> = journal.lines().collect();
    assert_eq!(lines.len(), 2, "journal: {journal}");
    assert!(lines[0].starts_with("{\"v\":1,\"op\":\"intent\",\"entry\":\"20260101/hand-1\",\"src_dev\":"), "journal: {journal}");
    assert!(lines[1].starts_with("{\"v\":1,\"op\":\"moved\",\"entry\":\"20260101/hand-1\",\"label_sha\":\""), "journal: {journal}");
    no_leak(&f, &o);
}

#[test]
fn a_regular_file_item_is_moved() {
    let f = fixture();
    let item = f.src.join("one.bin");
    fs::write(&item, b"abc").unwrap();
    let o = put_cmd(&f, &item, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "put=done entry=20260101/hand-1 files=1 bytes=3\n");
    assert!(!item.exists());
    assert_eq!(fs::read(entry_dir(&f, "hand-1").join("item")).unwrap(), b"abc");
}

#[test]
fn two_puts_on_one_day_get_two_entries() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    let o1 = put_cmd(&f, &a, &[]);
    let o2 = put_cmd(&f, &b, &[]);
    assert_eq!(out(&o1), "put=done entry=20260101/hand-1 files=4 bytes=11\n");
    assert_eq!(out(&o2), "put=done entry=20260101/hand-2 files=4 bytes=11\n");
    assert!(entry_dir(&f, "hand-1").join("item").exists() && entry_dir(&f, "hand-2").join("item").exists());
}

#[test]
fn usage_errors_exit_2_with_the_usage() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let p = s(&item);
    let mut cases: Vec<Vec<String>> = Vec::new();
    let mut unknown = put_args(&f, Some(&p), &[]);
    unknown.extend(["--bogus".to_string(), "x".to_string()]);
    cases.push(unknown);
    let mut missing = put_args(&f, Some(&p), &[]);
    missing.push("--ticket".to_string());
    cases.push(missing);
    let mut repeated = put_args(&f, Some(&p), &[]);
    repeated.extend(["--kind".to_string(), "hand".to_string()]);
    cases.push(repeated);
    cases.push(put_args(&f, None, &[]));
    let mut second = put_args(&f, Some(&p), &[]);
    second.push(s(&f.src.join("other")));
    cases.push(second);
    for c in cases {
        let o = run_vec(&c);
        assert_eq!(code(&o), 2, "{c:?}: {}", err(&o));
        assert_eq!(out(&o), "");
        assert!(err(&o).contains("usage: harness quarantine"), "stderr: {}", err(&o));
        assert!(err(&o).contains("quarantine put <path> --quarantine-dir <abs>"), "stderr: {}", err(&o));
    }
    assert!(item.exists() && snap(&f.q).is_empty());
}

// ---------------------------------------------------------------- steps 2 to 4

#[test]
fn a_source_that_is_a_link_exits_2() {
    let f = fixture();
    let real = plain_item(&f, "real");
    let lnk = f.src.join("lnk");
    symlink(&real, &lnk).unwrap();
    let before = snap(&real);
    let o = put_cmd(&f, &lnk, &[]);
    refused(&f, &o, 2);
    assert!(err(&o).contains("link"), "stderr names the link: {}", err(&o));
    assert_eq!(snap(&real), before);
    assert!(fs::symlink_metadata(&lnk).unwrap().file_type().is_symlink());
    assert!(snap(&f.q).is_empty(), "nothing was written");
}

#[test]
fn a_source_written_with_a_trailing_slash_exits_2() {
    let f = fixture();
    let dir = plain_item(&f, "dir");
    let real = plain_item(&f, "real");
    let lnk = f.src.join("lnk");
    symlink(&real, &lnk).unwrap();
    let before = snap(&f.src);
    for spelled in [format!("{}/", s(&dir)), format!("{}/.", s(&dir)), format!("{}/", s(&lnk)), format!("{}/.", s(&lnk))] {
        let o = put_str(&f, &spelled, &[]);
        refused(&f, &o, 2);
        assert_eq!(snap(&f.src), before, "{spelled}");
        assert!(snap(&f.q).is_empty());
    }
}

#[test]
fn a_quarantine_dir_link_exits_2() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let lq = f.t.raw.join("w/ql");
    symlink(&f.q, &lq).unwrap();
    let before = snap(&item);
    for spelled in [s(&lq), format!("{}/", s(&lq)), format!("{}/.", s(&lq))] {
        let o = put_cmd(&f, &item, &[("--quarantine-dir", &spelled)]);
        refused(&f, &o, 2);
        assert_eq!(snap(&item), before, "{spelled}");
        assert!(snap(&f.q).is_empty(), "{spelled}");
    }
}

#[test]
fn a_relative_source_exits_2() {
    let f = fixture();
    plain_item(&f, "p");
    let o = put_str(&f, "relative/p", &[]);
    refused(&f, &o, 2);
    assert!(snap(&f.q).is_empty());
}

#[test]
fn a_source_inside_the_quarantine_dir_exits_2() {
    let f = fixture();
    let inside = f.q.join("x");
    fs::create_dir_all(&inside).unwrap();
    fs::write(inside.join("k.txt"), b"k").unwrap();
    let before = snap(&f.q);
    let o = put_cmd(&f, &inside, &[]);
    refused(&f, &o, 2);
    assert_eq!(snap(&f.q), before);
}

#[test]
fn a_source_holding_the_quarantine_dir_exits_2() {
    let f = fixture();
    let holder = f.q.parent().unwrap().to_path_buf();
    let before = snap(&holder);
    let o = put_cmd(&f, &holder, &[]);
    refused(&f, &o, 2);
    assert_eq!(snap(&holder), before);
}

#[test]
fn a_source_holding_the_manifest_dir_exits_2() {
    let f = fixture();
    let holder = f.md.parent().unwrap().to_path_buf();
    let before = snap(&holder);
    let o = put_cmd(&f, &holder, &[]);
    refused(&f, &o, 2);
    assert_eq!(snap(&holder), before);
}

// ---------------------------------------------------------------- step 5: the live check

#[test]
fn a_source_inside_a_live_manifest_entry_exits_2() {
    let f = fixture();
    let live = f.t.canon.join("w/live");
    fs::create_dir_all(live.join("tree/sub")).unwrap();
    fs::write(live.join("tree/sub/k.txt"), b"k").unwrap();
    write_manifest(&f, "t-1", &live, &[live.join("tree")]);
    let before = snap(&live);
    let o = put_cmd(&f, &f.t.raw.join("w/live/tree/sub"), &[]);
    refused(&f, &o, 2);
    assert_eq!(snap(&live), before);
    assert!(snap(&f.q).is_empty());
}

#[test]
fn a_source_holding_a_live_manifest_entry_exits_2() {
    let f = fixture();
    let live = f.t.canon.join("w/live");
    fs::create_dir_all(live.join("tree/inner")).unwrap();
    fs::create_dir_all(live.join("tree2")).unwrap();
    // an entry that exists (identities) and one that does not (canonical text)
    for (entry, source) in [(live.join("tree/inner"), live.join("tree")), (live.join("tree2/ghost"), live.join("tree2"))] {
        write_manifest(&f, "t-1", &live, &[entry]);
        let before = snap(&live);
        let o = put_cmd(&f, &source, &[]);
        refused(&f, &o, 2);
        assert_eq!(snap(&live), before);
        assert!(snap(&f.q).is_empty());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_source_spelled_in_other_case_equal_to_a_live_entry_exits_2() {
    let f = fixture();
    let live = f.t.canon.join("w/live");
    fs::create_dir_all(live.join("Entry")).unwrap();
    if !live.join("entry").exists() {
        eprintln!("skipped: the temp folder is case-sensitive");
        return;
    }
    write_manifest(&f, "t-1", &live, &[live.join("Entry")]);
    let o = put_cmd(&f, &f.t.raw.join("w/live/entry"), &[]);
    refused(&f, &o, 2);
    assert!(live.join("Entry").exists());
    assert!(snap(&f.q).is_empty());
}

#[test]
fn a_torn_manifest_exits_2() {
    let f = fixture();
    let item = plain_item(&f, "p");
    fs::write(f.md.join("t-9.jsonl"), b"{\"v\":1,\"op\":\"root\"").unwrap();
    let before = snap(&item);
    let o = put_cmd(&f, &item, &[]);
    refused(&f, &o, 2);
    assert!(err(&o).contains("t-9"), "stderr names the ticket id: {}", err(&o));
    assert_eq!(snap(&item), before);
    assert!(snap(&f.q).is_empty());
}

#[test]
fn a_manifest_that_is_not_a_regular_file_exits_2() {
    let f = fixture();
    let item = plain_item(&f, "p");
    // A FIFO blocks a read, so put must refuse it from the stat alone; the deadline turns a block into a failure.
    let mk = Command::new("mkfifo").arg(f.md.join("t-3.jsonl")).status().unwrap();
    assert!(mk.success(), "mkfifo failed");
    let before = snap(&item);
    let mut c = Command::new(BIN);
    c.args(put_args(&f, Some(&s(&item)), &[])).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL", "HARNESS_CONFIG"] {
        c.env_remove(k);
    }
    let mut child = c.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("put blocked on the FIFO");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let o = child.wait_with_output().unwrap();
    refused(&f, &o, 2);
    assert!(err(&o).contains("t-3"), "stderr names the ticket id: {}", err(&o));
    assert_eq!(snap(&item), before);
    assert!(snap(&f.q).is_empty());
}

#[test]
fn a_manifest_name_that_is_not_a_token_exits_2() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let bad = f.md.join("Bad Name.jsonl");
    fs::write(&bad, b"").unwrap();
    let before = snap(&item);
    let o = put_cmd(&f, &item, &[]);
    refused(&f, &o, 2);
    assert_eq!(snap(&item), before);
    assert!(snap(&f.q).is_empty());
    // other names and sub-folders are skipped
    fs::remove_file(&bad).unwrap();
    fs::create_dir_all(f.md.join("other.jsonl")).unwrap();
    fs::write(f.md.join("notes.txt"), b"n").unwrap();
    let o = put_cmd(&f, &item, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
}

// ---------------------------------------------------------------- step 7: the shelf

#[test]
fn a_shelf_folder_that_is_a_link_exits_2() {
    for case in ["@shelf", "lock", "entries", "date", "journal"] {
        let f = fixture();
        let item = plain_item(&f, "p");
        let else_dir = f.t.raw.join("w/else");
        fs::create_dir_all(&else_dir).unwrap();
        fs::write(else_dir.join("k.txt"), b"keep").unwrap();
        let else_file = f.t.raw.join("w/else.txt");
        fs::write(&else_file, b"keep-file").unwrap();
        let shelf = f.q.join("@shelf");
        match case {
            "@shelf" => symlink(&else_dir, &shelf).unwrap(),
            "lock" => {
                fs::create_dir_all(&shelf).unwrap();
                symlink(&else_file, shelf.join("lock")).unwrap();
            }
            "entries" => {
                fs::create_dir_all(&shelf).unwrap();
                symlink(&else_dir, shelf.join("entries")).unwrap();
            }
            "date" => {
                fs::create_dir_all(shelf.join("entries")).unwrap();
                symlink(&else_dir, shelf.join("entries/20260101")).unwrap();
            }
            _ => {
                fs::create_dir_all(shelf.join("entries/20260101")).unwrap();
                symlink(&else_dir, shelf.join("journal")).unwrap();
            }
        }
        let (src_before, else_before) = (snap(&item), snap(&else_dir));
        let o = put_cmd(&f, &item, &[]);
        refused(&f, &o, 2);
        assert!(err(&o).contains("nothing was moved"), "{case}: {}", err(&o));
        assert_eq!(snap(&item), src_before, "{case}");
        assert_eq!(snap(&else_dir), else_before, "{case}");
        assert_eq!(fs::read(&else_file).unwrap(), b"keep-file", "{case}");
    }
}

#[test]
fn a_second_put_while_the_shelf_lock_is_held_exits_3() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    assert_eq!(code(&put_cmd(&f, &a, &[])), 0);
    let lock = fs::OpenOptions::new().read(true).write(true).open(f.q.join("@shelf/lock")).unwrap();
    lock.lock().unwrap();
    let before = snap(&b);
    let o = put_cmd(&f, &b, &[]);
    refused(&f, &o, 3);
    assert!(err(&o).contains("a put is running"), "stderr: {}", err(&o));
    assert_eq!(snap(&b), before);
    drop(lock);
    assert_eq!(code(&put_cmd(&f, &b, &[])), 0, "the lock was the cause");
}

#[test]
fn a_bad_kind_or_why_exits_2() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let before = snap(&item);
    let long = "a".repeat(201);
    let cases = [
        ("--kind", "bogus"),
        ("--why", ""),
        ("--why", long.as_str()),
        ("--why", "a\"b"),
        ("--why", "a\\b"),
        ("--why", "caf\u{e9}"),
        ("--why", "a\tb"),
        ("--by", "Bad Token"),
        ("--ticket", "Bad"),
        ("--repo", "Bad"),
        ("--ts-ms", "12x"),
    ];
    for (k, v) in cases {
        let o = put_cmd(&f, &item, &[(k, v)]);
        refused(&f, &o, 2);
        assert_eq!(snap(&item), before, "{k}");
        assert!(snap(&f.q).is_empty(), "{k}");
    }
    let max = "a".repeat(200);
    let o = put_cmd(&f, &item, &[("--why", max.as_str())]);
    assert_eq!(code(&o), 0, "200 bytes pass: {}", err(&o));
}

#[test]
fn cross_device_exits_3() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let before = snap(&item);
    let qc = f.t.canon.join("w/qh/q");
    let r = put::put_with_device_probe(&pargs(&f, &item), &|p| if p.starts_with(&qc) { Some(1) } else { Some(2) });
    match r {
        Err(Stop(3, line)) => {
            assert!(line.contains("cross-device"), "line: {line}");
            assert!(!line.contains(&s(&f.t.raw)) && !line.contains(&s(&f.t.canon)), "a path leaked: {line}");
        }
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
        Ok(line) => panic!("done: {line}"),
    }
    assert_eq!(snap(&item), before);
    assert!(snap(&f.q).is_empty(), "nothing was written");
}

// ---------------------------------------------------------------- the walk and the fingerprint

fn git_refused(f: &Fix, item: &Path) {
    let before = snap(item);
    let o = put_cmd(f, item, &[]);
    refused(f, &o, 2);
    assert!(err(&o).contains("git") && err(&o).contains("nothing was moved"), "stderr: {}", err(&o));
    assert_eq!(snap(item), before);
    assert!(snap(&f.q).is_empty(), "nothing was written");
}

#[test]
fn an_item_holding_git_at_the_top_exits_2() {
    let f = fixture();
    let item = plain_item(&f, "p");
    fs::create_dir_all(item.join(".git")).unwrap();
    fs::write(item.join(".git/HEAD"), b"ref").unwrap();
    git_refused(&f, &item);
}

#[test]
fn an_item_holding_nested_git_exits_2() {
    let f = fixture();
    let item = plain_item(&f, "p");
    fs::create_dir_all(item.join("sub/deep")).unwrap();
    fs::write(item.join("sub/deep/.GiT"), b"gitdir: x").unwrap();
    git_refused(&f, &item);
}

#[test]
fn a_git_link_inside_the_item_exits_2() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let elsewhere = f.t.raw.join("w/elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    symlink(&elsewhere, item.join(".git")).unwrap();
    git_refused(&f, &item);
}

#[test]
fn a_tagged_target_moves_and_is_counted() {
    let f = fixture();
    let item = f.src.join("proj");
    let target = item.join("proj/target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("CACHEDIR.TAG"), TAG).unwrap();
    fs::write(target.join("big.bin"), b"payload").unwrap();
    fs::write(item.join("proj/src.rs"), b"fn main() {}").unwrap();
    let o = put_cmd(&f, &item, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "put=done entry=20260101/hand-1 files=4 bytes=12\n");
    let entry = entry_dir(&f, "hand-1");
    assert!(read(&entry.join("label.json")).contains("\"skipped_build\":1"));
    assert_eq!(fs::read(entry.join("item/proj/target/CACHEDIR.TAG")).unwrap(), TAG.as_bytes());
    assert_eq!(fs::read(entry.join("item/proj/target/big.bin")).unwrap(), b"payload");
}

#[test]
fn an_untagged_target_is_part_of_the_fingerprint() {
    let f = fixture();
    let (plain, tagged) = (f.src.join("plain"), f.src.join("tagged"));
    for (item, tag) in [(&plain, false), (&tagged, true)] {
        let t = item.join("target");
        fs::create_dir_all(&t).unwrap();
        fs::write(t.join("x.bin"), b"xx").unwrap();
        if tag {
            fs::write(t.join("CACHEDIR.TAG"), TAG).unwrap();
        }
    }
    let edit = |item: PathBuf| {
        move |pt: Point| -> Result<(), Stop> {
            if pt == Point::AfterIntent {
                let mut x = fs::OpenOptions::new().append(true).open(item.join("target/x.bin")).unwrap();
                x.write_all(b"more").unwrap();
            }
            Ok(())
        }
    };
    // the untagged target is walked: the edit changes the size, the put says so
    let r = put::put_with_step(&pargs(&f, &plain), &dev_of, &edit(plain.clone()));
    match r {
        Err(Stop(3, line)) => assert!(line.contains("changed while it was put"), "line: {line}"),
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
        Ok(line) => panic!("done: {line}"),
    }
    assert!(read(&entry_dir(&f, "hand-1").join("label.json")).contains("\"skipped_build\":0"));
    // the tagged target is not entered: the same edit goes unseen
    let r = put::put_with_step(&pargs(&f, &tagged), &dev_of, &edit(tagged.clone()));
    match r {
        Ok(line) => assert!(line.starts_with("put=done entry=20260101/hand-2"), "line: {line}"),
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
    }
}

#[test]
fn an_unreadable_folder_in_the_item_exits_3_and_moves_nothing() {
    if !modes_enforced() {
        eprintln!("skipped: folder modes are not enforced for this user");
        return;
    }
    let f = fixture();
    let item = plain_item(&f, "p");
    let locked = item.join("locked");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("k.txt"), b"k").unwrap();
    let before = snap(&item);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let o = put_cmd(&f, &item, &[]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    refused(&f, &o, 3);
    assert!(err(&o).contains("cannot be read"), "stderr: {}", err(&o));
    assert_eq!(snap(&item), before);
    assert!(snap(&f.q).is_empty(), "nothing was written");
}

#[test]
fn over_the_depth_cap_exits_3_and_moves_nothing() {
    let f = fixture();
    let item = f.src.join("deep");
    let mut p = item.clone();
    for _ in 0..70 {
        p.push("d");
    }
    fs::create_dir_all(&p).unwrap();
    let before = snap(&item);
    let o = put_cmd(&f, &item, &[]);
    refused(&f, &o, 3);
    assert!(err(&o).contains("depth cap (64)"), "stderr: {}", err(&o));
    assert_eq!(snap(&item), before);
    assert!(snap(&f.q).is_empty(), "nothing was written");
}

// ---------------------------------------------------------------- entry, label, journal and the move

#[test]
fn the_label_is_written_before_the_move() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let entry = entry_dir(&f, "hand-1");
    let journal = f.q.join("@shelf/journal/20260101-1.jsonl");
    let step = |pt: Point| -> Result<(), Stop> {
        if pt == Point::AfterIntent {
            let label = read(&entry.join("label.json"));
            assert!(label.contains("\"state\":\"intent\"") && label.contains("\"fingerprint_after\":\"\""), "label: {label}");
            let j = read(&journal);
            assert!(j.contains("\"op\":\"intent\"") && !j.contains("\"op\":\"moved\""), "journal: {j}");
            assert!(item.exists(), "the source is still in place");
            assert!(!entry.join("item").exists(), "nothing is in the entry yet");
            return Err(Stop(3, "halt".to_string()));
        }
        Ok(())
    };
    match put::put_with_step(&pargs(&f, &item), &dev_of, &step) {
        Err(Stop(3, line)) => assert_eq!(line, "halt"),
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
        Ok(line) => panic!("done: {line}"),
    }
    assert!(item.exists(), "still in place after the stop");
    assert!(read(&entry.join("label.json")).contains("\"state\":\"intent\""));
    assert_eq!(read(&journal).lines().count(), 1, "one intent line and nothing more");
}

#[test]
fn the_journal_holds_the_final_label_sha() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let o = put_cmd(&f, &item, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let label = fs::read(entry_dir(&f, "hand-1").join("label.json")).unwrap();
    let journal = read(&f.q.join("@shelf/journal/20260101-1.jsonl"));
    let last = journal.lines().last().unwrap();
    assert_eq!(last, format!("{{\"v\":1,\"op\":\"moved\",\"entry\":\"20260101/hand-1\",\"label_sha\":\"{}\"}}", sha256_hex(&label)));
}

#[test]
fn a_taken_entry_name_is_skipped() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let taken = entry_dir(&f, "hand-1");
    fs::create_dir_all(&taken).unwrap();
    fs::write(taken.join("keep.txt"), b"keep").unwrap();
    let before = snap(&taken);
    let o = put_cmd(&f, &item, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("entry=20260101/hand-2"), "stdout: {}", out(&o));
    assert_eq!(snap(&taken), before, "the taken entry is byte-unchanged");
    assert!(entry_dir(&f, "hand-2").join("item").exists());
}

#[test]
fn a_change_during_put_is_labelled_and_exits_3() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let step = |pt: Point| -> Result<(), Stop> {
        if pt == Point::AfterIntent {
            let mut x = fs::OpenOptions::new().append(true).open(item.join("a.txt")).unwrap();
            x.write_all(b" and more").unwrap();
        }
        Ok(())
    };
    match put::put_with_step(&pargs(&f, &item), &dev_of, &step) {
        Err(Stop(3, line)) => assert_eq!(line, "harness: error: the item changed while it was put; it is on the shelf with a label that says so"),
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
        Ok(line) => panic!("done: {line}"),
    }
    let entry = entry_dir(&f, "hand-1");
    let label = read(&entry.join("label.json"));
    assert!(label.contains("\"state\":\"moved\"") && label.contains("\"judgeable\":\"no\"") && label.contains("\"reason\":\"changed-during-put\""), "label: {label}");
    assert!(entry.join("item/a.txt").exists() && !item.exists());
    let journal = read(&f.q.join("@shelf/journal/20260101-1.jsonl"));
    assert!(journal.lines().last().unwrap().contains("\"op\":\"moved\""), "journal: {journal}");
}

#[test]
fn a_source_gone_before_the_move_is_void_and_exits_3() {
    let f = fixture();
    let item = plain_item(&f, "p");
    let gone = f.src.join("gone");
    let step = |pt: Point| -> Result<(), Stop> {
        if pt == Point::AfterIntent {
            fs::rename(&item, &gone).unwrap();
        }
        Ok(())
    };
    match put::put_with_step(&pargs(&f, &item), &dev_of, &step) {
        Err(Stop(3, line)) => assert!(line.contains("did not happen") && line.contains("nothing was moved"), "line: {line}"),
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
        Ok(line) => panic!("done: {line}"),
    }
    let entry = entry_dir(&f, "hand-1");
    assert!(read(&entry.join("label.json")).contains("\"state\":\"void\""));
    assert!(!entry.join("item").exists());
    let journal = read(&f.q.join("@shelf/journal/20260101-1.jsonl"));
    assert!(journal.lines().last().unwrap().contains("\"op\":\"void\""), "journal: {journal}");
    let next = plain_item(&f, "next");
    let o = put_cmd(&f, &next, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("entry=20260101/hand-2"), "stdout: {}", out(&o));
}

#[cfg(target_os = "linux")]
#[test]
fn a_non_utf8_source_name_is_moved_with_its_exact_bytes_in_from_hex() {
    use std::os::unix::ffi::OsStrExt;
    let f = fixture();
    let raw_name: &[u8] = b"bad-\xff-name";
    let item = f.src.join(std::ffi::OsStr::from_bytes(raw_name));
    fs::create_dir_all(&item).unwrap();
    fs::write(item.join("x"), b"x").unwrap();
    match put::put(&pargs(&f, &item)) {
        Ok(line) => assert!(line.starts_with("put=done entry=20260101/hand-1"), "line: {line}"),
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
    }
    let mut want = f.t.canon.join("w/src").as_os_str().as_bytes().to_vec();
    want.push(b'/');
    want.extend_from_slice(raw_name);
    let label = read(&entry_dir(&f, "hand-1").join("label.json"));
    assert!(label.contains(&format!("\"from_hex\":\"{}\"", hex(&want))), "label: {label}");
    assert!(entry_dir(&f, "hand-1").join("item/x").exists() && !item.exists());
}

#[test]
fn a_torn_journal_tail_starts_a_new_journal_file() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    assert_eq!(code(&put_cmd(&f, &a, &[])), 0);
    let first = f.q.join("@shelf/journal/20260101-1.jsonl");
    let mut fh = fs::OpenOptions::new().append(true).open(&first).unwrap();
    fh.write_all(b"{\"v\":1,\"op\":\"mov").unwrap();
    drop(fh);
    let first_bytes = fs::read(&first).unwrap();
    let o = put_cmd(&f, &b, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(fs::read(&first).unwrap(), first_bytes, "the first file is byte-unchanged");
    let second = read(&f.q.join("@shelf/journal/20260101-2.jsonl"));
    let lines: Vec<&str> = second.lines().collect();
    assert_eq!(lines.len(), 2, "journal: {second}");
    assert!(lines[0].contains("\"op\":\"intent\"") && lines[1].contains("\"op\":\"moved\""), "journal: {second}");
    assert!(lines[0].contains("20260101/hand-2"));
}

// ---------------------------------------------------------------- the settle of a stopped put

fn journal1(f: &Fix) -> PathBuf {
    f.q.join("@shelf/journal/20260101-1.jsonl")
}

/// Put `item` in process and stop at `at` (the shape of a crash there).
fn stop_at(f: &Fix, item: &Path, at: Point) {
    let step = move |pt: Point| -> Result<(), Stop> {
        if pt == at {
            Err(Stop(3, "halt".to_string()))
        } else {
            Ok(())
        }
    };
    match put::put_with_step(&pargs(f, item), &dev_of, &step) {
        Err(Stop(3, line)) => assert_eq!(line, "halt"),
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
        Ok(line) => panic!("done: {line}"),
    }
}

#[test]
fn a_put_stopped_after_the_intent_is_voided_by_the_next_put() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    let a_before = snap(&a);
    stop_at(&f, &a, Point::AfterIntent);
    let o = put_cmd(&f, &b, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(out(&o).contains("entry=20260101/hand-2"), "stdout: {}", out(&o));
    no_leak(&f, &o);
    let label = fs::read(entry_dir(&f, "hand-1").join("label.json")).unwrap();
    assert!(String::from_utf8_lossy(&label).contains("\"state\":\"void\""));
    let journal = read(&journal1(&f));
    let want = format!("{{\"v\":1,\"op\":\"void\",\"entry\":\"20260101/hand-1\",\"label_sha\":\"{}\"}}", sha256_hex(&label));
    assert!(journal.lines().any(|l| l == want), "journal: {journal}");
    assert_eq!(snap(&a), a_before, "the first source is byte-unchanged");
    assert!(!entry_dir(&f, "hand-1").join("item").exists());
}

#[test]
fn a_put_stopped_after_the_move_is_finished_by_the_next_put() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    stop_at(&f, &a, Point::AfterMove);
    let o = put_cmd(&f, &b, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    no_leak(&f, &o);
    let label = fs::read(entry_dir(&f, "hand-1").join("label.json")).unwrap();
    let text = String::from_utf8_lossy(&label).to_string();
    assert!(
        text.contains("\"state\":\"moved\"") && text.contains("\"judgeable\":\"yes\"") && !text.contains("\"fingerprint_after\":\"\""),
        "label: {text}"
    );
    let journal = read(&journal1(&f));
    let want = format!("{{\"v\":1,\"op\":\"moved\",\"entry\":\"20260101/hand-1\",\"label_sha\":\"{}\"}}", sha256_hex(&label));
    assert!(journal.lines().any(|l| l == want), "journal: {journal}");
    assert!(entry_dir(&f, "hand-1").join("item/a.txt").exists());
}

#[test]
fn a_new_folder_at_the_old_source_path_does_not_change_the_settle() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    stop_at(&f, &a, Point::AfterMove);
    assert!(!a.exists());
    plain_item(&f, "a");
    fs::write(a.join("new.txt"), b"later").unwrap();
    let a_before = snap(&a);
    let o = put_cmd(&f, &b, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    no_leak(&f, &o);
    let text = read(&entry_dir(&f, "hand-1").join("label.json"));
    assert!(text.contains("\"state\":\"moved\"") && text.contains("\"judgeable\":\"yes\""), "label: {text}");
    assert_eq!(snap(&a), a_before, "the new folder is byte-unchanged");
}

#[test]
fn an_item_with_another_inode_in_an_unsettled_entry_exits_3() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    stop_at(&f, &a, Point::AfterIntent);
    let other = entry_dir(&f, "hand-1").join("item");
    fs::create_dir_all(other.join("x")).unwrap();
    fs::write(other.join("x/k.txt"), b"keep").unwrap();
    let (other_before, b_before) = (snap(&other), snap(&b));
    let o = put_cmd(&f, &b, &[]);
    refused(&f, &o, 3);
    assert!(err(&o).contains("cannot be decided") && err(&o).contains("nothing was moved"), "stderr: {}", err(&o));
    assert_eq!(snap(&other), other_before);
    assert_eq!(snap(&b), b_before);
    assert!(a.exists() && !entry_dir(&f, "hand-2").exists());
}

#[test]
fn a_label_that_does_not_match_its_intent_sha_exits_3() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    stop_at(&f, &a, Point::AfterIntent);
    let label = entry_dir(&f, "hand-1").join("label.json");
    let mut bytes = fs::read(&label).unwrap();
    bytes.pop();
    bytes.extend_from_slice(b" \n");
    fs::write(&label, &bytes).unwrap();
    let (a_before, b_before) = (snap(&a), snap(&b));
    let o = put_cmd(&f, &b, &[]);
    refused(&f, &o, 3);
    assert!(err(&o).contains("cannot be decided"), "stderr: {}", err(&o));
    assert_eq!(fs::read(&label).unwrap(), bytes);
    assert_eq!((snap(&a), snap(&b)), (a_before, b_before));
}

#[test]
fn a_change_before_the_stop_is_labelled_by_the_settle() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    let moved = entry_dir(&f, "hand-1").join("item/a.txt");
    let step = |pt: Point| -> Result<(), Stop> {
        if pt == Point::AfterMove {
            let mut x = fs::OpenOptions::new().append(true).open(&moved).unwrap();
            x.write_all(b" and more").unwrap();
            return Err(Stop(3, "halt".to_string()));
        }
        Ok(())
    };
    match put::put_with_step(&pargs(&f, &a), &dev_of, &step) {
        Err(Stop(3, line)) => assert_eq!(line, "halt"),
        _ => panic!("not stopped"),
    }
    let o = put_cmd(&f, &b, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    no_leak(&f, &o);
    let text = read(&entry_dir(&f, "hand-1").join("label.json"));
    assert!(
        text.contains("\"state\":\"moved\"") && text.contains("\"judgeable\":\"no\"") && text.contains("\"reason\":\"changed-during-put\""),
        "label: {text}"
    );
}

#[test]
fn a_stale_label_tmp_does_not_block_the_settle() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    stop_at(&f, &a, Point::AfterIntent);
    let stale = entry_dir(&f, "hand-1").join(format!("label.json.{}-1.tmp", std::process::id()));
    fs::write(&stale, b"stale").unwrap();
    match put::put(&pargs(&f, &b)) {
        Ok(line) => assert!(line.contains("entry=20260101/hand-2"), "line: {line}"),
        Err(Stop(c, line)) => panic!("exit {c}: {line}"),
    }
    assert!(read(&entry_dir(&f, "hand-1").join("label.json")).contains("\"state\":\"void\""));
    assert_eq!(fs::read(&stale).unwrap(), b"stale");
}

#[test]
fn a_torn_journal_tail_is_skipped_by_the_reader() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    stop_at(&f, &a, Point::AfterIntent);
    let mut fh = fs::OpenOptions::new().append(true).open(journal1(&f)).unwrap();
    fh.write_all(b"{\"v\":1,\"op\":\"vo").unwrap();
    drop(fh);
    let o = put_cmd(&f, &b, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    no_leak(&f, &o);
    assert!(read(&entry_dir(&f, "hand-1").join("label.json")).contains("\"state\":\"void\""));
}

#[test]
fn a_bad_complete_journal_line_exits_3() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    stop_at(&f, &a, Point::AfterIntent);
    let mut fh = fs::OpenOptions::new().append(true).open(journal1(&f)).unwrap();
    fh.write_all(b"{\"v\":1,\"op\":\"void\",\"entry\":\"20260101/hand-1\"}\n").unwrap();
    drop(fh);
    let (j_before, b_before) = (fs::read(journal1(&f)).unwrap(), snap(&b));
    let o = put_cmd(&f, &b, &[]);
    refused(&f, &o, 3);
    assert!(err(&o).contains("20260101") && err(&o).contains("line 2"), "stderr: {}", err(&o));
    assert_eq!(fs::read(journal1(&f)).unwrap(), j_before);
    assert_eq!(snap(&b), b_before);
    assert!(read(&entry_dir(&f, "hand-1").join("label.json")).contains("\"state\":\"intent\""));
}

#[test]
fn a_journal_file_with_another_name_exits_3() {
    let f = fixture();
    let (a, b) = (plain_item(&f, "a"), plain_item(&f, "b"));
    stop_at(&f, &a, Point::AfterIntent);
    fs::write(f.q.join("@shelf/journal/notes.txt"), b"n").unwrap();
    let b_before = snap(&b);
    let o = put_cmd(&f, &b, &[]);
    refused(&f, &o, 3);
    assert!(err(&o).contains("nothing was moved"), "stderr: {}", err(&o));
    assert_eq!(snap(&b), b_before);
}
