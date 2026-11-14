//! The ONE guarded write for the files that decide what agents may do (std only, unix only): the config, the permission set and
//! the control registry. Every target goes through `guarded_write`, which holds the only call of `human_origin::check` in `src/`,
//! so there is never a second gate. `run_verb` is the shared command-line shell for the three write verbs.
//!
//! Order (fail closed): check; on refusal append `human_origin_refused` and touch nothing; on success write and sync a temp file,
//! append `human_origin_accepted`, then rename over the target. No write lands without its accepted line. If the rename itself
//! fails the log holds an accepted line with no landed write: the accepted line records that the approval was accepted, and the
//! stdout `written` line confirms the write landed (`docs/human-origin.md`).

use crate::events::{self, Field, Kind, Record};
use crate::human_origin::{self, Anchor, Refusal, Target};
use crate::supervisor::LogRoot;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};

/// The largest new file a write verb reads.
const MAX_BYTES: u64 = 1024 * 1024;

pub struct Write<'a> {
    pub target: Target,
    pub path: &'a Path,
    pub name: &'a str,
    pub bytes: &'a [u8],
    pub approval: Option<&'a Path>,
    pub expires_ms: Option<u64>,
}

pub enum Outcome {
    Written,
    Refused(Refusal),
}

fn append(lr: &LogRoot, kind: Kind, now_ms: u64, target: Target, name: &str) -> bool {
    crate::supervisor::ensure_default_log_folder(lr).is_ok()
        && Record::build(kind, now_ms, &[("target", Field::Str(target.as_str().to_string())), ("path", Field::Str(name.to_string()))])
            .ok()
            .map(|rec| events::append_record(&lr.root, &lr.log, &rec).is_ok())
            .unwrap_or(false)
}

pub(crate) fn guarded_write(w: &Write, now_ms: u64, anchor: &Anchor, lr: &LogRoot) -> Result<Outcome, String> {
    if let Err(r) = human_origin::check(w.target, w.name, w.bytes, w.approval, w.expires_ms, now_ms, anchor) {
        if !append(lr, Kind::HumanOriginRefused, now_ms, w.target, w.name) {
            return Err("the refusal could not be logged".to_string());
        }
        return Ok(Outcome::Refused(r));
    }
    let dir = match w.path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let tmp = dir.join(format!(".{}.harness-tmp-{}", w.name, std::process::id()));
    let staged = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(w.bytes)?;
        if let Ok(md) = std::fs::metadata(w.path) {
            f.set_permissions(md.permissions())?;
        }
        f.sync_all()
    })();
    if staged.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("the file could not be written".to_string());
    }
    if !append(lr, Kind::HumanOriginAccepted, now_ms, w.target, w.name) {
        let _ = std::fs::remove_file(&tmp);
        return Err("the accepted write could not be logged".to_string());
    }
    if std::fs::rename(&tmp, w.path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err("the file could not be written".to_string());
    }
    Ok(Outcome::Written)
}

/// One write verb's shape.
pub struct Spec {
    pub target: Target,
    /// The flag naming the target file (`config`, `registry`, `settings`).
    pub flag: &'static str,
    /// The word in `<noun> written name=<n>` and in error text.
    pub noun: &'static str,
    /// Config only: an env var that supplies the file when the flag is absent.
    pub env: Option<&'static str>,
    /// Content validation; runs before the check.
    pub validate: fn(&str) -> Result<(), String>,
}

