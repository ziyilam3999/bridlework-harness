//! `harness ticket risk` (std only, Unix; `docs/risk.md` is the contract). Prints a risk class for the diff
//! between two commits of one repo, computed from the changed paths and never from the ticket's declared
//! `risk` alone. The class is `low` only when an entry of the risk path map `gates/risk-map.txt` (read at
//! `--from`) covers every changed path as `low` and the ticket's risk is `normal`. The map file itself and
//! any other path under `gates/` are high whatever the map says (built in: no map entry can lower them), a
//! path no entry covers is high (fail closed) and an entry may also say `high`. An override is accepted
//! only when it raises or keeps the class, and is then logged; one that lowers is refused. It decides no
//! review and marks no finding: a caller uses the last line as one input. The map is read from the object
//! store, never from the work tree, so a diff cannot change the map that judges it.
//! Git runs read only: no inherited GIT_ name, no stdin, no replace refs, no lazy fetch, raw tree compare.
//! `--repo-dir` is the top of the repo (a folder inside the work tree is refused) and the map is named from
//! the top.
//! Exit codes: 0 class printed; 1 an override that lowers is refused (one stderr line); 2 usage or setup
//! error (one stderr line, no path). The only write is the append to --log of an accepted override.

use crate::ticket::Ticket;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const USAGE: &str = "usage: harness ticket risk <ticket-file> --repo-dir <absolute-dir> --from <full-sha> --to <full-sha> [--override <low|high> --by <jev|model|human> --reason <text> --log <absolute-file>]";

const MAP_PATH: &str = "gates/risk-map.txt";
const GATES_ROOT: &str = "gates";
const GATES: &str = "gates/";
const MAX_MAP_BYTES: u64 = 1024 * 1024;
const MAX_TICKET_BYTES: u64 = 1024 * 1024;
const MAX_REASON_BYTES: usize = 200;
const MAX_ID_BYTES: usize = 200;
const BY: [&str; 3] = ["jev", "model", "human"];

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {m}");
    2
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Low,
    High,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Class::Low => "low",
            Class::High => "high",
        }
    }

    fn parse(s: &str) -> Option<Class> {
        match s {
            "low" => Some(Class::Low),
            "high" => Some(Class::High),
            _ => None,
        }
    }
}

struct Override {
    class: Class,
    by: String,
    reason: String,
    log: PathBuf,
}

struct Args {
    ticket: String,
    repo: PathBuf,
    from: String,
    to: String,
    over: Option<Override>,
}

fn abs_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

