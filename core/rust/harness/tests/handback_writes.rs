//! Process-level tests for the human-origin check behind `harness controls write` and `harness permission write` (#3708). Every run is the real debug binary as a child
//! process. Each test generates a throwaway signer at run time with ssh-keygen; no key material is in this file. This file never reads env.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const KEYGEN: &str = "/usr/bin/ssh-keygen";
const REGISTRY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../controls/registry.json");
const SETTINGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../plugins/claude/managed-settings.template.json");
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


#[derive(Clone, Copy, PartialEq)]
enum K {
    Reg,
    Perm,
}

impl K {
    fn verb(self) -> &'static str {
        if self == K::Reg { "controls" } else { "permission" }
    }
    fn flag(self) -> &'static str {
        if self == K::Reg { "--registry" } else { "--settings" }
    }
    fn target(self) -> &'static str {
        if self == K::Reg { "control_registry" } else { "permission" }
    }
    fn file(self) -> &'static str {
        if self == K::Reg { "registry.json" } else { "managed-settings.json" }
    }
}

struct Fx {
    t: Tmp,
    k: K,
    file: PathBuf,
    old: Vec<u8>,
    new: PathBuf,
    log: PathBuf,
    key: PathBuf,
    anchor: PathBuf,
}

fn setup(k: K) -> Fx {
    let t = scratch();
    let old = fs::read(if k == K::Reg { REGISTRY } else { SETTINGS }).unwrap();
    let file = t.0.join(k.file());
    fs::write(&file, &old).unwrap();
    let text = String::from_utf8(old.clone()).unwrap();
    let nb = if k == K::Reg {
        assert!(text.contains("\"enabled\": true") || text.contains("\"enabled\":true"));
        if text.contains("\"enabled\": true") { text.replacen("\"enabled\": true", "\"enabled\": false", 1) } else { text.replacen("\"enabled\":true", "\"enabled\":false", 1) }
    } else {
        text.replacen("\"deny\": [", "\"deny\": [\n      \"Bash(extra-deny:*)\",", 1)
    };
    assert_ne!(nb, text, "the fixture edit must change the bytes");
    let new = t.0.join("new.json");
    fs::write(&new, nb).unwrap();
    let key = keygen(&t.0, "k");
    let anchor = anchor_for(&t, &key);
    let log = t.0.join("events.jsonl");
    Fx { t, k, file, old, new, log, key, anchor }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn msg_for(fx: &Fx, verb: &str, flag: &str, from: &Path, exp: u64) -> Vec<u8> {
    let o = Command::new(BIN)
        .args([verb, "approval-message", flag, s(&fx.file), "--from", s(from), "--expires-ms", &exp.to_string()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

fn msg(fx: &Fx, from: &Path, exp: u64) -> Vec<u8> {
    msg_for(fx, fx.k.verb(), fx.k.flag(), from, exp)
}

fn sign(fx: &Fx, key: &Path, message: &[u8], tag: &str) -> PathBuf {
    let m = fx.t.0.join(format!("msg-{tag}"));
    fs::write(&m, message).unwrap();
    let st = Command::new(KEYGEN).args(["-q", "-Y", "sign", "-n", "bridlework-human-origin", "-f"]).arg(key).arg(&m).status().unwrap();
    assert!(st.success());
    PathBuf::from(format!("{}.sig", m.display()))
}

fn write_as(fx: &Fx, k: K, from: &Path, sig: Option<&Path>, exp: Option<u64>) -> Output {
    let mut c = Command::new(BIN);
    c.args([k.verb(), "write", k.flag(), s(&fx.file), "--from", s(from), "--log", s(&fx.log), "--trust-anchor", s(&fx.anchor)]);
    if let Some(p) = sig {
        c.args(["--approval", s(p)]);
    }
    if let Some(e) = exp {
        c.args(["--expires-ms", &e.to_string()]);
    }
    c.stdin(Stdio::null()).output().unwrap()
}

fn write(fx: &Fx, from: &Path, sig: Option<&Path>, exp: Option<u64>) -> Output {
    write_as(fx, fx.k, from, sig, exp)
}

fn log_text(fx: &Fx) -> String {
    fs::read_to_string(&fx.log).unwrap_or_default()
}

fn assert_refused(fx: &Fx, o: &Output) {
    assert_eq!(o.status.code(), Some(3), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains(&format!("human_origin_refused target={}", fx.k.target())));
    assert_eq!(fs::read(&fx.file).unwrap(), fx.old, "the file must be unchanged");
    let l = log_text(fx);
    assert_eq!(l.lines().count(), 1, "log: {l}");
    assert!(
        l.contains("\"kind\":\"human_origin_refused\"") && l.contains(&format!("\"target\":\"{}\"", fx.k.target())) && l.contains(&format!("\"path\":\"{}\"", fx.k.file())),
        "log: {l}"
    );
}

fn good_approval(fx: &Fx) -> (PathBuf, u64) {
    let exp = now_ms() + 600_000;
    let sig = sign(fx, &fx.key, &msg(fx, &fx.new, exp), "good");
    (sig, exp)
}

fn agent_refused(k: K) {
    let fx = setup(k);
    let o = write(&fx, &fx.new, None, None);
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=no_approval"));
}

fn human_accepted(k: K) {
    let fx = setup(k);
    let (sig, exp) = good_approval(&fx);
    let o = write(&fx, &fx.new, Some(&sig), Some(exp));
    assert_eq!(o.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read(&fx.file).unwrap(), fs::read(&fx.new).unwrap());
    let l = log_text(&fx);
    assert_eq!(l.matches("\"kind\":\"human_origin_accepted\"").count(), 1, "log: {l}");
    assert_eq!(l.lines().count(), 1, "log: {l}");
    assert!(l.contains(&format!("\"target\":\"{}\"", k.target())) && l.contains(&format!("\"path\":\"{}\"", k.file())), "log: {l}");
    assert!(!l.contains("human_origin_refused"));
}

#[test]
fn agent_registry_row_write_is_refused() {
    agent_refused(K::Reg);
}

#[test]
fn human_registry_row_write_is_accepted_and_logged() {
    human_accepted(K::Reg);
}

#[test]
fn agent_permission_edit_is_refused() {
    agent_refused(K::Perm);
}

#[test]
fn human_permission_edit_is_accepted_and_logged() {
    human_accepted(K::Perm);
}

#[test]
fn config_approval_replayed_on_registry_is_refused() {
    let fx = setup(K::Reg);
    let exp = now_ms() + 600_000;
    // The message a `config approval-message` would build: same name and bytes, target=config.
    let m = String::from_utf8(msg(&fx, &fx.new, exp)).unwrap().replacen("target=control_registry", "target=config", 1);
    let sig = sign(&fx, &fx.key, m.as_bytes(), "cfg");
    let o = write(&fx, &fx.new, Some(&sig), Some(exp));
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=signature_invalid"));
}

#[test]
fn registry_approval_replayed_on_permission_is_refused() {
    let fx = setup(K::Perm);
    let exp = now_ms() + 600_000;
    let m = String::from_utf8(msg(&fx, &fx.new, exp)).unwrap().replacen("target=permission", "target=control_registry", 1);
    let sig = sign(&fx, &fx.key, m.as_bytes(), "reg");
    let o = write(&fx, &fx.new, Some(&sig), Some(exp));
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=signature_invalid"));
}

#[test]
fn permission_approval_for_other_content_is_refused() {
    let fx = setup(K::Perm);
    let (sig, exp) = good_approval(&fx);
    let b = fx.t.0.join("b.json");
    fs::write(&b, String::from_utf8(fs::read(&fx.new).unwrap()).unwrap().replacen("extra-deny", "other-deny", 1)).unwrap();
    let o = write(&fx, &b, Some(&sig), Some(exp));
    assert_refused(&fx, &o);
}

#[test]
fn untrusted_key_registry_write_is_refused() {
    let fx = setup(K::Reg);
    let other = keygen(&fx.t.0, "other");
    let exp = now_ms() + 600_000;
    let sig = sign(&fx, &other, &msg(&fx, &fx.new, exp), "other");
    let o = write(&fx, &fx.new, Some(&sig), Some(exp));
    assert_refused(&fx, &o);
    assert!(String::from_utf8_lossy(&o.stderr).contains("reason=signature_invalid"));
}

#[test]
fn invalid_registry_content_is_exit_2_and_writes_nothing() {
    let fx = setup(K::Reg);
    let text = String::from_utf8(fx.old.clone()).unwrap();
    // Duplicate the first row's id by making the second row's id equal to the first's.
    let ids: Vec<&str> = text.lines().filter(|l| l.trim_start().starts_with("\"id\"")).collect();
    assert!(ids.len() >= 2, "the committed registry has at least two rows");
    let bad = text.replacen(ids[1], ids[0], 1);
    assert_ne!(bad, text);
    let bp = fx.t.0.join("bad.json");
    fs::write(&bp, bad).unwrap();
    let o = write(&fx, &bp, None, None);
    assert_eq!(o.status.code(), Some(2), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read(&fx.file).unwrap(), fx.old);
    assert!(!fx.log.exists());
}

#[test]
fn invalid_permission_content_is_exit_2_and_writes_nothing() {
    let fx = setup(K::Perm);
    let bp = fx.t.0.join("bad.json");
    fs::write(&bp, r#"{"permissions":{"deny":["a"]},"extra":1}"#).unwrap();
    let o = write(&fx, &bp, None, None);
    assert_eq!(o.status.code(), Some(2), "stderr: {}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(fs::read(&fx.file).unwrap(), fx.old);
    assert!(!fx.log.exists());
}
