//! `harness session done-check` and `harness session close` (P1B-17; S-1.6 and S-1.6.1; std only, Unix). Both read the host's session
//! listing (`<host> agents --json --all`), work out each session's ticket from its NAME with the P1B-16 predicate and the config's lane
//! count, and compare it with the done set. A name the predicate refuses, or one with no ticket, is never matched and never touched.
//! A hyphenated read-back (only the legacy long form makes one) that equals a done slug once its `-` are removed is UNRESOLVED: it is
//! never matched, never touched, and makes `done-check` exit 3 (it cannot tell whether that session belongs to the done ticket).
//! `done-check` is read-only. `close` runs `stop` (unless the entry's state is `done`) then `rm` on each matching session.
//! Host verbs are exercised against stub hosts only until the S-1.6 host re-probe. See docs/session-close.md.

use crate::config::Config;
use crate::launcher::{parse_json, run_host, HostOut, J};
use crate::session_name;
use std::ffi::OsStr;
use std::path::Path;

const AUX_TIMEOUT_MS: u64 = 10000;
/// At most this many listed entries are read; more is `cannot-check`.
const MAX_ENTRIES: usize = 10000;
/// At most this many `--done` tickets.
const MAX_DONE: usize = 64;
const MAX_ARG: usize = 256;

pub const USAGE: &str = "usage: harness session done-check --host <abs> --config <file> --done <ticket-id> [--done <ticket-id> ...] | session close --host <abs> --config <file> --ticket <ticket-id>";

/// The S-1.6 slug of a ticket id: lower-cased, every char outside `[a-z0-9]` dropped. Empty is refused.
pub fn slug(ticket_id: &str) -> Option<String> {
    let s: String = ticket_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (!s.is_empty()).then_some(s)
}

#[derive(Debug)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub state: Option<String>,
}

fn id_ok(tok: &str) -> bool {
    !tok.is_empty()
        && !tok.starts_with('-')
        && tok.len() <= 64
        && tok
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// One `agents --json --all` run. Any failure is `Err` (cannot-check).
pub fn listing(host: &Path) -> Result<Vec<Entry>, String> {
    let o = match run_host(
        host,
        &["agents".as_ref(), "--json".as_ref(), "--all".as_ref()],
        None,
        AUX_TIMEOUT_MS,
    ) {
        Ok(o) if o.ok => o,
        Ok(_) => return Err("the host's listing exited non-zero".into()),
        Err(_) => return Err("the host's listing could not be run or timed out".into()),
    };
    let J::Arr(items) =
        parse_json(&o.stdout).map_err(|_| "the host's listing is not JSON".to_string())?
    else {
        return Err("the host's listing is not an array".into());
    };
    if items.len() > MAX_ENTRIES {
        return Err("the host's listing is over the entry cap".into());
    }
    let mut v = Vec::new();
    for e in &items {
        let (Some(id), Some(name)) = (e.str("id"), e.str("name")) else {
            return Err("a listing entry has no string id and name".into());
        };
        if !id_ok(id) {
            return Err("a listing entry has an id that is not a plain token".into());
        }
        v.push(Entry {
            id: id.to_string(),
            name: name.to_string(),
            state: e.str("state").map(|s| s.to_string()),
        });
    }
    Ok(v)
}

#[derive(PartialEq, Debug)]
enum Class {
    Open,
    Unresolved(String),
    Other,
}

fn classify(e: &Entry, lanes: u64, done: &[String]) -> Class {
    let Some(t) = session_name::check(&e.name, lanes, false)
        .ok()
        .and_then(|v| v.ticket)
    else {
        return Class::Other;
    };
    if done.contains(&t) {
        return Class::Open;
    }
    if t.contains('-') {
        let flat: String = t.chars().filter(|c| *c != '-').collect();
        if done.contains(&flat) {
            return Class::Unresolved(t);
        }
    }
    Class::Other
}

struct Opts {
    host: String,
    config: Option<String>,
    values: Vec<String>,
}

fn parse(a: &[String], flag: &str, many: bool) -> Result<Opts, String> {
    let (mut host, mut config, mut values) = (None, None, Vec::new());
    let mut it = a.iter();
    while let Some(f) = it.next() {
        let name = f.as_str();
        if !matches!(name, "--host" | "--config") && name != flag {
            return Err(format!(
                "unknown option {}",
                name.chars().take(40).collect::<String>()
            ));
        }
        let v = it.next().ok_or_else(|| format!("{} needs a value", name))?;
        if v.len() > MAX_ARG {
            return Err(format!("{} value is too long", name));
        }
        match name {
            "--host" => {
                if host.replace(v.clone()).is_some() {
                    return Err("--host given twice".into());
                }
            }
            "--config" => {
                if config.replace(v.clone()).is_some() {
                    return Err("--config given twice".into());
                }
            }
            _ => {
                if !many && !values.is_empty() {
                    return Err(format!("{} given twice", flag));
                }
                if values.len() >= MAX_DONE {
                    return Err(format!("more than {} {} values", MAX_DONE, flag));
                }
                values.push(v.clone());
            }
        }
    }
    if values.is_empty() {
        return Err(format!("{} is required", flag));
    }
    Ok(Opts {
        host: host.ok_or("--host is required")?,
        config,
        values,
    })
}

fn setup(o: &Opts) -> Result<(std::path::PathBuf, u64, Vec<String>), String> {
    let host = std::path::PathBuf::from(&o.host);
    if !host.is_absolute() {
        return Err("--host must be an absolute path (no PATH lookup)".into());
    }
    if !std::fs::metadata(&host)
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return Err("--host is not an existing regular file".into());
    }
    let mut slugs = Vec::new();
    for v in &o.values {
        slugs.push(slug(v).ok_or("a ticket id has an empty slug")?);
    }
    let path = match &o.config {
        Some(f) => std::ffi::OsString::from(f),
        None => std::env::var_os("HARNESS_CONFIG")
            .ok_or("no config configured (pass --config <file> or set HARNESS_CONFIG)")?,
    };
    if path.is_empty() {
        return Err("the config source is empty".into());
    }
    let cfg = Config::load(Path::new(&path))?;
    Ok((host, cfg.lane_count(), slugs))
}