fn sha_ok(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn token_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID_BYTES && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// An override reason: 1 to 200 bytes, not blank, one line, no control character.
fn reason_ok(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_REASON_BYTES && !s.chars().any(char::is_control)
}

/// None: a flag-shape error (usage). The ticket file is the one word that is not a flag; the four override
/// flags are given all or none.
fn parse_args(a: &[String]) -> Option<Args> {
    let (mut ticket, mut repo, mut from, mut to): (Option<&str>, Option<&str>, Option<&str>, Option<&str>) = (None, None, None, None);
    let (mut class, mut by, mut reason, mut log): (Option<&str>, Option<&str>, Option<&str>, Option<&str>) = (None, None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        if arg.starts_with('-') {
            let slot: &mut Option<&str> = match arg.as_str() {
                "--repo-dir" => &mut repo,
                "--from" => &mut from,
                "--to" => &mut to,
                "--override" => &mut class,
                "--by" => &mut by,
                "--reason" => &mut reason,
                "--log" => &mut log,
                _ => return None,
            };
            let v = it.next()?;
            if slot.replace(v.as_str()).is_some() {
                return None;
            }
        } else if arg.is_empty() || ticket.replace(arg.as_str()).is_some() {
            return None;
        }
    }
    let (ticket, repo, from, to) = (ticket?, repo?, from?, to?);
    let over = match (class, by, reason, log) {
        (None, None, None, None) => None,
        (Some(c), Some(b), Some(r), Some(l)) => Some(Override { class: Class::parse(c)?, by: b.to_string(), reason: r.to_string(), log: PathBuf::from(l) }),
        _ => return None,
    };
    if let Some(o) = &over {
        if !BY.contains(&o.by.as_str()) || !reason_ok(&o.reason) || !abs_ok(o.log.to_str()?) {
            return None;
        }
    }
    if !abs_ok(repo) || !sha_ok(from) || !sha_ok(to) {
        return None;
    }
    Some(Args { ticket: ticket.to_string(), repo: PathBuf::from(repo), from: from.to_string(), to: to.to_string(), over })
}

/// One read-only git call on the repo: no inherited GIT_ name, no stdin, no replace refs, no lazy fetch of a
/// missing object (a partial clone fails the call). None: git cannot run.
fn git(repo: &Path, args: &[&str]) -> Option<Output> {
    let mut c = Command::new("git");
    c.arg("--no-replace-objects").arg("--no-lazy-fetch").arg("-C").arg(repo).args(args);
    for (k, _) in std::env::vars_os() {
        if k.as_encoded_bytes().starts_with(b"GIT_") {
            c.env_remove(&k);
        }
    }
    c.stdin(Stdio::null()).output().ok()
}

fn git_ok(repo: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let o = git(repo, args)?;
    o.status.success().then_some(o.stdout)
}

fn read_ticket(file: &str) -> Result<Ticket, String> {
    if !std::fs::metadata(file).map(|m| m.is_file()).unwrap_or(false) {
        return Err("the ticket file is missing or not a plain file".to_string());
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(file).and_then(|f| f.take(MAX_TICKET_BYTES + 1).read_to_end(&mut bytes));
    if read.is_err() {
        return Err("the ticket file cannot be read".to_string());
    }
    if bytes.len() as u64 > MAX_TICKET_BYTES {
        return Err("the ticket file is over 1 MiB".to_string());
    }
    let ticket = Ticket::parse_bytes(&bytes).map_err(|e| format!("the ticket file is refused: {e}"))?;
    if !token_ok(&ticket.id) {
        return Err("the ticket id is not a plain token".to_string());
    }
    Ok(ticket)
}

/// One entry of the risk path map: a plain repo-relative path and its class.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    path: String,
    class: Class,
}

/// A plain path for a map entry: `/`-separated, no empty, `.` or `..` part, no `.git` part, no backslash and
/// no control character. The empty string splits into one empty part, so it is refused with the others.
fn plain_path(p: &str) -> bool {
    !p.chars().any(|c| c == '\\' || c.is_control()) && p.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.eq_ignore_ascii_case(".git"))
}

/// Strict parse of the map: one entry per line `<repo-relative path> <low|high>` with one space; a blank
/// line and a line that starts with `#` are skipped. The Err names the bad line by number, never its text.
fn parse_map(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "is not utf-8".to_string())?;
    let mut out: Vec<Entry> = Vec::new();
    for (n, line) in text.split('\n').enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |what: &str| format!("line {} {what}", n + 1);
        let (path, class) = line.split_once(' ').ok_or_else(|| bad("is malformed"))?;
        let class = Class::parse(class).ok_or_else(|| bad("is malformed"))?;
        if !plain_path(path) {
            return Err(bad("has a path that is not plain"));
        }
        if out.iter().any(|e| e.path == path) {
            return Err(bad("gives one path twice"));
        }
        out.push(Entry { path: path.to_string(), class });
    }
    if out.is_empty() {
        return Err("has no entries".to_string());
    }
    Ok(out)
}

/// The map as it is at `sha`, read from the object store: `<mode> <type> <oid>` from `ls-tree`, then the blob.
/// `--full-tree` names the path from the top of the repo, whatever folder git runs in.
fn map_at(repo: &Path, sha: &str) -> Result<Vec<Entry>, String> {
    let name = format!("the risk map {MAP_PATH} at --from");
    let Some(raw) = git_ok(repo, &["ls-tree", "-z", "--full-tree", sha, "--", MAP_PATH]) else {
        return Err(format!("{name} cannot be read"));
    };
    // `<mode> <type> <oid>` TAB `<path>` NUL: one record for the path. A folder of that name is a `tree`
    // record, refused here because the type must be `blob`.
    let records: Vec<&[u8]> = raw.split(|&b| b == 0).filter(|r| !r.is_empty()).collect();
    let head = records.first().copied().unwrap_or_default();
    let end = head.iter().position(|&b| b == b'\t');
    let meta = std::str::from_utf8(end.map_or(head, |e| &head[..e])).unwrap_or_default();
    let fields: Vec<&str> = meta.split(' ').collect();
    let named = records.len() == 1 && end.is_some_and(|e| std::str::from_utf8(&head[e + 1..]).is_ok_and(|rest| rest == MAP_PATH));
    if fields.len() != 3 || !matches!(fields[0], "100644" | "100755") || fields[1] != "blob" || !named {
        return Err(format!("{name} is missing or not a plain file"));
    }
    let oid = fields[2];
    let Some(size) = git_ok(repo, &["cat-file", "-s", oid]) else {
        return Err("a git call failed".to_string());
    };
    let Some(size) = std::str::from_utf8(&size).ok().and_then(|s| s.trim().parse::<u64>().ok()) else {
        return Err("a git call failed".to_string());
    };
    if size > MAX_MAP_BYTES {
        return Err(format!("{name} is over 1 MiB"));
    }
    let Some(blob) = git_ok(repo, &["cat-file", "blob", oid]) else {
        return Err("a git call failed".to_string());
    };
    parse_map(&blob).map_err(|e| format!("{name} {e}"))
}

