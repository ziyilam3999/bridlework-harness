//! Command line. Exit 0 clean, 1 hit, 2 error (nothing on stdout when erroring).
use crate::classes;
use crate::explain;
use crate::gitio;
use crate::overlay;
use crate::scan::{scan_text, Ctx, Hit};
use std::io::Read;

fn take_testmode(args: &mut Vec<String>) -> bool {
    let mut on = false;
    if let Some(i) = args.iter().position(|a| a == "--testmode") {
        args.remove(i);
        if i < args.len() {
            on = args.remove(i) == "synthetic-test";
        }
    }
    on
}

fn take_surface(args: &mut Vec<String>) -> Result<String, String> {
    let mut surface = "text".to_string();
    if let Some(i) = args.iter().position(|a| a == "--surface") {
        args.remove(i);
        if i >= args.len() {
            return Err("missing surface".to_string());
        }
        surface = args.remove(i);
    }
    if surface.is_empty() || !surface.bytes().all(|c| c.is_ascii_lowercase() || c == b'_' || c == b'.') {
        return Err("bad surface".to_string());
    }
    Ok(surface)
}

struct Outcome {
    hits: Vec<Hit>,
    stderr: Vec<String>,
    text_mode: bool,
}

fn run_scan(args: &[String], testmode: bool, surface: &str) -> Result<Outcome, String> {
    let list = overlay::load()?;
    let ctx = Ctx { list: &list, testmode };
    let mode = args.first().map(|s| s.as_str()).ok_or("no mode given")?;
    let mut stderr = Vec::new();
    let mut text_mode = false;
    let hits = match mode {
        "--staged" => gitio::staged(&ctx)?,
        "--range" => {
            let (h, n) = gitio::range(&ctx, &args[1..])?;
            stderr.push(format!("scanned_commits={}", n));
            h
        }
        "--tag" => gitio::tag(&ctx, args.get(1).ok_or("no tag object")?)?,
        "--tree" => {
            let (h, n) = gitio::tree(&ctx)?;
            stderr.push(format!("files={}", n));
            h
        }
        "--text" => {
            text_mode = true;
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf).map_err(|_| "stdin unreadable".to_string())?;
            let t = String::from_utf8(buf).map_err(|_| "invalid utf-8".to_string())?;
            scan_text(&ctx, surface, &t)
        }
        _ => return Err("unknown mode".to_string()),
    };
    Ok(Outcome { hits, stderr, text_mode })
}

/// `harness fixtures names | list | near-miss | line <name> | marker <name> | class <name> | extra <name>`:
/// prints synthetic test material for drivers. It reads nothing.
fn fixtures_cmd(a: &[String]) -> i32 {
    use crate::fixtures;
    let cases = fixtures::cases();
    let pick = |n: &str| cases.iter().find(|c| c.name == n);
    match a.iter().map(|s| s.as_str()).collect::<Vec<&str>>().as_slice() {
        ["names"] => cases.iter().for_each(|c| println!("{}", c.name)),
        ["list"] => print!("{}", fixtures::synthetic_list()),
        ["near-miss"] => fixtures::near_misses().iter().for_each(|l| println!("{}", l)),
        ["line", n] => match pick(n) {
            Some(c) => println!("{}", c.line),
            None => return 2,
        },
        ["marker", n] => match pick(n) {
            Some(c) => println!("{}", c.marker),
            None => return 2,
        },
        ["class", n] => match pick(n) {
            Some(c) => println!("{}", c.class),
            None => return 2,
        },
        ["extra", n] => match fixtures::extra(n) {
            Some(t) => println!("{}", t),
            None => return 2,
        },
        _ => return 2,
    }
    0
}

const EVENTS_USAGE: &str = "usage: harness events append [--log <path>] [--root <dir>] --kind <kind> --ticket <t> --repo <r> --reason <x> [--ts-ms <n>] | events list|verify [--log <path>]";
/// Default log, relative to the working folder (the ticket's repo). Git-ignored (`.harness/`).
const EVENTS_DEFAULT_LOG: &str = ".harness/events.jsonl";

