//! Process-level tests for the human-origin check behind `harness config write`. Every run is the real debug binary as a child
//! process. Each test generates a throwaway signer at run time with ssh-keygen; no key material is in this file. This file never reads env.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const KEYGEN: &str = "/usr/bin/ssh-keygen";
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

fn scratch() -> Tmp {
    assert!(Path::new(KEYGEN).exists(), "{KEYGEN} is required by these tests (it is the verifier); install OpenSSH");
    let p = std::env::temp_dir().join(format!("hho-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    Tmp(p.canonicalize().unwrap())
}

fn keygen(dir: &Path, name: &str) -> PathBuf {
    let k = dir.join(name);
    let st = Command::new(KEYGEN).args(["-q", "-t", "ed25519", "-N", "", "-C", "t", "-f"]).arg(&k).status().unwrap();
    assert!(st.success(), "ssh-keygen could not make a throwaway key");
    k
}

/// An anchor trusting `key`, in a 0700 folder; the file is 0644.
fn anchor_for(t: &Tmp, key: &Path) -> PathBuf {
    let d = t.0.join("anchor");
    fs::create_dir_all(&d).unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
    let pubkey = fs::read_to_string(format!("{}.pub", key.display())).unwrap();
    let a = d.join("allowed");
    fs::write(&a, format!("bridlework-human namespaces=\"bridlework-human-origin\" {}", pubkey)).unwrap();
    fs::set_permissions(&a, fs::Permissions::from_mode(0o644)).unwrap();
    a
}

struct Fx {
    t: Tmp,
    cfg: PathBuf,
    old: Vec<u8>,
    new: PathBuf,
    log: PathBuf,
    key: PathBuf,
    anchor: PathBuf,
}

fn setup() -> Fx {
    let t = scratch();
    let old = fs::read(EXAMPLE).unwrap();
    let cfg = t.0.join("harness.yaml");
    fs::write(&cfg, &old).unwrap();
    let mut nb = old.clone();
    nb.extend_from_slice(b"# changed by the human\n");
    let new = t.0.join("new.yaml");
    fs::write(&new, nb).unwrap();
    let key = keygen(&t.0, "k");
    let anchor = anchor_for(&t, &key);
    let log = t.0.join("events.jsonl");
    Fx { t, cfg, old, new, log, key, anchor }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn msg(fx: &Fx, from: &Path, exp: u64) -> Vec<u8> {
    let o = Command::new(BIN)
        .args(["config", "approval-message", "--config", s(&fx.cfg), "--from", s(from), "--expires-ms", &exp.to_string()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    o.stdout
}

/// Sign `message` with `key`; returns the signature file.
fn sign(fx: &Fx, key: &Path, message: &[u8], tag: &str) -> PathBuf {
    let m = fx.t.0.join(format!("msg-{tag}"));
    fs::write(&m, message).unwrap();
    let st = Command::new(KEYGEN).args(["-q", "-Y", "sign", "-n", "bridlework-human-origin", "-f"]).arg(key).arg(&m).status().unwrap();
    assert!(st.success());
    PathBuf::from(format!("{}.sig", m.display()))
}

fn write(fx: &Fx, from: &Path, sig: Option<&Path>, exp: Option<u64>, anchor: &Path) -> Output {
    let mut c = Command::new(BIN);
    c.args(["config", "write", "--config", s(&fx.cfg), "--from", s(from), "--log", s(&fx.log), "--trust-anchor", s(anchor)]);
    if let Some(p) = sig {
        c.args(["--approval", s(p)]);
    }
    if let Some(e) = exp {
        c.args(["--expires-ms", &e.to_string()]);
    }
    c.stdin(Stdio::null()).output().unwrap()
}

fn log_text(fx: &Fx) -> String {
    fs::read_to_string(&fx.log).unwrap_or_default()
}

fn assert_refused(fx: &Fx, o: &Output) {
    assert_eq!(o.status.code(), Some(3), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("human_origin_refused"));
    assert_eq!(fs::read(&fx.cfg).unwrap(), fx.old, "the config must be unchanged");
    let l = log_text(fx);
    assert_eq!(l.lines().count(), 1);
    assert!(l.contains("\"kind\":\"human_origin_refused\"") && l.contains("\"target\":\"config\"") && l.contains("\"path\":\"harness.yaml\""), "log: {l}");
}

/// A signed, valid approval for the fixture's new file, expiring in 10 minutes.
fn good_approval(fx: &Fx) -> (PathBuf, u64) {
    let exp = now_ms() + 600_000;
    let sig = sign(fx, &fx.key, &msg(fx, &fx.new, exp), "good");
    (sig, exp)
}

#[test]
fn agent_write_without_approval_is_refused() {
    let fx = setup();
    let o = write(&fx, &fx.new, None, None, &fx.anchor);
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=no_approval"));
}

#[test]
fn human_signed_write_is_accepted() {
    let fx = setup();
    let (sig, exp) = good_approval(&fx);
    let o = write(&fx, &fx.new, Some(&sig), Some(exp), &fx.anchor);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read(&fx.cfg).unwrap(), fs::read(&fx.new).unwrap());
    assert!(!log_text(&fx).contains("human_origin_refused"));
    let l = log_text(&fx);
    assert_eq!(l.lines().count(), 1);
    assert!(l.contains("\"kind\":\"human_origin_accepted\"") && l.contains("\"target\":\"config\""), "log: {l}");
}

#[test]
fn signature_by_untrusted_key_is_refused() {
    let fx = setup();
    let other = keygen(&fx.t.0, "other");
    let exp = now_ms() + 600_000;
    let sig = sign(&fx, &other, &msg(&fx, &fx.new, exp), "other");
    let o = write(&fx, &fx.new, Some(&sig), Some(exp), &fx.anchor);
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=signature_invalid"));
}

#[test]
fn approval_for_other_content_is_refused() {
    let fx = setup();
    let (sig, exp) = good_approval(&fx);
    let b = fx.t.0.join("b.yaml");
    let mut bb = fx.old.clone();
    bb.extend_from_slice(b"# something else\n");
    fs::write(&b, bb).unwrap();
    let o = write(&fx, &b, Some(&sig), Some(exp), &fx.anchor);
    assert_refused(&fx, &o);
}

#[test]
fn expired_approval_is_refused() {
    let fx = setup();
    let exp = now_ms() - 60_000;
    let sig = sign(&fx, &fx.key, &msg(&fx, &fx.new, exp), "old");
    let o = write(&fx, &fx.new, Some(&sig), Some(exp), &fx.anchor);
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=expired"));
}

#[test]
fn overlong_window_is_refused() {
    let fx = setup();
    let exp = now_ms() + 3_600_000 + 120_000;
    let sig = sign(&fx, &fx.key, &msg(&fx, &fx.new, exp), "long");
    let o = write(&fx, &fx.new, Some(&sig), Some(exp), &fx.anchor);
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=window_too_long"));
}

#[test]
fn group_writable_anchor_is_refused() {
    let fx = setup();
    let (sig, exp) = good_approval(&fx);
    fs::set_permissions(&fx.anchor, fs::Permissions::from_mode(0o664)).unwrap();
    let o = write(&fx, &fx.new, Some(&sig), Some(exp), &fx.anchor);
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=anchor_untrusted"));
}

#[test]
fn symlinked_anchor_is_refused() {
    let fx = setup();
    let (sig, exp) = good_approval(&fx);
    let link = fx.anchor.parent().unwrap().join("link");
    std::os::unix::fs::symlink(&fx.anchor, &link).unwrap();
    let o = write(&fx, &fx.new, Some(&sig), Some(exp), &link);
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=anchor_untrusted"));
}

#[test]
fn invalid_new_content_is_exit_2_and_writes_nothing() {
    let fx = setup();
    let bad = fx.t.0.join("bad.yaml");
    fs::write(&bad, "not: a config\n").unwrap();
    let o = write(&fx, &bad, None, None, &fx.anchor);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(fs::read(&fx.cfg).unwrap(), fx.old);
    assert!(!fx.log.exists());
}