/// `<verb> approval-message` and `<verb> write`. Exit 0 done; 2 usage/bad content/log failure; 3 refused (file untouched).
pub fn run_verb(spec: &Spec, sub: &str, a: &[String]) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let write_flags: Vec<&str> = {
        #[allow(unused_mut)]
        let mut v = vec![spec.flag, "from", "approval", "expires-ms", "log", "root"];
        #[cfg(debug_assertions)]
        v.push("trust-anchor");
        v
    };
    let msg_flags = [spec.flag, "from", "expires-ms"];
    let o = match crate::cli::take_flags(a, if sub == "write" { &write_flags } else { &msg_flags }) {
        Ok(o) => o,
        Err(e) => return fail(&e),
    };
    let path = match o.get(spec.flag) {
        Some(f) => std::ffi::OsString::from(f),
        None => match spec.env.and_then(std::env::var_os) {
            Some(v) => v,
            None => {
                return match spec.env {
                    Some(e) => fail(&format!("no {} configured (pass --{} <file> or set {})", spec.noun, spec.flag, e)),
                    None => fail(&format!("--{} <file> is required", spec.flag)),
                }
            }
        },
    };
    if path.is_empty() {
        return fail(&format!("the {} source is empty", spec.noun));
    }
    let path = PathBuf::from(path);
    let Some(from) = o.get("from") else { return fail("--from <new-file> is required") };
    let expires = match o.get("expires-ms").map(|s| s.parse::<u64>()) {
        Some(Ok(n)) => Some(n),
        Some(Err(_)) => return fail("--expires-ms must be a number"),
        None => None,
    };
    let mut bytes = Vec::new();
    match std::fs::File::open(from) {
        Ok(f) => {
            if f.take(MAX_BYTES + 1).read_to_end(&mut bytes).is_err() {
                return fail("the new file is unreadable");
            }
        }
        Err(_) => return fail("the new file is unreadable"),
    }
    if bytes.len() as u64 > MAX_BYTES {
        return fail("the new file is too large");
    }
    match std::str::from_utf8(&bytes).map_err(|_| format!("{}: file is not utf-8", spec.noun)).and_then(spec.validate) {
        Ok(()) => {}
        Err(e) => return fail(&e),
    }
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) if events::token_ok(n) => n.to_string(),
        _ => return fail("the target file name must be a plain token (lowercase letters, digits, '-', '_', '.')"),
    };
    if sub == "approval-message" {
        let Some(exp) = expires else { return fail("--expires-ms is required") };
        let m = human_origin::message(spec.target, &name, exp, &bytes);
        return match std::io::stdout().write_all(&m).and_then(|_| std::io::stdout().flush()) {
            Ok(()) => 0,
            Err(_) => fail("stdout is not writable"),
        };
    }
    #[cfg(debug_assertions)]
    let anchor = match o.get("trust-anchor") {
        Some(p) => Anchor::for_test(Path::new(p)),
        None => Anchor::system(),
    };
    #[cfg(not(debug_assertions))]
    let anchor = Anchor::system();
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(_) => return fail("clock is before the epoch"),
    };
    let lr = crate::supervisor::resolve_log_root(o.get("log").map(|s| s.as_str()), o.get("root").map(|s| s.as_str()));
    let w = Write { target: spec.target, path: &path, name: &name, bytes: &bytes, approval: o.get("approval").map(Path::new), expires_ms: expires };
    match guarded_write(&w, now, &anchor, &lr) {
        Ok(Outcome::Written) => {
            println!("{} written name={}", spec.noun, name);
            0
        }
        Ok(Outcome::Refused(r)) => {
            eprintln!("harness: human_origin_refused target={} reason={}", spec.target.as_str(), r.as_str());
            3
        }
        Err(e) => fail(&e),
    }
}

/// `harness controls ...` and `harness permission ...` (the registry and the permission set).
pub fn controls_cmd(a: &[String]) -> i32 {
    verb("controls", a)
}

pub fn permission_cmd(a: &[String]) -> i32 {
    verb("permission", a)
}

const CONTROLS_USAGE: &str = "usage: harness controls approval-message --registry <file> --from <new-file> --expires-ms <n>\n       harness controls write --registry <file> --from <new-file> [--approval <sig> --expires-ms <n>] [--log <file>] [--root <dir>]";
const PERMISSION_USAGE: &str = "usage: harness permission approval-message --settings <file> --from <new-file> --expires-ms <n>\n       harness permission write --settings <file> --from <new-file> [--approval <sig> --expires-ms <n>] [--log <file>] [--root <dir>]";

fn verb(which: &str, a: &[String]) -> i32 {
    let (usage, spec) = if which == "controls" {
        (CONTROLS_USAGE, Spec { target: Target::ControlRegistry, flag: "registry", noun: "controls", env: None, validate: |t| crate::control::Registry::parse(t).map(|_| ()) })
    } else {
        (PERMISSION_USAGE, Spec { target: Target::Permission, flag: "settings", noun: "permission", env: None, validate: crate::permission::parse })
    };
    match a.first().map(|s| s.as_str()) {
        Some(sub @ ("write" | "approval-message")) => run_verb(&spec, sub, &a[1..]),
        _ => {
            eprintln!("{}", usage);
            2
        }
    }
}
