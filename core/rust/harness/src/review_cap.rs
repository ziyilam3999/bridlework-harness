//! Review-round cap (std only, Unix). The engine keeps its own count of review FAILs per ticket, because bw keeps one review record per ticket
//! and overwrites it. The record `<caps>/<ticket id>.reviews` is `{"v":1,"ticket":"<id>","repo_id":"<repo>","fail_heads":[...]}` plus a newline:
//! 0 to 16 distinct full lowercase shas in the order recorded. The reader takes the canonical bytes for the expected ticket and repo only.
//! Two verbs: `ticket review-fail` records one FAIL for a head (once per head); `ticket fix-round-check` refuses a fix round when 3 or more
//! heads failed, with one `escalation` event (reason `review-round-cap`) written with the role and model given as flags. See docs/review-round-cap.md.

use crate::caps::{read_bounded, Caps, Lock, LockError, Refusal};
use crate::events::{self, Kind};
use crate::intake::RepoMap;
use crate::ticket::Ticket;
use crate::ticket_gate::ticket_id_ok;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// A further fix round is refused when this many review FAIL heads are recorded.
pub const REVIEW_ROUND_CAP: usize = 3;
/// The most heads a record holds.
pub const MAX_HEADS: usize = 16;
/// The record schema version this reader accepts.
pub const REVIEW_RECORD_VERSION: u64 = 1;
/// The reason token of the review-round cap refusal.
pub const REASON_REVIEW_ROUND_CAP: &str = "review-round-cap";

const REVIEW_FAIL_USAGE: &str = "usage: harness ticket review-fail <ticket-file> --repo-map <repo_id>=<absolute-dir> [--repo-map ...] --caps-dir <absolute-dir> --head <full-sha>";
const FIX_ROUND_USAGE: &str = "usage: harness ticket fix-round-check <ticket-file> --repo-map <repo_id>=<absolute-dir> [--repo-map ...] --caps-dir <absolute-dir> --role <token> --model <token>";

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a ticket's review record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRecord {
    pub ticket: String,
    pub repo_id: String,
    pub fail_heads: Vec<String>,
}

impl ReviewRecord {
    /// A state a writer may produce: at most `MAX_HEADS` distinct full lowercase shas.
    pub fn state_ok(&self) -> bool {
        self.fail_heads.len() <= MAX_HEADS
            && self.fail_heads.iter().all(|h| sha_ok(h))
            && self.fail_heads.iter().enumerate().all(|(i, h)| !self.fail_heads[..i].contains(h))
    }

    /// The canonical bytes, newline included.
    pub fn encode(&self) -> String {
        let heads: Vec<String> = self.fail_heads.iter().map(|h| format!("\"{}\"", h)).collect();
        format!("{{\"v\":{},\"ticket\":\"{}\",\"repo_id\":\"{}\",\"fail_heads\":[{}]}}\n", REVIEW_RECORD_VERSION, self.ticket, self.repo_id, heads.join(","))
    }

    /// Strict decode: the bytes must equal the canonical encoding of a valid state for exactly this ticket and repo.
    pub fn decode(bytes: &[u8], ticket: &str, repo_id: &str) -> Result<ReviewRecord, String> {
        let refused = || "the review record is not the canonical record for this ticket and repo".to_string();
        let text = std::str::from_utf8(bytes).map_err(|_| refused())?;
        let lead = format!("{{\"v\":{},\"ticket\":\"{}\",\"repo_id\":\"{}\",\"fail_heads\":[", REVIEW_RECORD_VERSION, ticket, repo_id);
        let inner = text.strip_prefix(lead.as_str()).and_then(|r| r.strip_suffix("]}\n")).ok_or_else(refused)?;
        let mut fail_heads = Vec::new();
        if !inner.is_empty() {
            for part in inner.split(',') {
                let h = part.strip_prefix('"').and_then(|p| p.strip_suffix('"')).ok_or_else(refused)?;
                fail_heads.push(h.to_string());
            }
        }
        let rec = ReviewRecord { ticket: ticket.to_string(), repo_id: repo_id.to_string(), fail_heads };
        if !rec.state_ok() || rec.encode().as_bytes() != bytes {
            return Err(refused());
        }
        Ok(rec)
    }
}