/// Collect `--flag value` pairs. Only the flags in `allowed` are accepted, each at most once, each with a value.
fn take_event_opts(rest: &[String], allowed: &[&str]) -> Result<std::collections::HashMap<String, String>, String> {
    let mut out = std::collections::HashMap::new();
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let name = flag.strip_prefix("--").filter(|n| allowed.contains(n)).ok_or_else(|| format!("unknown option {:?}", flag))?;
        let val = it.next().ok_or_else(|| format!("{} needs a value", flag))?;
        if out.insert(name.to_string(), val.clone()).is_some() {
            return Err(format!("{} given twice", flag));
        }
    }
    Ok(out)
}

/// `harness events append | list | verify`. Exit 0 ok; 1 the log is not clean (torn tail or corruption; `list` still prints the
/// intact events before it); 2 error or refusal (usage, bad input, unreadable or unwritable log, torn-tail append). A refused
/// append writes nothing to the log and says on stderr that the event was not recorded.
fn events_cmd(a: &[String]) -> i32 {
    use crate::events::{self, Event, Kind};
    use std::path::{Path, PathBuf};
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{}", EVENTS_USAGE);
        return 2;
    };
    match sub.as_str() {
        "append" => {
            let o = match take_event_opts(rest, &["log", "root", "kind", "ticket", "repo", "reason", "ts-ms"]) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("harness: error: {}", e);
                    eprintln!("harness: EVENT NOT RECORDED");
                    return 2;
                }
            };
            let need = |k: &str| o.get(k).cloned().ok_or_else(|| format!("--{} is required", k));
            let built = (|| -> Result<Event, String> {
                let kind = Kind::parse(&need("kind")?).ok_or("unknown kind")?;
                let ts_ms = match o.get("ts-ms") {
                    Some(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => s.parse::<u64>().map_err(|_| "--ts-ms out of range".to_string())?,
                    Some(_) => return Err("--ts-ms must be a non-negative integer".to_string()),
                    None => std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| "clock is before the epoch".to_string())?.as_millis() as u64,
                };
                Ok(Event { kind, ts_ms, ticket: need("ticket")?, repo: need("repo")?, reason: need("reason")? })
            })();
            let ev = match built {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("harness: error: {}", e);
                    eprintln!("harness: EVENT NOT RECORDED");
                    return 2;
                }
            };
            let default_log = !o.contains_key("log");
            let log = PathBuf::from(o.get("log").map(|s| s.as_str()).unwrap_or(EVENTS_DEFAULT_LOG));
            let root = match o.get("root") {
                Some(r) => PathBuf::from(r),
                None if default_log => PathBuf::from("."),
                None => match log.parent() {
                    Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                    _ => PathBuf::from("."),
                },
            };
            if default_log {
                if let Some(dir) = log.parent() {
                    // The default folder is created inside the working folder, so it must lie under the root: check that BEFORE
                    // creating anything, so a refused append leaves no empty folder behind.
                    if !dir.exists() {
                        let inside = match (std::path::Path::new(".").canonicalize(), root.canonicalize()) {
                            (Ok(cwd), Ok(r)) => cwd.starts_with(r),
                            _ => false,
                        };
                        if !inside {
                            eprintln!("harness: error: log path refused: the working folder is outside the root");
                            eprintln!("harness: EVENT NOT RECORDED");
                            return 2;
                        }
                    }
                    if let Err(e) = std::fs::create_dir_all(dir) {
                        eprintln!("harness: error: cannot create the default log folder: {}", e.kind());
                        eprintln!("harness: EVENT NOT RECORDED");
                        return 2;
                    }
                }
            }
            match events::append(&root, &log, &ev) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("harness: error: {}", e);
                    eprintln!("harness: EVENT NOT RECORDED");
                    2
                }
            }
        }
        "list" | "verify" => {
            let o = match take_event_opts(rest, &["log"]) {
                Ok(o) => o,
                Err(e) => return fail(&e),
            };
            let log = o.get("log").map(|s| s.as_str()).unwrap_or(EVENTS_DEFAULT_LOG);
            let r = match events::read_log(Path::new(log)) {
                Ok(r) => r,
                Err(e) => return fail(&e),
            };
            if sub == "list" {
                for e in &r.events {
                    match e.encode() {
                        Ok(l) => print!("{}", l),
                        Err(e) => return fail(&e.to_string()),
                    }
                }
            }
            if let Some(c) = &r.corrupt {
                eprintln!("harness: log is corrupt at line {}: {}", c.line, c.msg);
                return 1;
            }
            if let Some(t) = &r.torn_tail {
                eprintln!("harness: log has a torn tail at line {} ({} bytes, no newline); not read as an event", t.line, t.bytes);
                return 1;
            }
            0
        }
        _ => {
            eprintln!("{}", EVENTS_USAGE);
            2
        }
    }
}

