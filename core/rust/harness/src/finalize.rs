//! `harness ticket finalize` (P1B-15; std only, Unix). The one command that decides whether a ticket may be called Done. It builds nothing:
//! every check runs as a CHILD of this same binary (the pattern `teardown.rs` uses), its stdout is captured under a fixed cap and never
//! printed, its stderr is dropped. Order (the first "no" stops, exit 1): gate (via `status`), pushed, clean, sessions (P1B-17: close this ticket's host sessions, then re-read the listing), sweep, teardown, evidence, then
//! an in-process re-read of the manifest. The sweep runs before teardown (teardown keeps evidence files, so a later sweep would shelve them);
//! evidence runs after teardown (only teardown stops a live process, and a live process makes evidence refuse). Exit 0 plus the one
//! `finalize=done` line IS "marked Done" (the `ticket gate` convention). See docs/finalize.md. No folder is ever printed.

use crate::events::token_ok;
use crate::manifest::{self, Class, Target};
use crate::ticket::Ticket;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

pub const USAGE: &str = "usage: harness ticket finalize <ticket-file> --manifest-dir <abs> --repo-dir <abs> --workspace-root <abs> --quarantine-dir <abs> --result-dir <abs> --caps-dir <abs> --log <abs> --role <token> --model <token> --host <abs> --config <abs> [--grace-ms <n>] [--ts-ms <n>]";
/// One byte over the 1 MiB ticket cap: an oversize file is read no further than this, then refused by the strict reader.
const TICKET_READ_LIMIT: u64 = 1024 * 1024 + 1;
/// A child's stdout is read no further than this; more than this is `cannot-check`.
const CHILD_STDOUT_CAP: u64 = 4 * 1024 * 1024;

struct Args {
    ticket_file: String,
    manifest_dir: String,
    repo_dir: String,
    workspace_root: String,
    quarantine_dir: String,
    result_dir: String,
    caps_dir: String,
    log: String,
    role: String,
    model: String,
    host: String,
    config: String,
    grace_ms: Option<String>,
    ts_ms: Option<String>,
}

fn refuse(m: &str) -> i32 {
    eprintln!("harness: error: {}", m);
    2
}

fn abs_ok(s: &str) -> bool {
    s.starts_with('/') && s.len() <= 4096 && !s.contains('\0') && !s.split('/').skip(1).any(|c| c == "." || c == "..")
}

fn num_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u64>().is_ok()
}

fn parse(a: &[String]) -> Result<Args, String> {
    let mut file: Option<String> = None;
    let mut slots: [Option<String>; 13] = Default::default();
    const NAMES: [&str; 13] = [
        "manifest-dir", "repo-dir", "workspace-root", "quarantine-dir", "result-dir", "caps-dir", "log", "role", "model", "grace-ms", "ts-ms", "host", "config",
    ];
    let mut i = 0;
    while i < a.len() {
        if let Some(name) = a[i].strip_prefix("--") {
            let Some(idx) = NAMES.iter().position(|n| *n == name) else { return Err(format!("unknown option --{}", name)) };
            let v = a.get(i + 1).ok_or_else(|| format!("--{} needs a value", name))?;
            if slots[idx].replace(v.clone()).is_some() {
                return Err(format!("--{} given twice", name));
            }
            i += 2;
        } else {
            if file.replace(a[i].clone()).is_some() {
                return Err("more than one ticket file given".to_string());
            }
            i += 1;
        }
    }
    let file = file.ok_or("no ticket file given")?;
    let mut take = |idx: usize| -> Result<String, String> { slots[idx].take().ok_or_else(|| format!("--{} is required", NAMES[idx])) };
    let md = take(0)?;
    let rd = take(1)?;
    let wr = take(2)?;
    let qd = take(3)?;
    let res = take(4)?;
    let caps = take(5)?;
    let log = take(6)?;
    let role = take(7)?;
    let model = take(8)?;
    let host = take(11)?;
    let config = take(12)?;
    let grace = slots[9].take();
    let ts = slots[10].take();
    for (n, v) in [("manifest-dir", &md), ("repo-dir", &rd), ("workspace-root", &wr), ("quarantine-dir", &qd), ("result-dir", &res), ("caps-dir", &caps), ("log", &log), ("host", &host), ("config", &config)] {
        if !abs_ok(v) {
            return Err(format!("--{} must be an absolute path with no `.` or `..` component", n));
        }
    }
    if !std::fs::metadata(&host).map(|m| m.is_file()).unwrap_or(false) {
        return Err("--host is not an existing regular file".to_string());
    }
    if !token_ok(&role) || !token_ok(&model) {
        return Err("--role and --model must be plain tokens".to_string());
    }
    for (n, v) in [("grace-ms", &grace), ("ts-ms", &ts)] {
        if let Some(v) = v {
            if !num_ok(v) {
                return Err(format!("--{} must be a non-negative integer", n));
            }
        }
    }
    Ok(Args { ticket_file: file, manifest_dir: md, repo_dir: rd, workspace_root: wr, quarantine_dir: qd, result_dir: res, caps_dir: caps, log, role, model, host, config, grace_ms: grace, ts_ms: ts })
}