fn record_name(id: &str) -> String {
    format!("{}.reviews", id)
}

/// A differently-cased twin of the record name is a collision (by name, ASCII case-insensitive).
fn check_case_twin(caps: &Caps, id: &str) -> Result<(), String> {
    let want = record_name(id);
    let rd = fs::read_dir(caps.dir()).map_err(|_| "the caps directory could not be listed".to_string())?;
    for e in rd {
        let name = e.map_err(|_| "the caps directory could not be listed".to_string())?.file_name().to_string_lossy().into_owned();
        if name != want && name.eq_ignore_ascii_case(&want) {
            return Err("a review record whose name differs only by letter case already exists".to_string());
        }
    }
    Ok(())
}

/// `Ok(None)` when no record exists (0 FAILs), `Ok(Some)` for a canonical record for this ticket and repo, `Err` for anything else.
pub fn read(caps: &Caps, id: &str, repo_id: &str) -> Result<Option<ReviewRecord>, String> {
    if !ticket_id_ok(id) {
        return Err("the ticket id cannot be used as a file name".to_string());
    }
    check_case_twin(caps, id)?;
    let path = caps.dir().join(record_name(id));
    match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(_) => return Err("the review record exists and is not a regular file".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("the review record could not be inspected".to_string()),
    }
    let f = File::open(&path).map_err(|_| "the review record could not be opened".to_string())?;
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err("the review record exists and is not a regular file".to_string());
    }
    let bytes = read_bounded(f)?;
    ReviewRecord::decode(&bytes, id, repo_id).map(Some)
}