/// One changed entry of the raw tree compare.
struct Changed {
    path: Vec<u8>,
}

fn parse_entries(raw: &[u8]) -> Option<Vec<Changed>> {
    let mut toks = raw.split(|&b| b == 0).peekable();
    let mut out = Vec::new();
    while let Some(head) = toks.next() {
        if head.is_empty() && toks.peek().is_none() {
            break;
        }
        let head = std::str::from_utf8(head).ok()?.strip_prefix(':')?;
        if head.split(' ').count() != 5 {
            return None;
        }
        out.push(Changed { path: toks.next()?.to_vec() });
    }
    Some(out)
}

/// Class and why of one changed path, in this order: the map file itself; the `gates` folder and any other path
/// under it; the longest map entry that covers the path (itself, or a folder it is in); no entry.
fn classify(map: &[Entry], path: &str) -> (Class, &'static str) {
    if path == MAP_PATH {
        return (Class::High, "risk-map");
    }
    if path == GATES_ROOT || path.starts_with(GATES) {
        return (Class::High, "gates-path");
    }
    let mut best: Option<&Entry> = None;
    for e in map {
        let hit = path == e.path || (path.starts_with(e.path.as_str()) && path.as_bytes().get(e.path.len()) == Some(&b'/'));
        if hit && best.is_none_or(|b| e.path.len() > b.path.len()) {
            best = Some(e);
        }
    }
    match best {
        None => (Class::High, "unmapped"),
        Some(e) if e.class == Class::High => (Class::High, "mapped-high"),
        Some(_) => (Class::Low, "mapped-low"),
    }
}