/// One child run of this same binary: no stdin, stderr dropped, stdout captured under the cap.
/// None when it could not be spawned, was ended by a signal, or wrote more than the cap.
fn child(args: &[String]) -> Option<(i32, String)> {
    let exe = std::env::current_exe().ok()?;
    let mut c = Command::new(exe).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut buf = Vec::new();
    let read = c.stdout.take()?.take(CHILD_STDOUT_CAP + 1).read_to_end(&mut buf);
    if read.is_err() || buf.len() as u64 > CHILD_STDOUT_CAP {
        let _ = c.kill();
        let _ = c.wait();
        return None;
    }
    let code = c.wait().ok()?.code()?;
    Some((code, String::from_utf8_lossy(&buf).to_string()))
}

fn sv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Why {
    Fail,
    CannotCheck,
}

struct Stop {
    check: &'static str,
    why: Why,
    /// The session a `sessions` failure names, from the failing child's own output.
    session: Option<String>,
}

fn stop(check: &'static str, why: Why) -> Stop {
    Stop { check, why, session: None }
}

/// The `name=` value of the first line starting with `lead`, only when it is itself a valid session name.
fn session_of(out: &str, lead: &str) -> String {
    let name = out.lines().find(|l| l.starts_with(lead)).and_then(|l| field(l, "name"));
    match name {
        Some(n) if crate::session_name::check(n, u64::MAX, false).is_ok() => n.to_string(),
        _ => "-".to_string(),
    }
}

/// A child's exit code folded to pass / fail / cannot-check: `ok` passes, `no` is the check's own "no", anything else cannot be checked.
fn fold(check: &'static str, r: Option<(i32, String)>, ok: i32, no: &[i32]) -> Result<String, Stop> {
    match r {
        Some((c, o)) if c == ok => Ok(o),
        Some((c, _)) if no.contains(&c) => Err(stop(check, Why::Fail)),
        _ => Err(stop(check, Why::CannotCheck)),
    }
}

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ').find_map(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

/// The live `worktree` entry's path, from a clean manifest only.
fn worktree_of(md: &Path, t: &str) -> Option<String> {
    let read = manifest::read_manifest(md, t).ok()?;
    if !read.is_clean() {
        return None;
    }
    let e = read.live.iter().find(|e| e.id == "worktree" && e.class == Class::Worktree)?;
    match &e.target {
        Target::Path(p) => Some(p.clone()),
        _ => None,
    }
}