/// Replace the record durably, with the per-ticket lock held: temp file, sync, rename, folder sync.
pub fn write(caps: &Caps, rec: &ReviewRecord, _held: &Lock) -> Result<(), String> {
    if !ticket_id_ok(&rec.ticket) || !rec.state_ok() {
        return Err("the review record state is out of range".to_string());
    }
    let target = caps.dir().join(record_name(&rec.ticket));
    let tmp = caps.dir().join(format!(".{}.reviews.tmp", rec.ticket));
    match fs::symlink_metadata(&target) {
        Ok(m) if !m.file_type().is_file() => return Err("the review record exists and is not a regular file".to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("the review record could not be inspected".to_string()),
    }
    match fs::symlink_metadata(&tmp) {
        Ok(_) => fs::remove_file(&tmp).map_err(|_| "a stale review temp file could not be removed".to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("the review temp file could not be inspected".to_string()),
    }
    let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|_| "the review record could not be created".to_string())?;
    if f.write_all(rec.encode().as_bytes()).and_then(|_| f.sync_all()).is_err() {
        let _ = fs::remove_file(&tmp);
        return Err("the review record could not be written".to_string());
    }
    drop(f);
    if fs::rename(&tmp, &target).is_err() {
        let _ = fs::remove_file(&tmp);
        return Err("the review record could not be put in place".to_string());
    }
    File::open(caps.dir()).and_then(|d| d.sync_all()).map_err(|_| "the caps directory could not be synced".to_string())
}

/// A stop: the exit code and the full stderr text.
struct Stop(i32, String);

fn err2(m: impl AsRef<str>) -> Stop {
    Stop(2, format!("harness: error: {}", m.as_ref()))
}

/// What both verbs share once the command line and the ticket are checked.
struct Ctx {
    ticket_id: String,
    repo_id: String,
    caps: Caps,
    flags: HashMap<String, String>,
}

fn abs_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

/// Parse the command line (usage faults exit 2 with the usage text), read the ticket, find the repo, open the caps folder.
fn load(a: &[String], extra: &[&str], usage: &str) -> Result<Ctx, Stop> {
    let ue = |m: &str| Stop(2, format!("harness: error: {}\n{}", m, usage));
    let mut map = RepoMap::new();
    let mut file: Option<&str> = None;
    let mut flags: HashMap<String, String> = HashMap::new();
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        if arg == "--repo-map" {
            let Some(spec) = it.next() else { return Err(ue("--repo-map is missing its value")) };
            map.add(spec).map_err(|e| ue(&e))?;
        } else if let Some(name) = arg.strip_prefix("--") {
            if name != "caps-dir" && !extra.contains(&name) {
                return Err(ue("unknown option"));
            }
            let Some(v) = it.next() else { return Err(ue("an option is missing its value")) };
            if flags.insert(name.to_string(), v.clone()).is_some() {
                return Err(ue("an option was given twice"));
            }
        } else if file.replace(arg.as_str()).is_some() {
            return Err(ue("more than one ticket file given"));
        }
    }
    let file = file.ok_or_else(|| ue("no ticket file given"))?;
    for k in extra.iter().chain(["caps-dir"].iter()) {
        if !flags.contains_key(*k) {
            return Err(ue(&format!("--{} is required", k)));
        }
    }
    if !abs_ok(&flags["caps-dir"]) {
        return Err(ue("--caps-dir must be an absolute path with no `..` component"));
    }
    if let Some(h) = flags.get("head") {
        if !sha_ok(h) {
            return Err(ue("--head must be a full lowercase hex commit sha"));
        }
    }
    for k in ["role", "model"] {
        if let Some(v) = flags.get(k) {
            if !events::token_ok(v) {
                return Err(ue(&format!("--{} must be a plain token (lowercase letters, digits, '-', '_', '.')", k)));
            }
        }
    }
    let mut bytes = Vec::new();
    let read = File::open(file).and_then(|f| f.take(1024 * 1024 + 1).read_to_end(&mut bytes));
    if read.is_err() {
        return Err(err2("the ticket file cannot be read"));
    }
    let ticket = Ticket::parse_bytes(&bytes).map_err(|e| Stop(1, format!("harness: ticket refused: {}", e)))?;
    let repo_id = ticket.repo_id.as_str().to_string();
    let Some(mapped) = map.lookup(&ticket.repo_id) else {
        return Err(Stop(1, format!("harness: ticket refused: repo id `{}` is not in the repo map", repo_id)));
    };
    let root: PathBuf = mapped.canonicalize().map_err(|_| Stop(1, format!("harness: ticket refused: repo not found for repo id `{}`", repo_id)))?;
    if !ticket_id_ok(&ticket.id) {
        return Err(err2("the ticket id cannot be used as a file name"));
    }
    let caps = Caps::open(Path::new(&flags["caps-dir"]), &root).map_err(err2)?;
    Ok(Ctx { ticket_id: ticket.id, repo_id, caps, flags })
}

fn finish(r: Result<i32, Stop>) -> i32 {
    match r {
        Ok(code) => code,
        Err(Stop(code, msg)) => {
            eprintln!("{}", msg);
            code
        }
    }
}

/// `harness ticket review-fail`: record one review FAIL for a head. Exit 0 (counted=yes or counted=no); 1 ticket refusal; 2 any other error.
pub fn review_fail_cmd(a: &[String]) -> i32 {
    finish((|| -> Result<i32, Stop> {
        let cx = load(a, &["head"], REVIEW_FAIL_USAGE)?;
        let head = cx.flags["head"].clone();
        let lock = cx.caps.lock(&cx.ticket_id).map_err(|e| match e {
            LockError::Busy => err2("a run for this ticket is in progress"),
            LockError::Other(m) => err2(m),
        })?;
        let mut rec = read(&cx.caps, &cx.ticket_id, &cx.repo_id).map_err(err2)?.unwrap_or(ReviewRecord {
            ticket: cx.ticket_id.clone(),
            repo_id: cx.repo_id.clone(),
            fail_heads: Vec::new(),
        });
        let counted = !rec.fail_heads.contains(&head);
        if counted {
            if rec.fail_heads.len() >= MAX_HEADS {
                return Err(err2("the review record is full (16 heads); nothing was written"));
            }
            rec.fail_heads.push(head.clone());
            write(&cx.caps, &rec, &lock).map_err(err2)?;
        }
        println!("review-fail ticket={} head={} fails={} counted={}", cx.ticket_id, head, rec.fail_heads.len(), if counted { "yes" } else { "no" });
        Ok(0)
    })())
}