/// The whole judgement of one diff. `final_class` and `why` are the summary line; `computed` is the class
/// before any override, the one an override is compared with.
#[derive(Debug, PartialEq, Eq)]
struct Verdict {
    per_path: Vec<(Class, &'static str)>,
    computed: Class,
    final_class: Class,
    why: &'static str,
}

/// Pure judgement. The computed class is high when any path is high or the ticket's risk is high. The
/// summary why is the override's (any accepted override, raised or same), else `ticket-high`, else the why of
/// the first high path in the given order, else `mapped-low`. An override that lowers is read as what it is:
/// the caller refuses it before anything is printed.
fn judge(map: &[Entry], paths: &[&str], ticket_high: bool, over: Option<Class>) -> Verdict {
    let per_path: Vec<(Class, &'static str)> = paths.iter().map(|p| classify(map, p)).collect();
    let (computed, why) = match (per_path.iter().find(|(c, _)| *c == Class::High), ticket_high) {
        (_, true) => (Class::High, "ticket-high"),
        (Some((_, w)), false) => (Class::High, *w),
        (None, false) => (Class::Low, "mapped-low"),
    };
    match over {
        Some(o) if o >= computed => Verdict { per_path, computed, final_class: o, why: "override" },
        _ => Verdict { per_path, computed, final_class: computed, why },
    }
}

/// Appends one line to the log file. A file that exists and does not end with a newline gets one first, so
/// the new line is a whole line. Nothing else of the file is touched. The lookup does not follow a symlink:
/// a link is not a plain file here, also when it points at one.
fn append_log(path: &Path, line: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if !m.is_file() => return Err("--log is not a plain file".to_string()),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err("--log cannot be read".to_string()),
        _ => {}
    }
    let mut f = std::fs::OpenOptions::new().read(true).append(true).create(true).open(path).map_err(|_| "--log cannot be opened".to_string())?;
    let mut tail = [0u8; 1];
    let open_line = f.metadata().map(|m| m.len() > 0).unwrap_or(false)
        && (f.seek(SeekFrom::End(-1)).and_then(|_| f.read_exact(&mut tail)).is_err() || tail[0] != b'\n');
    let mut bytes = Vec::new();
    if open_line {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(line.as_bytes());
    f.write_all(&bytes).and_then(|_| f.sync_all()).map_err(|_| "--log cannot be written".to_string())
}

pub fn risk_cmd(a: &[String]) -> i32 {
    let Some(args) = parse_args(a) else { return usage() };
    let ticket = match read_ticket(&args.ticket) {
        Ok(t) => t,
        Err(m) => return fail(&m),
    };
    let repo = args.repo.as_path();
    if !std::fs::metadata(repo).map(|m| m.is_dir()).unwrap_or(false) {
        return fail("the repo dir is missing or not a folder");
    }
    match git(repo, &["rev-parse", "--git-dir"]) {
        None => return fail("git cannot run"),
        Some(o) if !o.status.success() => return fail("the repo dir is not a git repo"),
        Some(_) => {}
    }
    // `rev-parse --show-prefix` prints the path from the top of the work tree down to the folder (empty at
    // the top). A folder inside the work tree is refused: the changed paths are named from the top.
    match git(repo, &["rev-parse", "--show-prefix"]) {
        None => return fail("git cannot run"),
        Some(o) if !o.status.success() => return fail("a git call failed"),
        Some(o) if o.stdout.iter().any(|b| *b != b'\n') => return fail("--repo-dir is not the top of the repo"),
        Some(_) => {}
    }
    for (flag, sha) in [("--from", &args.from), ("--to", &args.to)] {
        match git(repo, &["cat-file", "-t", sha]) {
            None => return fail("git cannot run"),
            Some(o) if o.status.success() && o.stdout == b"commit\n" => {}
            Some(_) => return fail(&format!("{flag} is not a commit of the repo")),
        }
    }
    if args.from == args.to {
        return fail("--from equals --to");
    }
    let map = match map_at(repo, &args.from) {
        Ok(m) => m,
        Err(m) => return fail(&m),
    };
    let Some(raw) = git_ok(repo, &["diff-tree", "-r", "-z", "--raw", "--no-renames", "--ignore-submodules=none", &args.from, &args.to]) else {
        return fail("a git call failed");
    };
    let Some(entries) = parse_entries(&raw) else { return fail("the tree compare could not be read") };
    let mut paths: Vec<&str> = Vec::with_capacity(entries.len());
    for e in &entries {
        let Some(path) = std::str::from_utf8(&e.path).ok().filter(|p| !p.chars().any(char::is_control)) else {
            return fail("a changed path is not utf-8 or holds a control character");
        };
        paths.push(path);
    }
    let over = args.over.as_ref().map(|o| o.class);
    let v = judge(&map, &paths, ticket.risk == "high", over);
    let Some(o) = &args.over else {
        return print_out(&v, &paths, &args, &ticket.id);
    };
    if o.class < v.computed {
        eprintln!("harness: risk refused: override-lowers computed={} override={}", v.computed.as_str(), o.class.as_str());
        return 1;
    }
    let effect = if o.class > v.computed { "raised" } else { "same" };
    let line = format!(
        "risk-override ticket={} from={} to={} computed={} override={} by={} effect={effect} reason={}\n",
        ticket.id, args.from, args.to, v.computed.as_str(), o.class.as_str(), o.by, o.reason
    );
    if let Err(m) = append_log(&o.log, &line) {
        return fail(&m);
    }
    print_out(&v, &paths, &args, &ticket.id)
}

/// The whole stdout, written once: one line per changed path in git's order, then the summary line.
fn print_out(v: &Verdict, paths: &[&str], args: &Args, id: &str) -> i32 {
    let mut out = String::new();
    for (path, (class, why)) in paths.iter().zip(&v.per_path) {
        out.push_str(&format!("path={path} class={} why={why}\n", class.as_str()));
    }
    out.push_str(&format!("risk={} ticket={id} from={} to={} files={} why={}\n", v.final_class.as_str(), args.from, args.to, paths.len(), v.why));
    let mut so = std::io::stdout().lock();
    match so.write_all(out.as_bytes()).and_then(|_| so.flush()) {
        Ok(()) => 0,
        Err(_) => fail("stdout cannot be written"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(text: &str) -> Vec<Entry> {
        parse_map(text.as_bytes()).expect("the map parses")
    }

    fn entry(path: &str, class: Class) -> Entry {
        Entry { path: path.to_string(), class }
    }

    #[test]
    fn a_map_parses_comments_blanks_and_both_classes() {
        assert_eq!(map("# risk path map\n\ndocs low\n  \nci high\nREADME.md low"), vec![entry("docs", Class::Low), entry("ci", Class::High), entry("README.md", Class::Low)]);
        // A last line with no newline is an entry; a UTF-8 path is plain.
        assert_eq!(map("déjà low"), vec![entry("déjà", Class::Low)]);
    }

    #[test]
    fn a_malformed_map_line_is_refused_naming_the_line() {
        for (text, needle) in [
            ("docs medium\n", "line 1 is malformed"),
            ("../x low\n", "line 1 has a path that is not plain"),
            ("docs low\ndocs high\n", "line 2 gives one path twice"),
            ("/abs low\n", "not plain"),
            ("docs/ low\n", "not plain"),
            ("a/./b low\n", "not plain"),
            ("a/.Git/x low\n", "not plain"),
            ("a\\b low\n", "not plain"),
            ("docs\tlow\n", "malformed"),
            ("docs  low\n", "malformed"),
            ("docs low high\n", "malformed"),
            ("docs low\r\n", "malformed"),
            ("docs\n", "malformed"),
            (" low\n", "not plain"),
            ("#only comments\n\n   \n", "has no entries"),
            ("", "has no entries"),
        ] {
            let err = parse_map(text.as_bytes()).expect_err(text);
            assert!(err.contains(needle), "{text:?}: {err}");
        }
        assert!(parse_map(&[0xff, b'\n']).unwrap_err().contains("utf-8"), "a map that is not UTF-8 is refused");
        // Planted twins: the same lines with the fault removed parse.
        assert_eq!(map("docs high\n"), vec![entry("docs", Class::High)]);
        assert_eq!(map("a/b low\n"), vec![entry("a/b", Class::Low)]);
    }

    #[test]
    fn the_map_file_and_the_gates_folder_are_high_whatever_the_map_says() {
        let m = map("gates low\ngates/risk-map.txt low\ndocs low\ndocs/api high\nci high\n");
        for (path, class, why) in [
            ("gates/risk-map.txt", Class::High, "risk-map"),
            ("gates/x.json", Class::High, "gates-path"),
            ("gates/deep/y.sh", Class::High, "gates-path"),
            ("docs/a.md", Class::Low, "mapped-low"),
            ("docs/api/x.md", Class::High, "mapped-high"),
            ("docs/api", Class::High, "mapped-high"),
            ("docs/apix.md", Class::Low, "mapped-low"),
            ("ci/d.sh", Class::High, "mapped-high"),
            ("newdir/c.txt", Class::High, "unmapped"),
            ("zz.md", Class::High, "unmapped"),
            ("gates", Class::High, "gates-path"),
        ] {
            assert_eq!(classify(&m, path), (class, why), "{path}");
        }
        // Planted twins: a map with only the low entry reads the same `high` path as low.
        assert_eq!(classify(&[entry("ci", Class::Low)], "ci/d.sh"), (Class::Low, "mapped-low"));
        assert_eq!(classify(&[], "docs/a.md"), (Class::High, "unmapped"), "an empty map covers nothing");
    }

    #[test]
    fn the_summary_why_follows_the_order_override_ticket_first_high_path_low() {
        let m = map("docs low\nci high\n");
        let low = ["docs/a.md"];
        let high = ["docs/a.md", "ci/d.sh"];
        assert_eq!(judge(&m, &low, false, None), Verdict { per_path: vec![(Class::Low, "mapped-low")], computed: Class::Low, final_class: Class::Low, why: "mapped-low" });
        assert_eq!(judge(&m, &[], false, None).why, "mapped-low", "an empty diff is low");
        assert_eq!(judge(&m, &high, false, None).why, "mapped-high", "the first high path in order");
        assert_eq!(judge(&m, &low, true, None), Verdict { per_path: vec![(Class::Low, "mapped-low")], computed: Class::High, final_class: Class::High, why: "ticket-high" });
        assert_eq!(judge(&m, &high, true, None).why, "ticket-high", "the ticket's high comes before the first high path");
        assert_eq!(judge(&m, &low, false, Some(Class::High)), Verdict { per_path: vec![(Class::Low, "mapped-low")], computed: Class::Low, final_class: Class::High, why: "override" });
        assert_eq!(judge(&m, &high, false, Some(Class::High)).why, "override", "an override that keeps the class is logged and named");
        assert_eq!(judge(&m, &low, false, Some(Class::Low)).final_class, Class::Low);
        // An override that lowers: computed stays high, so the caller refuses it before printing.
        assert_eq!(judge(&m, &high, false, Some(Class::Low)), Verdict { per_path: vec![(Class::Low, "mapped-low"), (Class::High, "mapped-high")], computed: Class::High, final_class: Class::High, why: "mapped-high" });
    }

    #[test]
    fn the_flag_shapes_that_never_reach_git_are_refused() {
        let ok = |extra: &[&str]| -> Option<Args> {
            let mut v: Vec<String> = ["t.json", "--repo-dir", "/r", "--from", &"a".repeat(40), "--to", &"b".repeat(40)].iter().map(|s| s.to_string()).collect();
            v.extend(extra.iter().map(|s| s.to_string()));
            parse_args(&v)
        };
        assert!(ok(&[]).is_some(), "the bare call parses");
        assert!(ok(&["--override", "high", "--by", "jev", "--reason", "because", "--log", "/l"]).is_some());
        assert!(ok(&["--override", "low", "--by", "human", "--reason", " r ", "--log", "/l"]).is_some(), "a reason keeps its blanks");
        for extra in [
            vec!["--override", "high"],
            vec!["--override", "high", "--by", "jev", "--reason", "r"],
            vec!["--override", "medium", "--by", "jev", "--reason", "r", "--log", "/l"],
            vec!["--override", "high", "--by", "Jev", "--reason", "r", "--log", "/l"],
            vec!["--override", "high", "--by", "other", "--reason", "r", "--log", "/l"],
            vec!["--override", "high", "--by", "jev", "--reason", "", "--log", "/l"],
            vec!["--override", "high", "--by", "jev", "--reason", "   ", "--log", "/l"],
            vec!["--override", "high", "--by", "jev", "--reason", "a\nb", "--log", "/l"],
            vec!["--override", "high", "--by", "jev", "--reason", "a\tb", "--log", "/l"],
            vec!["--override", "high", "--by", "jev", "--reason", &"r".repeat(201), "--log", "/l"],
            vec!["--override", "high", "--by", "jev", "--reason", "r", "--log", "l"],
            vec!["--override", "high", "--by", "jev", "--reason", "r", "--log", "/l/../l"],
        ] {
            assert!(ok(&extra).is_none(), "{extra:?}");
        }
        assert!(ok(&["--override", "high", "--by", "model", "--reason", &"r".repeat(200), "--log", "/l"]).is_some(), "a 200 byte reason is the limit");
    }

    /// A scratch git call (local git only) with the repo names of the caller removed.
    fn scratch_git(dir: &Path, args: &[&str]) -> Output {
        let contact = format!("user.email=t{}scratch", '@');
        let mut c = Command::new("git");
        c.args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str(), "-c", "commit.gpgsign=false"]).arg("-C").arg(dir).args(args);
        for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
            c.env_remove(k);
        }
        c.stdin(Stdio::null()).output().unwrap()
    }

    /// A scratch repo holding one commit of `files`; returns its folder and the sha.
    fn scratch_repo(files: &[(&str, &str)]) -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("risk-unit-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok = |args: &[&str]| assert!(scratch_git(&dir, args).status.success(), "scratch git call failed: {args:?}");
        ok(&["init", "-q"]);
        // Git's own background upkeep off, as for each scratch repo of this crate.
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            ok(&["config", k, v]);
        }
        for (p, text) in files {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        ok(&["add", "-A"]);
        ok(&["commit", "-q", "-m", "m"]);
        let head = scratch_git(&dir, &["rev-parse", "HEAD"]);
        assert!(head.status.success(), "scratch git call failed: rev-parse HEAD");
        (dir, String::from_utf8(head.stdout).unwrap().trim().to_string())
    }

    /// The map read names its path from the top of the repo: run from a folder inside, it reads the one map
    /// at the top and not a file of the same name under that folder.
    #[test]
    fn the_map_read_does_not_depend_on_the_run_folder() {
        let (dir, sha) = scratch_repo(&[("gates/risk-map.txt", "docs low\nci high\n"), ("docs/gates/risk-map.txt", "core low\ngates low\n")]);
        let top = Ok(map("docs low\nci high\n"));
        assert_eq!(map_at(&dir, &sha), top);
        assert_eq!(map_at(&dir.join("docs"), &sha), top, "the run folder does not choose the map");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
