//! Shared helpers of the ticket-3601 process-level friction tests (`friction_evidence`, `friction_rank`,
//! `friction_triage`): a scratch folder, the real binary as a child with no config or list variable inherited,
//! and a row-line builder. Every value is synthetic; the deny list is injected on the child only.
#![allow(dead_code)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
const PLACEHOLDER: &str = "/placeholder/friction-inbox.jsonl";
/// A synthetic deny word: the ONLY entry of the injected list.
pub const DENY: &str = "zzdenyzz";
static SEQ: AtomicUsize = AtomicUsize::new(0);

pub struct Tmp(pub PathBuf);

impl Tmp {
    pub fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("f3601-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    pub fn inbox(&self) -> PathBuf {
        self.0.join("inbox.jsonl")
    }

    pub fn state(&self) -> PathBuf {
        self.0.join("state.jsonl")
    }

    pub fn handoff(&self) -> PathBuf {
        let d = self.0.join("handoff");
        fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let f = self.0.join(name);
        fs::write(&f, body).unwrap();
        f
    }

    /// The scratch config: the public example with its placeholder inbox swapped for this scratch inbox
    /// (and, when given, a `triage_top` line added under `friction`).
    pub fn config_with(&self, extra: &str) -> PathBuf {
        let text = fs::read_to_string(EXAMPLE).unwrap();
        let mut text = text.replace(PLACEHOLDER, self.inbox().to_str().unwrap());
        if !extra.is_empty() {
            text = text.replace("  pattern_threshold: 3\n", &format!("  pattern_threshold: 3\n{extra}\n"));
        }
        self.file("config.yaml", text)
    }

    pub fn config(&self) -> PathBuf {
        self.config_with("")
    }

    pub fn write_inbox(&self, rows: &[String]) {
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        fs::write(self.inbox(), body).unwrap();
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn base() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET");
    c
}

pub fn run(args: &[&str]) -> Output {
    let mut c = base();
    c.args(args).env("HARNESS_PRIVACY_LIST_SECRET", DENY).stdin(Stdio::null());
    c.output().unwrap()
}

pub fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

pub fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

/// A canonical row line: the six keys, then any extra `(key, value)` pairs in the order given.
pub fn row(id: &str, date: &str, breaks: &str, text: &str, extra: &[(&str, &str)]) -> String {
    let mut l = format!("{{\"id\":\"{id}\",\"date\":\"{date}\",\"ticket\":\"t-1\",\"where-it-lives\":\"memory\",\"breaks\":\"{breaks}\",\"text\":\"{text}\"");
    for (k, v) in extra {
        l.push_str(&format!(",\"{k}\":\"{v}\""));
    }
    l.push('}');
    l
}

/// A hand row with an impact.
pub fn hand(id: &str, date: &str, breaks: &str, impact: &str) -> String {
    if impact.is_empty() {
        row(id, date, breaks, "lesson", &[])
    } else {
        row(id, date, breaks, "lesson", &[("impact", impact)])
    }
}

/// An event row of one signature in one week (the signature is the id prefix before `.<yyyy>-w<ww>`).
pub fn event(sig: &str, week: &str, date: &str, impact: &str) -> String {
    let mut extra: Vec<(&str, &str)> = vec![("source", "event")];
    if !impact.is_empty() {
        extra.push(("impact", impact));
    }
    row(&format!("{sig}.{week}"), date, "", "event row", &extra).replace("\"ticket\":\"t-1\"", "\"ticket\":\"t-9.3\"")
}

pub fn pattern(id: &str, date: &str, breaks: &str) -> String {
    row(id, date, breaks, "procedure", &[("kind", "pattern")])
}

pub fn add(t: &Tmp, kv: &[(&str, &str)]) -> Output {
    let cfg = t.config();
    let mut args: Vec<&str> = vec!["friction", "add"];
    for (k, v) in kv {
        args.push(k);
        args.push(v);
    }
    args.push("--config");
    args.push(p(&cfg));
    run(&args)
}