fn usage(m: &str) -> i32 {
    eprintln!("harness: error: {}", m);
    eprintln!("{}", USAGE);
    2
}

/// `harness session done-check ...`: 0 pass, 1 open sessions listed, 3 cannot check (listing unreadable, or unresolved entries), 2 usage.
pub fn done_check_cmd(a: &[String]) -> i32 {
    let o = match parse(a, "--done", true) {
        Ok(o) => o,
        Err(m) => return usage(&m),
    };
    let (host, lanes, done) = match setup(&o) {
        Ok(x) => x,
        Err(m) => return usage(&m),
    };
    let entries = match listing(&host) {
        Ok(e) => e,
        Err(m) => {
            eprintln!("harness: done-check: {}", m);
            return 3;
        }
    };
    let (mut open, mut unresolved) = (Vec::new(), Vec::new());
    for e in &entries {
        match classify(e, lanes, &done) {
            Class::Open => open.push(format!(
                "session=open name={} id={} ticket={}",
                e.name,
                e.id,
                session_name::check(&e.name, lanes, false)
                    .ok()
                    .and_then(|v| v.ticket)
                    .unwrap_or_default()
            )),
            Class::Unresolved(t) => unresolved.push(format!(
                "session=unresolved name={} id={} ticket={}",
                e.name, e.id, t
            )),
            Class::Other => {}
        }
    }
    for l in open.iter().chain(unresolved.iter()) {
        println!("{}", l);
    }
    if !open.is_empty() {
        println!("done-check=fail open={}", open.len());
        1
    } else if !unresolved.is_empty() {
        println!("done-check=cannot-check unresolved={}", unresolved.len());
        3
    } else {
        println!("done-check=pass open=0");
        0
    }
}

fn step(host: &Path, verb: &str, id: &str) -> bool {
    matches!(
        run_host(
            host,
            &[OsStr::new(verb), OsStr::new(id)],
            None,
            AUX_TIMEOUT_MS
        ),
        Ok(HostOut { ok: true, .. })
    )
}

/// `harness session close ...`: 0 every matching session removed, 1 a `stop` or `rm` failed (the rest are not tried), 3 listing unreadable, 2 usage.
pub fn close_cmd(a: &[String]) -> i32 {
    let o = match parse(a, "--ticket", false) {
        Ok(o) => o,
        Err(m) => return usage(&m),
    };
    let (host, lanes, done) = match setup(&o) {
        Ok(x) => x,
        Err(m) => return usage(&m),
    };
    let entries = match listing(&host) {
        Ok(e) => e,
        Err(m) => {
            eprintln!("harness: close: {}", m);
            return 3;
        }
    };
    let mut closed = 0;
    for e in &entries {
        match classify(e, lanes, &done) {
            Class::Open => {
                if e.state.as_deref() != Some("done") && !step(&host, "stop", &e.id) {
                    println!("close=fail name={} id={} step=stop", e.name, e.id);
                    return 1;
                }
                if !step(&host, "rm", &e.id) {
                    println!("close=fail name={} id={} step=rm", e.name, e.id);
                    return 1;
                }
                println!("closed name={} id={}", e.name, e.id);
                closed += 1;
            }
            Class::Unresolved(_) => println!("unresolved name={} id={}", e.name, e.id),
            Class::Other => {}
        }
    }
    println!("close=pass ticket={} closed={}", done[0], closed);
    0
}