/// `harness ticket fix-round-check`: allowed (exit 0) with fewer than `REVIEW_ROUND_CAP` FAILs; else one escalation event, then refused (exit 1).
/// A failed append is exit 2 with `ESCALATION NOT RECORDED` and no refused line.
pub fn fix_round_check_cmd(a: &[String]) -> i32 {
    finish((|| -> Result<i32, Stop> {
        let cx = load(a, &["role", "model"], FIX_ROUND_USAGE)?;
        let fails = read(&cx.caps, &cx.ticket_id, &cx.repo_id).map_err(err2)?.map(|r| r.fail_heads.len()).unwrap_or(0);
        if fails < REVIEW_ROUND_CAP {
            println!("ticket={} result=allowed fails={}", cx.ticket_id, fails);
            return Ok(0);
        }
        let refusal = Refusal { kind: Kind::Escalation, reason: REASON_REVIEW_ROUND_CAP };
        let inherited = std::env::var_os("HARNESS_RUN_ID").map(|v| v.to_string_lossy().to_string());
        cx.caps
            .escalate_with(&refusal, &cx.ticket_id, &cx.repo_id, &cx.flags["role"], &cx.flags["model"], inherited.as_deref())
            .map_err(|m| Stop(2, format!("harness: ESCALATION NOT RECORDED reason={} ({})", REASON_REVIEW_ROUND_CAP, m)))?;
        println!("ticket={} result=refused reason={} fails={} next=operator-waiting", cx.ticket_id, REASON_REVIEW_ROUND_CAP, fails);
        Ok(1)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(i: usize) -> String {
        format!("{:040x}", i + 1)
    }

    fn rec(n: usize) -> ReviewRecord {
        ReviewRecord { ticket: "EX-1".to_string(), repo_id: "r".to_string(), fail_heads: (0..n).map(sha).collect() }
    }

    #[test]
    fn the_review_record_round_trips() {
        assert_eq!(rec(0).encode(), "{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\",\"fail_heads\":[]}\n");
        for n in [0, 1, 16] {
            let r = rec(n);
            assert_eq!(ReviewRecord::decode(r.encode().as_bytes(), "EX-1", "r"), Ok(r));
        }
    }

    #[test]
    fn the_review_record_reader_takes_canonical_bytes_only() {
        let good = rec(2).encode();
        let d = |s: &str, t: &str, r: &str| ReviewRecord::decode(s.as_bytes(), t, r);
        assert!(d(&good, "EX-1", "r").is_ok());
        assert!(d(&good, "EX-2", "r").is_err(), "other ticket");
        assert!(d(&good, "EX-1", "q").is_err(), "other repo");
        let (a, b) = (sha(0), sha(1));
        let enc = |heads: &[&str]| {
            let h: Vec<String> = heads.iter().map(|x| format!("\"{}\"", x)).collect();
            format!("{{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\",\"fail_heads\":[{}]}}\n", h.join(","))
        };
        assert!(d(&enc(&[&a, &b]), "EX-1", "r").is_ok());
        assert!(d(&enc(&[&a, &a]), "EX-1", "r").is_err(), "duplicate");
        assert!(d(&enc(&["ABCDEF0123456789ABCDEF0123456789ABCDEF01"]), "EX-1", "r").is_err(), "uppercase");
        assert!(d(&enc(&["abc123"]), "EX-1", "r").is_err(), "short");
        assert!(d(&good.replace('\n', ""), "EX-1", "r").is_err(), "no newline");
        assert!(d(&format!("{} ", good.trim_end()), "EX-1", "r").is_err(), "trailing space");
        assert!(d(&good.replace(",\"", ", \""), "EX-1", "r").is_err(), "widened");
        assert!(d(&good.replace("\"v\":1", "\"v\":2"), "EX-1", "r").is_err(), "version");
        let seventeen: Vec<String> = (0..17).map(sha).collect();
        let refs: Vec<&str> = seventeen.iter().map(|s| s.as_str()).collect();
        assert!(d(&enc(&refs), "EX-1", "r").is_err(), "17 heads");
    }
}