fn run_checks(a: &Args, t: &Ticket, tree: &str) -> Result<String, Stop> {
    let id = t.id.as_str();
    let repo = t.repo_id.as_str();
    let map = format!("{}={}", repo, tree);
    // a: the gate's note, read through `status`.
    let st = child(&sv(&["status", &a.ticket_file, "--repo-map", &map, "--result-dir", &a.result_dir, "--caps-dir", &a.caps_dir]));
    let line = fold("gate", st, 0, &[])?;
    let line = line.trim_end();
    if field(line, "last_gate") != Some("pass") || field(line, "gate_current") != Some("yes") {
        return Err(stop("gate", Why::Fail));
    }
    let head = match field(line, "head") {
        Some(h) if sha_ok(h) => h.to_string(),
        _ => return Err(stop("gate", Why::CannotCheck)),
    };
    // b: every commit is on the remote.
    let r = child(&sv(&["workspace", "check", id, "--manifest-dir", &a.manifest_dir, "--pushed", "--repo", &a.repo_dir]));
    fold("pushed", r, 0, &[4])?;
    // c: nothing unexpected in the work folder.
    let r = child(&sv(&["workspace", "check", id, "--manifest-dir", &a.manifest_dir]));
    fold("clean", r, 0, &[2])?;
    // c2: close this ticket's host sessions, then re-read the listing (S-1.6, S-1.6.1). Nothing before this step acts.
    let (host, cfg) = (a.host.as_str(), a.config.as_str());
    match child(&sv(&["session", "close", "--host", host, "--config", cfg, "--ticket", id])) {
        Some((0, _)) => {}
        Some((1, o)) => return Err(Stop { check: "sessions", why: Why::Fail, session: Some(session_of(&o, "close=fail ")) }),
        _ => return Err(stop("sessions", Why::CannotCheck)),
    }
    match child(&sv(&["session", "done-check", "--host", host, "--config", cfg, "--done", id])) {
        Some((0, _)) => {}
        Some((1, o)) => return Err(Stop { check: "sessions", why: Why::Fail, session: Some(session_of(&o, "session=open ")) }),
        _ => return Err(stop("sessions", Why::CannotCheck)),
    }
    // d: unlisted leftovers go to the shelf; moving any one refuses Done.
    let mut sweep = sv(&["quarantine", "sweep", id, "--manifest-dir", &a.manifest_dir, "--workspace-root", &a.workspace_root, "--quarantine-dir", &a.quarantine_dir]);
    sweep.extend(sv(&["--log", &a.log, "--repo", repo, "--role", &a.role, "--model", &a.model]));
    if let Some(ts) = &a.ts_ms {
        sweep.extend(sv(&["--ts-ms", ts]));
    }
    let out = fold("sweep", child(&sweep), 0, &[])?;
    let prefix = format!("quarantine=done ticket={} moved=", id);
    match out.trim_end().strip_prefix(prefix.as_str()) {
        Some("0") => {}
        Some(n) if num_ok(n) => return Err(stop("sweep", Why::Fail)),
        _ => return Err(stop("sweep", Why::CannotCheck)),
    }
    // e: stop processes, release ports, remove listed files and the worktree.
    let mut td = sv(&["workspace", "teardown", id, "--manifest-dir", &a.manifest_dir, "--repo-dir", &a.repo_dir, "--log", &a.log, "--repo", repo]);
    if let Some(g) = &a.grace_ms {
        td.extend(sv(&["--grace-ms", g]));
    }
    if let Some(ts) = &a.ts_ms {
        td.extend(sv(&["--ts-ms", ts]));
    }
    let r = child(&td);
    let out = fold("teardown", r, 0, &[1])?;
    if out.trim_end() != format!("teardown=pass ticket={} manifest_left=0", id) {
        return Err(stop("teardown", Why::CannotCheck));
    }
    // f: runtime evidence, read after teardown has stopped every process.
    let r = child(&sv(&["evidence", "check", id, "--manifest-dir", &a.manifest_dir]));
    fold("evidence", r, 0, &[1])?;
    // g: nothing live is left in the manifest.
    match manifest::read_manifest(Path::new(&a.manifest_dir), id) {
        Ok(read) if read.is_clean() && read.live.is_empty() => Ok(head),
        Ok(read) if read.is_clean() => Err(stop("done", Why::Fail)),
        _ => Err(stop("done", Why::CannotCheck)),
    }
}

/// `harness ticket finalize ...`. Exit 0 = Done (one line `finalize=done ticket=<t> repo_id=<r> head=<sha>`); 1 = Done refused (one line
/// `finalize=refused ticket=<t> check=<name> reason=<fail|cannot-check>`); 2 = usage or input error, nothing was run.
pub fn finalize_cmd(a: &[String]) -> i32 {
    let args = match parse(a) {
        Ok(x) => x,
        Err(m) => {
            eprintln!("harness: error: {}\n{}", m, USAGE);
            return 2;
        }
    };
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&args.ticket_file).and_then(|f| f.take(TICKET_READ_LIMIT).read_to_end(&mut bytes));
    if read.is_err() {
        return refuse("the ticket file cannot be read");
    }
    let ticket = match Ticket::parse_bytes(&bytes) {
        Ok(t) => t,
        Err(e) => return refuse(&format!("ticket refused: {}", e)),
    };
    if !token_ok(&ticket.id) || !token_ok(ticket.repo_id.as_str()) {
        return refuse("the ticket id and repo_id must be plain tokens");
    }
    let Some(tree) = worktree_of(Path::new(&args.manifest_dir), &ticket.id) else {
        return refuse("the manifest is missing, damaged, or has no live `worktree` path entry");
    };
    match run_checks(&args, &ticket, &tree) {
        Ok(head) => {
            println!("finalize=done ticket={} repo_id={} head={}", ticket.id, ticket.repo_id.as_str(), head);
            0
        }
        Err(s) => {
            let why = if s.why == Why::Fail { "fail" } else { "cannot-check" };
            match &s.session {
                Some(n) => println!("finalize=refused ticket={} check={} reason={} session={}", ticket.id, s.check, why, n),
                None => println!("finalize=refused ticket={} check={} reason={}", ticket.id, s.check, why),
            }
            eprintln!("harness: finalize refused: check={}", s.check);
            1
        }
    }
}