const CONFIG_USAGE: &str = "usage: harness config check [--config <file>] [--repo <repo_id>] [--budget <S|M|L>]";

/// `harness config check`. Exit 0 the config loaded (stdout: `config ok repos=<n> shadow=ok`, plus `repo_id=<id> mapped` and
/// `budget size=<S> wall_seconds=<n> tokens=<n>` when asked); 1 `--repo` names an id that is not in the map; 2 everything else
/// (no config configured, an empty source, a bad file, a bad command line). The source is `--config`, else `HARNESS_CONFIG`, nothing
/// else: an empty value is refused and never falls back. No folder and no config value is ever printed.
fn config_cmd(a: &[String]) -> i32 {
    use crate::config::Config;
    use crate::repo_id::RepoId;
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    if a.first().map(|s| s.as_str()) != Some("check") {
        eprintln!("{}", CONFIG_USAGE);
        return 2;
    }
    let (mut file, mut repo, mut size): (Option<&str>, Option<&str>, Option<&str>) = (None, None, None);
    let mut it = a[1..].iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--config" => &mut file,
            "--repo" => &mut repo,
            "--budget" => &mut size,
            _ => return fail("unknown option"),
        };
        let Some(val) = it.next() else { return fail("an option is missing its value") };
        if slot.replace(val.as_str()).is_some() {
            return fail("an option was given twice");
        }
    }
    let id = match repo.map(RepoId::parse) {
        Some(Ok(id)) => Some(id),
        Some(Err(e)) => return fail(&e),
        None => None,
    };
    let path = match file {
        Some(f) => std::ffi::OsString::from(f),
        None => match std::env::var_os("HARNESS_CONFIG") {
            Some(v) => v,
            None => return fail("no config configured (pass --config <file> or set HARNESS_CONFIG)"),
        },
    };
    if path.is_empty() {
        return fail("the config source is empty");
    }
    let cfg = match Config::load(std::path::Path::new(&path)) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    // A bad size is a command-line error (2) and is decided BEFORE the repo lookup (1), so a bad size never reads as a lookup refusal.
    let budget_line = match size {
        Some(s) => match cfg.budget(s) {
            Some(b) => Some(format!("budget size={} wall_seconds={} tokens={}", s, b.wall_seconds, b.tokens)),
            None => return fail("--budget must be exactly S, M or L"),
        },
        None => None,
    };
    if let Some(id) = &id {
        if cfg.repo_dir(id).is_none() {
            eprintln!("harness: repo_id {} is not in the config map", id.as_str());
            return 1;
        }
    }
    println!("config ok repos={} shadow=ok", cfg.repo_count());
    if let Some(id) = &id {
        println!("repo_id={} mapped", id.as_str());
    }
    if let Some(l) = budget_line {
        println!("{}", l);
    }
    0
}

const TICKET_USAGE: &str = "usage: harness ticket intake <ticket-file> --repo-map <repo_id>=<absolute-dir> [--repo-map ...] | ticket gate <ticket-file> --repo-map <repo_id>=<absolute-dir> [--repo-map ...] --base <full-sha> --result-dir <absolute-dir>";
/// One byte over the 1 MiB ticket cap: an oversize file is read no further than this, then refused by the strict reader.
const TICKET_READ_LIMIT: u64 = 1024 * 1024 + 1;

/// `harness ticket intake <ticket-file> --repo-map <id>=<dir> [--repo-map ...]`. Exit 0 accepted (one stdout line:
/// `ticket=<id> repo_id=<id> head=<sha>`); 1 the ticket is refused (stdout empty, stderr names the field or the repo id);
/// 2 usage error or the ticket file cannot be read (stdout empty). The repo comes from the ticket and the map only, never from the
/// working folder. Read-only. No folder (mapped or ticket file) is ever printed.
fn ticket_cmd(a: &[String]) -> i32 {
    use crate::intake::{intake, RepoMap};
    use crate::ticket::Ticket;
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    if a.first().map(|s| s.as_str()) == Some("gate") {
        #[cfg(unix)]
        return crate::ticket_gate::gate_cmd(&a[1..]);
        #[cfg(not(unix))]
        return fail("ticket gate is not available on this platform");
    }
    if a.first().map(|s| s.as_str()) != Some("intake") {
        eprintln!("{}", TICKET_USAGE);
        return 2;
    }
    let mut map = RepoMap::new();
    let mut file: Option<&str> = None;
    let mut it = a[1..].iter();
    while let Some(arg) = it.next() {
        if arg == "--repo-map" {
            let Some(spec) = it.next() else { return fail("--repo-map is missing its value") };
            if let Err(e) = map.add(spec) {
                return fail(&e);
            }
        } else if arg.starts_with("--") {
            return fail("unknown option");
        } else if file.replace(arg.as_str()).is_some() {
            return fail("more than one ticket file given");
        }
    }
    let Some(file) = file else {
        eprintln!("{}", TICKET_USAGE);
        return 2;
    };
    let mut bytes = Vec::new();
    let read = std::fs::File::open(file).and_then(|f| f.take(TICKET_READ_LIMIT).read_to_end(&mut bytes));
    if read.is_err() {
        return fail("the ticket file cannot be read");
    }
    let ticket = match Ticket::parse_bytes(&bytes) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("harness: ticket refused: {}", e);
            return 1;
        }
    };
    match intake(&ticket, &map) {
        Ok(ok) => {
            println!("ticket={} repo_id={} head={}", ok.ticket_id, ok.repo_id.as_str(), ok.head_sha);
            0
        }
        Err(r) => {
            eprintln!("harness: ticket refused: {}", r);
            1
        }
    }
}

pub fn run(mut args: Vec<String>) -> i32 {
    if args.len() == 1 && args[0] == "--version" {
        println!("{}", crate::version_line());
        return 0;
    }
    let testmode = take_testmode(&mut args);
    if args.len() == 2 && args[0] == "privacy" && args[1] == "classes" {
        for c in classes::ALL {
            println!("{}", c);
        }
        return 0;
    }
    if !args.is_empty() && args[0] == "events" {
        return events_cmd(&args[1..]);
    }
    if !args.is_empty() && args[0] == "config" {
        return config_cmd(&args[1..]);
    }
    if !args.is_empty() && args[0] == "ticket" {
        return ticket_cmd(&args[1..]);
    }
    if !args.is_empty() && args[0] == "status" {
        return crate::status::status_cmd(&args[1..]);
    }
    if !args.is_empty() && args[0] == "fixtures" {
        return fixtures_cmd(&args[1..]);
    }
    if args.len() < 3 || args[0] != "privacy" || (args[1] != "scan" && args[1] != "explain") {
        eprintln!("usage: harness --version | privacy classes | privacy scan|explain <mode> | status <ticket-file> ...");
        return 2;
    }
    let explain_mode = args[1] == "explain";
    let mut rest: Vec<String> = args[2..].to_vec();
    let surface = match take_surface(&mut rest) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("harness: error: {}", e);
            return 2;
        }
    };
    match run_scan(&rest, testmode, &surface) {
        Err(e) => {
            eprintln!("harness: error: {}", e);
            2
        }
        Ok(o) => {
            for l in &o.stderr {
                eprintln!("{}", l);
            }
            for h in &o.hits {
                let line = if explain_mode {
                    explain::explain_line(h, o.text_mode)
                } else if o.text_mode {
                    explain::text_line(h)
                } else {
                    explain::hit_line(h)
                };
                println!("{}", line);
            }
            if o.hits.is_empty() {
                0
            } else {
                1
            }
        }
    }
}
