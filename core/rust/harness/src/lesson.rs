//! `harness lesson lint <path> [<path> ...]`: checks the shape of lesson pages (see `docs/lesson-lint.md`).
//! `harness lesson lookup` lists the pages that match a ticket (ticket mode: --ticket and --scope-file) or the
//! committed diff between two commits of one repo (diff mode: --repo-dir, --from, --to; changed paths and added
//! lines); same docs page. A page that carries the front matter key `always` (one rule sentence, no `match`)
//! is listed, when not retired, on an `always:` line in both modes. `harness lesson replay` (lesson_replay.rs) replays the ticket-mode lookup over a
//! folder of tickets and counts the offered pages with the `always` pages apart (an `always` page that is not
//! retired is in no `pages=` list; over the cap below the replay refuses as the lookup does). `harness lesson
//! brief` (lesson_brief.rs) writes the lessons block of a builder brief and the copies of the matched pages. Both modes skip a page with status retired (still checked, counted in scanned, and
//! counted in the receipt field retired=<r>) and refuse an unknown status. The lint
//! checks shape only: it does not check that a sentence is true, that a link resolves, or that an eval result exists.
//! std only; the front matter is parsed by hand (the crate has no dependency).

use crate::fix_tier::{PROVEN_ABSENT, TIERS};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub(crate) const LESSON_USAGE: &str = "usage: harness lesson lint <path> [<path> ...]\n       harness lesson lookup <lessons-dir> --ticket <ticket-file> --scope-file <file>\n       harness lesson lookup <lessons-dir> --repo-dir <absolute-dir> --from <full-sha> --to <full-sha>\n       harness lesson replay <lessons-dir> --tickets <absolute-dir> --scopes <absolute-dir> [--config <absolute-file> --controls <absolute-file> [--rows <absolute-file>]]\n       harness lesson brief <lessons-dir> --ticket <ticket-file> --scope-file <file> --out <absolute-dir>";
/// The read limit of a git answer (the diff).
const MAX_DIFF: usize = 8 * 1024 * 1024;
/// The read limit of the ticket and the scope file (as `lane claim` reads them).
pub(crate) const MAX_FILE: u64 = 1024 * 1024;

/// The seven level-2 headings, in the order they must appear, each once.
const SECTIONS: [&str; 7] = [
    "Issue and symptoms",
    "Context",
    "Root cause",
    "Resolution",
    "Controls",
    "Verification",
    "Effectiveness",
];

/// A real calendar date written `YYYY-MM-DD` (a well-formed date: the month is 1-12, the day fits the month).
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let dig = |r: &[u8]| r.iter().all(|c| c.is_ascii_digit());
    if !dig(&b[0..4]) || !dig(&b[5..7]) || !dig(&b[8..10]) {
        return false;
    }
    let num = |r: &[u8]| std::str::from_utf8(r).unwrap().parse::<u32>().unwrap();
    let (y, m, d) = (num(&b[0..4]), num(&b[5..7]), num(&b[8..10]));
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return false,
    };
    d >= 1 && d <= days
}

/// `ISSUE-<digits>` (at least one digit, digits only).
fn is_issue_id(s: &str) -> bool {
    match s.strip_prefix("ISSUE-") {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// A front matter key: not empty, and no character in it is a blank or a control character (an ASCII
/// space or tab inside the key is refused too).
fn is_key(k: &str) -> bool {
    !k.is_empty() && !k.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A page file name inside a directory argument: `ISSUE-<digits>-<anything>.md`.
fn is_page_name(n: &str) -> bool {
    let Some(rest) = n.strip_prefix("ISSUE-") else { return false };
    let digits = rest.bytes().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && rest[digits..].starts_with('-') && n.ends_with(".md")
}

/// The tokens of an inline list already known to be bracketed (`[a, b]`): split on commas, each trimmed. A list
/// with nothing but blanks between the brackets (`[]`, `[ ]`) has no tokens.
fn split_list(v: &str) -> Vec<&str> {
    let inner = &v[1..v.len() - 1];
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner.split(',').map(|t| t.trim()).collect()
}

/// A control character or a Unicode white-space character other than a plain space is written `\u{hex}`.
pub(crate) fn esc(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() || (c.is_whitespace() && c != ' ') {
            format!("\\u{{{:x}}}", c as u32)
        } else {
            c.to_string()
        })
        .collect()
}

/// Add a page unless an equal one (compared by canonical path) was already added: the same page reached twice in
/// one run (the same file given twice, or a file and the directory that holds it) is checked and counted once.
fn push_page(pages: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, p: PathBuf) {
    let c = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
    if seen.insert(c) {
        pages.push(p);
    }
}

/// Expand the path arguments into page files: a file is one page, a directory means every matching file directly
/// inside it (sorted by name); anything else, an unreadable directory, or nothing found at all is an error. An
/// entry directly inside a directory whose name is `ISSUE-<digits>-<anything>.md` but that cannot be read as a
/// regular file (a broken symbolic link) is an error too; a sub-folder with such a name is skipped.
fn expand(args: &[String]) -> Result<Vec<PathBuf>, String> {
    if args.is_empty() {
        return Err("no path given".to_string());
    }
    let mut pages: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for a in args {
        let p = Path::new(a);
        if p.is_dir() {
            let rd = std::fs::read_dir(p).map_err(|_| format!("the directory {} cannot be read", a))?;
            let mut names: Vec<String> = Vec::new();
            for entry in rd.flatten() {
                let n = entry.file_name().to_string_lossy().to_string();
                if !is_page_name(&n) {
                    continue;
                }
                let ep = entry.path();
                if ep.is_dir() {
                    continue;
                }
                if !ep.is_file() {
                    return Err(format!("the file {} cannot be read", n));
                }
                names.push(n);
            }
            names.sort();
            for n in names {
                push_page(&mut pages, &mut seen, p.join(n));
            }
        } else if p.is_file() {
            push_page(&mut pages, &mut seen, p.to_path_buf());
        } else {
            return Err(format!("the path {} does not exist or cannot be read", a));
        }
    }
    if pages.is_empty() {
        return Err("no lesson pages found".to_string());
    }
    Ok(pages)
}

/// Lint every page. Ok holds the page count and the `<RULE> <detail>` lines (empty when all pages pass); a read
/// error or a file that is not valid UTF-8 is Err, so nothing is printed for a run that cannot be completed.
fn lint(args: &[String]) -> Result<(usize, Vec<String>), String> {
    let pages = expand(args)?;
    let mut out = Vec::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut always_count = 0usize;
    for p in &pages {
        let path = p.to_string_lossy().to_string();
        let file = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
        let bytes = std::fs::read(p).map_err(|_| format!("the file {} cannot be read", path))?;
        let text = String::from_utf8(bytes).map_err(|_| format!("the file {} is not valid UTF-8", path))?;
        for problem in check_page(&text, &file, &mut seen, &mut always_count) {
            out.push(format!("{}: {}", file, problem));
        }
    }
    Ok((pages.len(), out))
}

/// The line index of the closing `---`; else a FRONT-MATTER problem is pushed and the result is None.
pub(crate) fn front_close(lines: &[&str], out: &mut Vec<String>) -> Option<usize> {
    if lines.first() != Some(&"---") {
        out.push("FRONT-MATTER line 1 is not ---".to_string());
        return None;
    }
    let close = lines[1..].iter().position(|&l| l == "---").map(|i| i + 1);
    if close.is_none() {
        out.push("FRONT-MATTER no closing ---".to_string());
    }
    close
}

/// The key/value pairs between the opening and the closing `---`; a bad or repeated key pushes a FRONT-MATTER problem.
pub(crate) fn front_pairs<'a>(lines: &[&'a str], close: usize, out: &mut Vec<String>) -> HashMap<&'a str, &'a str> {
    let mut keys: HashMap<&str, &str> = HashMap::new();
    for (i, &l) in lines[1..close].iter().enumerate() {
        match l.split_once(':') {
            // Only ASCII spaces and tabs are trimmed around the key and the value.
            Some((k, v)) if is_key(k.trim_matches([' ', '\t'])) => {
                let (k, v) = (k.trim_matches([' ', '\t']), v.trim_matches([' ', '\t']));
                if keys.insert(k, v).is_some() {
                    out.push(format!("FRONT-MATTER line {} repeats the key {}", i + 2, k));
                }
            }
            _ => out.push(format!("FRONT-MATTER line {} is not key: value", i + 2)),
        }
    }
    keys
}

/// The tokens of the `match` key: None when the key is absent; Err is the MATCH problem line.
pub(crate) fn match_tokens<'a>(keys: &HashMap<&'a str, &'a str>) -> Result<Option<Vec<&'a str>>, String> {
    let Some(&v) = keys.get("match") else { return Ok(None) };
    if !(v.starts_with('[') && v.ends_with(']')) {
        return Err(format!("MATCH match {} is not an inline list [..]", v));
    }
    let tokens = split_list(v);
    if tokens.is_empty() {
        return Err("MATCH match is an empty list".to_string());
    }
    if tokens.iter().any(|t| t.is_empty()) {
        return Err("MATCH match has an empty token".to_string());
    }
    Ok(Some(tokens))
}

/// The ALWAYS rule of the `always` key: Ok(None) when the key is absent, Ok(Some) with the rule sentence when
/// it passes; Err is the ALWAYS problem line. The value is read as the front matter reader gives it (ASCII
/// spaces and tabs trimmed), retired pages included.
pub(crate) fn always_of<'a>(keys: &HashMap<&'a str, &'a str>) -> Result<Option<&'a str>, String> {
    let Some(&v) = keys.get("always") else { return Ok(None) };
    if v.is_empty() {
        return Err("ALWAYS always is empty".to_string());
    }
    if v.len() > 200 {
        return Err("ALWAYS always is over 200 bytes".to_string());
    }
    if v.chars().any(|c| c.is_control() || (c.is_whitespace() && c != ' ')) {
        return Err("ALWAYS always holds a control character or a blank other than a space".to_string());
    }
    if keys.contains_key("match") {
        return Err("ALWAYS always and match on one page".to_string());
    }
    Ok(Some(v))
}

/// The ALWAYS-CAP refusal shared by the lookup and the replay: `count` is the number of pages read that carry
/// the key `always` and are not retired (a refused value included); over 5 the run is refused with this line,
/// else None.
pub(crate) fn always_cap(count: usize) -> Option<String> {
    (count > 5).then(|| format!("harness: lesson lookup refused: ALWAYS-CAP {} pages carry always, at most 5", count))
}

/// The status of a page: Ok(None) when the key is absent; Err is the STATUS problem line of an unknown value.
pub(crate) fn status_of<'a>(keys: &HashMap<&'a str, &'a str>) -> Result<Option<&'a str>, String> {
    match keys.get("status").copied() {
        None => Ok(None),
        Some(v @ ("unvalidated" | "validated" | "retired")) => Ok(Some(v)),
        Some(v) => Err(format!("STATUS status {} is not unvalidated, validated or retired", v)),
    }
}

/// Check one page (`file` is its file name, not its path) and return its `<RULE> <detail>` lines. `seen` maps
/// an id to the first file that carried it; `always_count` is raised by each page that carries the key `always`
/// and is not retired (for ALWAYS-CAP, counted in check order, a page reached twice counts once).
fn check_page(text: &str, file: &str, seen: &mut HashMap<String, String>, always_count: &mut usize) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    // FRONT-MATTER: line 1 opens, a later `---` closes; without a well-formed block nothing later can be checked.
    let Some(close) = front_close(&lines, &mut out) else { return out };
    let keys = front_pairs(&lines, close, &mut out);
    // SHARED-FIELD
    for k in ["kind", "updated", "sources", "verified"] {
        if !keys.contains_key(k) {
            out.push(format!("SHARED-FIELD {} is missing", k));
        }
    }
    if let Some(&v) = keys.get("updated") {
        if !is_date(v) {
            out.push(format!("SHARED-FIELD updated {} is not a date YYYY-MM-DD", v));
        }
    }
    if let Some(&v) = keys.get("verified") {
        if v != "yes" && v != "no" {
            out.push(format!("SHARED-FIELD verified {} is not yes or no", v));
        }
    }
    // TYPE
    match keys.get("type") {
        None => out.push("TYPE type is missing".to_string()),
        Some(&"lesson") => {}
        Some(&v) => out.push(format!("TYPE type is {} and must be lesson", v)),
    }
    // ID (the heading prefix check later needs an ISSUE-shaped id, so it is kept separate from the name check)
    let mut id: Option<&str> = None;
    match keys.get("id") {
        None => out.push("ID id is missing".to_string()),
        Some(&v) => {
            if !is_issue_id(v) {
                out.push(format!("ID id {} is not ISSUE-<digits>", v));
            } else {
                if !file.starts_with(&format!("{}-", v)) {
                    out.push(format!("ID the file name does not start with {}-", v));
                }
                id = Some(v);
            }
        }
    }
    // STATUS
    let mut status: Option<&str> = None;
    match status_of(&keys) {
        Ok(None) => out.push("STATUS status is missing".to_string()),
        Ok(s) => status = s,
        Err(p) => out.push(p),
    }
    // TIER (the six values the G8.3 reader accepts for this field, docs/fix-tier.md)
    match keys.get("minimum_proven_tier") {
        None => out.push("TIER minimum_proven_tier is missing".to_string()),
        Some(&"") => out.push("TIER minimum_proven_tier is empty".to_string()),
        Some(&v) => {
            if !TIERS.contains(&v) && !PROVEN_ABSENT.contains(&v) {
                out.push(format!(
                    "TIER minimum_proven_tier {} is not one of T1, T2, T3, T4, unproven, unmeasured",
                    v
                ));
            }
        }
    }
    // CONTROLS
    let mut controls: Option<Vec<&str>> = None;
    match keys.get("controls") {
        None => out.push("CONTROLS controls is missing".to_string()),
        Some(&v) => {
            if v.starts_with('[') && v.ends_with(']') {
                let tokens = split_list(v);
                if tokens.iter().any(|t| t.is_empty()) {
                    out.push("CONTROLS controls has an empty token".to_string());
                }
                controls = Some(tokens);
            } else {
                out.push(format!("CONTROLS controls {} is not an inline list [..]", v));
            }
        }
    }
    // MATCH
    if let Err(p) = match_tokens(&keys) {
        out.push(p);
    }
    // ALWAYS, then ALWAYS-CAP (the cap counts pages that are not retired, a refused value included)
    if keys.contains_key("always") {
        if let Err(p) = always_of(&keys) {
            out.push(p);
        }
        if status != Some("retired") {
            *always_count += 1;
            if *always_count > 5 {
                out.push("ALWAYS-CAP more than 5 pages carry always".to_string());
            }
        }
    }
    // DUPLICATE-ID (any two pages of one run with the same id value)
    if let Some(&v) = keys.get("id") {
        match seen.get(v) {
            Some(first) => out.push(format!("DUPLICATE-ID id {} also in {}", v, first)),
            None => {
                seen.insert(v.to_string(), file.to_string());
            }
        }
    }
    // The body: everything after the closing `---`. A line that starts with three backticks toggles a code fence;
    // a line inside a fence is never a heading.
    let body = &lines[close + 1..];
    let mut h1: Vec<&str> = Vec::new();
    let mut h2: Vec<(String, usize)> = Vec::new();
    let mut heads: Vec<usize> = Vec::new();
    let mut fence = false;
    for (i, &l) in body.iter().enumerate() {
        if l.starts_with("```") {
            fence = !fence;
            continue;
        }
        if fence {
            continue;
        }
        if l.starts_with("# ") {
            h1.push(l);
            heads.push(i);
        } else if l.starts_with("## ") {
            h2.push((l[2..].trim().to_string(), i));
            heads.push(i);
        }
    }
    // TITLE
    if h1.len() != 1 {
        out.push(format!("TITLE the body has {} level-1 headings and needs exactly one", h1.len()));
    } else if let Some(id) = id {
        let prefix = format!("# {}:", id);
        if !h1[0].starts_with(&prefix) {
            out.push(format!("TITLE the level-1 heading does not start with {}", prefix));
        }
    }
    // SECTIONS
    let actual: Vec<&str> = h2.iter().map(|(t, _)| t.as_str()).collect();
    for e in SECTIONS {
        let n = actual.iter().filter(|&a| *a == e).count();
        match n {
            0 => out.push(format!("SECTIONS the heading {} is missing", e)),
            1 => {}
            n => out.push(format!("SECTIONS the heading {} appears {} times", e, n)),
        }
    }
    let mut extras: Vec<&str> = Vec::new();
    for &a in &actual {
        if !SECTIONS.contains(&a) && !extras.contains(&a) {
            extras.push(a);
        }
    }
    for a in extras {
        out.push(format!("SECTIONS the heading {} is not one of the seven", a));
    }
    let mut last: Option<usize> = None;
    for &a in &actual {
        if let Some(p) = SECTIONS.iter().position(|&e| e == a) {
            if last.is_some_and(|l| p <= l) {
                out.push(format!("SECTIONS the heading {} is out of order", a));
                break;
            }
            last = Some(p);
        }
    }
    // The lines of one section: after its heading, up to the next level-1 or level-2 heading or the end of the body.
    let section = |name: &str| -> Option<&[&str]> {
        let &(_, start) = h2.iter().find(|(t, _)| t == name)?;
        let end = heads.iter().copied().find(|&h| h > start).unwrap_or(body.len());
        Some(&body[start + 1..end])
    };
    for (t, start) in &h2 {
        let end = heads.iter().copied().find(|&h| h > *start).unwrap_or(body.len());
        // A code-fence line (a line that starts with three backticks) is not section text.
        if body[*start + 1..end].iter().all(|l| l.trim().is_empty() || l.starts_with("```")) {
            out.push(format!("SECTIONS the heading {} has no text", t));
        }
    }
    // ROOT-CAUSE
    if let (Some(a), Some(b)) = (section("Issue and symptoms"), section("Root cause")) {
        if a.join("\n").trim() == b.join("\n").trim() {
            out.push("ROOT-CAUSE the Root cause section repeats Issue and symptoms".to_string());
        }
    }
    // EFFECTIVENESS
    if let Some(sec) = section("Effectiveness") {
        let lns: Vec<&str> = sec.iter().copied().filter(|l| !l.trim().is_empty()).collect();
        let wanted = [("- metric:", false), ("- value:", false), ("- date:", true), ("- source:", false)];
        if lns.len() != 4 {
            out.push("EFFECTIVENESS the section must hold the four lines - metric:, - value:, - date:, - source:".to_string());
        } else {
            for (i, (prefix, dated)) in wanted.iter().enumerate() {
                let Some(v) = lns[i].strip_prefix(prefix) else {
                    out.push(format!("EFFECTIVENESS line {} must start with {}", i + 1, prefix));
                    continue;
                };
                let v = v.trim();
                if v.is_empty() {
                    out.push(format!("EFFECTIVENESS line {} has an empty value", i + 1));
                } else if *dated && !is_date(v) {
                    out.push(format!("EFFECTIVENESS line {} holds {} which is not a date YYYY-MM-DD", i + 1, v));
                }
            }
        }
    }
    // VALIDATED-CONTROL
    if status == Some("validated") {
        let listed = controls.as_ref().is_some_and(|c| !c.is_empty());
        let words = section("Controls").is_some_and(|s| s.join(" ").contains("no reusable control"));
        if !listed && !words {
            out.push("VALIDATED-CONTROL status is validated but controls is empty and the Controls section does not say no reusable control".to_string());
        }
    }
    out
}

/// `harness lesson lint <path> [<path> ...]`. Exit 0 all pages pass (one stdout line); 1 one stdout line per problem;
/// 2 usage or read error (nothing on stdout). `lookup` goes to `lookup_cmd`, `replay` to `lesson_replay::replay_cmd`,
/// `brief` to `lesson_brief::brief_cmd`. Any verb but `lint`, `lookup`, `replay` and `brief`, or no path, prints
/// the usage and exits 2.
pub fn lesson_cmd(a: &[String]) -> i32 {
    if a.first().map(|s| s.as_str()) == Some("lookup") {
        return lookup_cmd(&a[1..]);
    }
    if a.first().map(|s| s.as_str()) == Some("replay") {
        return crate::lesson_replay::replay_cmd(&a[1..]);
    }
    if a.first().map(|s| s.as_str()) == Some("brief") {
        return crate::lesson_brief::brief_cmd(&a[1..]);
    }
    if a.first().map(|s| s.as_str()) != Some("lint") || a.len() < 2 {
        eprintln!("{}", LESSON_USAGE);
        return 2;
    }
    match lint(&a[1..]) {
        Ok((n, problems)) if problems.is_empty() => {
            println!("lesson-lint ok pages={}", n);
            0
        }
        Ok((_, problems)) => {
            for line in problems {
                println!("{}", esc(&line));
            }
            1
        }
        Err(m) => {
            eprintln!("harness: error: {}", esc(&m));
            2
        }
    }
}

/// A lookup page name: `ISSUE-` and exactly three digits, then `-`, anything, `.md`.
fn is_lookup_page(n: &str) -> bool {
    let Some(rest) = n.strip_prefix("ISSUE-") else { return false };
    let b = rest.as_bytes();
    b.len() >= 7 && b[..3].iter().all(|c| c.is_ascii_digit()) && b[3] == b'-' && n.ends_with(".md")
}

/// What a directory entry name is to the lookup.
#[derive(Debug, PartialEq)]
enum NameKind {
    /// Not `ISSUE-<digits>-<anything>.md`: skipped.
    NotPage,
    /// Page-shaped and a good page name.
    Page,
    /// Page-shaped (by its lossy form) but not valid UTF-8.
    NotUtf8,
    /// Page-shaped but not exactly 3 digits.
    Digits,
}

/// Checks (a) and (b) of the lookup walk, over the raw name.
fn name_kind(n: &OsStr) -> NameKind {
    let lossy = n.to_string_lossy();
    if !is_page_name(&lossy) {
        NameKind::NotPage
    } else if n.to_str().is_none() {
        NameKind::NotUtf8
    } else if !is_lookup_page(&lossy) {
        NameKind::Digits
    } else {
        NameKind::Page
    }
}

/// The pages of the lookup: regular files directly inside `dir`, by file name in byte order. Err holds the
/// messages for exit 2: one, or one per page-shaped entry that is not a page and not a folder (raw name byte order).
pub(crate) fn walk(dir: &str) -> Result<Vec<(String, PathBuf)>, Vec<String>> {
    let one = |m: &str| vec![m.to_string()];
    let p = Path::new(dir);
    let md = std::fs::metadata(p).map_err(|_| one("the lessons folder does not exist or cannot be read"))?;
    if !md.is_dir() {
        return Err(one("the lessons argument is a file, not a folder"));
    }
    let rd = std::fs::read_dir(p).map_err(|_| one("the lessons folder cannot be read"))?;
    let mut pages: Vec<(String, PathBuf)> = Vec::new();
    let mut bad: Vec<(OsString, String)> = Vec::new();
    for ent in rd {
        let ent = ent.map_err(|_| one("the lessons folder cannot be read"))?;
        let raw = ent.file_name();
        let kind = name_kind(&raw);
        if kind == NameKind::NotPage {
            continue;
        }
        let n = esc(&raw.to_string_lossy());
        let ft = ent.file_type().map_err(|_| vec![format!("the entry {} cannot be inspected", n)])?;
        if ft.is_dir() {
            continue;
        }
        let why = if kind == NameKind::NotUtf8 {
            "has a name that is not valid UTF-8"
        } else if kind == NameKind::Digits {
            "does not have exactly 3 digits after ISSUE-"
        } else if ft.is_symlink() {
            "is a symbolic link"
        } else if !ft.is_file() {
            "is not a regular file"
        } else {
            let Ok(s) = raw.into_string() else { continue };
            pages.push((s, ent.path()));
            continue;
        };
        bad.push((raw, format!("the entry {} {}", n, why)));
    }
    if !bad.is_empty() {
        bad.sort();
        return Err(bad.into_iter().map(|(_, m)| m).collect());
    }
    if pages.is_empty() {
        return Err(one("the lessons folder holds no lesson page"));
    }
    pages.sort();
    Ok(pages)
}

/// A file read to the limit plus one byte; Err is a message for exit 2.
fn read_limited(path: &str, what: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut b = Vec::new();
    std::fs::File::open(path).and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut b)).map_err(|_| format!("the {} cannot be read", what))?;
    Ok(b)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Does `tok` occur in `hay`, letter case ignored per character, with no word character directly before or after?
fn word_in(tok: &str, hay: &str) -> bool {
    let (t, h): (Vec<char>, Vec<char>) = (tok.chars().collect(), hay.chars().collect());
    if t.is_empty() || h.len() < t.len() {
        return false;
    }
    (0..=h.len() - t.len()).any(|i| {
        let same = t.iter().enumerate().all(|(j, c)| h[i + j].to_lowercase().eq(c.to_lowercase()));
        same && (i == 0 || !is_word(h[i - 1])) && (i + t.len() == h.len() || !is_word(h[i + t.len()]))
    })
}

fn path_parts(s: &str) -> Vec<&str> {
    s.split('/').filter(|p| !p.is_empty()).collect()
}

/// One token hits by repo, by path (whole leading parts of a scope entry) or by word (title or a criterion).
pub(crate) fn token_hits(tok: &str, t: &crate::ticket::Ticket, scope: &[String]) -> bool {
    if tok == t.repo_id.as_str() {
        return true;
    }
    let tp = path_parts(tok);
    if !tp.is_empty() && scope.iter().any(|e| path_parts(e).starts_with(&tp)) {
        return true;
    }
    word_in(tok, &t.title) || t.acceptance_criteria.iter().any(|c| word_in(tok, c))
}

/// The heading of a page: text after `# ` of the first such line below the front matter, else the name between the id and `.md`.
fn heading(lines: &[&str], close: usize, name: &str) -> String {
    match lines[close + 1..].iter().find_map(|l| l.strip_prefix("# ")).map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => name[10..name.len() - 3].replace('-', " "),
    }
}

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a diff gives the lookup: the changed paths and the added lines (each with its lower-case form).
pub(crate) struct Diff {
    paths: Vec<String>,
    lines: Vec<(String, String)>,
}

/// One token hits a diff: its path parts lead a changed path, or it is a word of an added line.
fn diff_hits(tok: &str, d: &Diff) -> bool {
    let tp = path_parts(tok);
    if !tp.is_empty() && d.paths.iter().any(|p| path_parts(p).starts_with(&tp)) {
        return true;
    }
    // A cheap pre-check: the lower case of each character is joined, as `word_in` compares them.
    let low = lower(tok);
    d.lines.iter().any(|(l, orig)| l.contains(&low) && word_in(tok, orig))
}

/// The lower case of each character, one by one (not of the whole string, which reads a word-final sigma
/// differently).
fn lower(s: &str) -> String {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// A git command in the repo: no inherited GIT_ name, no replace refs, no lazy fetch, no color, no external diff.
fn git_cmd(repo: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    c.arg("--no-replace-objects").arg("--no-lazy-fetch").arg("-C").arg(repo);
    c.args(["-c", "color.ui=false", "-c", "diff.relative=false", "-c", "diff.external=", "-c", "core.fsmonitor=false"]);
    c.args(["-c", "diff.interHunkContext=0", "-c", "diff.suppressBlankEmpty=false"]);
    for (k, _) in std::env::vars_os() {
        if k.as_encoded_bytes().starts_with(b"GIT_") {
            c.env_remove(&k);
        }
    }
    c
}

/// One read-only git call: no stdin, no stderr. The answer is read to `limit` bytes; Err(true) is an answer
/// over it, Err(false) a call that cannot run or fails.
fn git_read(repo: &Path, args: &[&str], limit: usize) -> Result<Vec<u8>, bool> {
    use std::io::Read;
    use std::process::Stdio;
    let mut c = git_cmd(repo);
    c.args(args);
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|_| false)?;
    let mut buf = Vec::new();
    let read = child.stdout.take().map(|o| o.take(limit as u64 + 1).read_to_end(&mut buf));
    if buf.len() > limit {
        let _ = child.kill();
        let _ = child.wait();
        return Err(true);
    }
    let st = child.wait().map_err(|_| false)?;
    if read.is_some_and(|r| r.is_err()) || !st.success() {
        return Err(false);
    }
    Ok(buf)
}

/// The changed paths of a raw `-z` tree compare. None: the bytes are not a raw compare.
fn raw_paths(raw: &[u8]) -> Option<Vec<String>> {
    let mut toks = raw.split(|&b| b == 0);
    let mut out = Vec::new();
    while let Some(head) = toks.next() {
        if head.is_empty() && toks.next().is_none() {
            break;
        }
        if !head.starts_with(b":") {
            return None;
        }
        out.push(String::from_utf8_lossy(toks.next()?).to_string());
    }
    Some(out)
}

/// The blob ids (not all zero) of the `index` lines of a full-index patch. A hunk line never starts with `index `.
fn index_ids(l: &[u8]) -> Vec<String> {
    let Some(r) = l.strip_prefix(b"index ") else { return Vec::new() };
    let Ok(r) = std::str::from_utf8(r) else { return Vec::new() };
    let ids = r.split(' ').next().unwrap_or("");
    ids.split("..").filter(|s| !s.is_empty() && s.bytes().any(|b| b != b'0')).map(str::to_string).collect()
}

/// Which blobs hold a NUL byte in their first 8000 bytes: the default rule of git for a binary file, read here so
/// that no attribute, size or user setting decides it. A missing object or a non-blob reads as text.
fn nul_blobs(repo: &Path, ids: &[String]) -> Result<HashMap<String, bool>, ()> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;
    let mut res = HashMap::new();
    if ids.is_empty() {
        return Ok(res);
    }
    let mut c = git_cmd(repo);
    c.args(["cat-file", "--batch"]);
    let mut child = c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|_| ())?;
    let (Some(mut sin), Some(sout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(());
    };
    let input: Vec<u8> = ids.iter().flat_map(|s| format!("{}\n", s).into_bytes()).collect();
    let writer = std::thread::spawn(move || {
        let _ = sin.write_all(&input);
    });
    let mut r = BufReader::new(sout);
    let mut read_all = || -> Result<(), ()> {
        for id in ids {
            let mut head = String::new();
            if r.read_line(&mut head).map_err(|_| ())? == 0 {
                return Err(());
            }
            let f: Vec<&str> = head.trim_end().split(' ').collect();
            let nul = match f.as_slice() {
                [_, "missing"] => false,
                [_, kind, size] => {
                    let size: u64 = size.parse().map_err(|_| ())?;
                    let mut first = Vec::new();
                    r.by_ref().take(size.min(8000)).read_to_end(&mut first).map_err(|_| ())?;
                    // the rest of the object and the newline after it
                    std::io::copy(&mut r.by_ref().take(size - first.len() as u64 + 1), &mut std::io::sink()).map_err(|_| ())?;
                    *kind == "blob" && first.contains(&0)
                }
                _ => return Err(()),
            };
            res.insert(id.clone(), nul);
        }
        Ok(())
    };
    let ok = read_all();
    if ok.is_err() {
        let _ = child.kill();
    }
    let _ = writer.join();
    let st = child.wait();
    if ok.is_err() || !st.is_ok_and(|s| s.success()) {
        return Err(());
    }
    Ok(res)
}

/// The added lines of a `-U0` patch, read by the hunk counts so a line that only looks like a file header is
/// still a line. A gitlink change and a file with a NUL byte (`bin`) have no line read. None: the patch cannot be read.
fn added_lines(patch: &[u8], bin: &HashMap<String, bool>) -> Option<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut gitlink = false;
    let mut binary = false;
    let mut it = patch.split(|&b| b == b'\n');
    while let Some(l) = it.next() {
        if l.starts_with(b"diff --git ") {
            gitlink = false;
            binary = false;
        } else if l.starts_with(b"index ") && !l.ends_with(b" 160000") {
            binary = index_ids(l).iter().any(|i| bin.get(i) == Some(&true));
        } else if l.starts_with(b"new file mode 160000")
            || l.starts_with(b"deleted file mode 160000")
            || l.starts_with(b"new mode 160000")
            || l.starts_with(b"old mode 160000")
            || (l.starts_with(b"index ") && l.ends_with(b" 160000"))
        {
            gitlink = true;
        } else if l.starts_with(b"@@ ") {
            let h = std::str::from_utf8(l).ok()?;
            let mut f = h.split(' ');
            let count = |s: Option<&str>, c: char| -> Option<usize> {
                let s = s?.strip_prefix(c)?;
                match s.split_once(',') {
                    Some((_, n)) => n.parse().ok(),
                    None => Some(1),
                }
            };
            f.next();
            let (mut old, mut new) = (count(f.next(), '-')?, count(f.next(), '+')?);
            while old > 0 || new > 0 {
                let l = it.next()?;
                match l.first() {
                    Some(b'-') if old > 0 => old -= 1,
                    Some(b'+') if new > 0 => {
                        new -= 1;
                        if !gitlink && !binary {
                            let t = String::from_utf8_lossy(&l[1..]);
                            let t = t.strip_suffix('\r').unwrap_or(&t).to_string();
                            out.push((lower(&t), t));
                        }
                    }
                    // a context line (a setting can ask for one)
                    Some(b' ') | None if old > 0 && new > 0 => {
                        old -= 1;
                        new -= 1;
                    }
                    Some(b'\\') => {}
                    _ => return None,
                }
            }
        }
    }
    Some(out)
}

/// The git checks and the diff read of diff mode; Err is the one message line for exit 2.
fn read_diff(repo: &Path, from: &str, to: &str) -> Result<Diff, String> {
    if !std::fs::metadata(repo).map(|m| m.is_dir()).unwrap_or(false) {
        return Err("the repo dir is missing or not a folder".to_string());
    }
    let big = "the diff is over 8 MiB";
    let small = |args: &[&str]| git_read(repo, args, MAX_FILE as usize).map_err(|_| "a git call failed".to_string());
    let Ok(top) = git_read(repo, &["rev-parse", "--show-toplevel"], 4096) else {
        return Err("the repo dir is not a git repo".to_string());
    };
    let top = String::from_utf8(top).unwrap_or_default();
    match (std::fs::canonicalize(repo), std::fs::canonicalize(top.trim_end_matches('\n'))) {
        (Ok(a), Ok(b)) if a == b => {}
        _ => return Err("the repo dir is not the top folder of a git work tree".to_string()),
    }
    for (flag, sha) in [("--from", from), ("--to", to)] {
        if !matches!(small(&["cat-file", "-t", sha]), Ok(o) if o == b"commit\n") {
            return Err(format!("{} is not a commit of the repo", flag));
        }
    }
    if from == to {
        return Err("--from and --to are the same commit".to_string());
    }
    if git_read(repo, &["merge-base", "--is-ancestor", from, to], 4096).is_err() {
        return Err("--from is not an ancestor of --to".to_string());
    }
    let tree = ["--no-renames", "--ignore-submodules=none"];
    let mut raw = vec!["diff-tree", "-r", "-z", "--raw"];
    raw.extend(tree);
    raw.extend([from, to]);
    let raw = git_read(repo, &raw, MAX_DIFF).map_err(|o| if o { big.to_string() } else { "a git call failed".to_string() })?;
    let paths = raw_paths(&raw).ok_or("the tree compare could not be read")?;
    // --text: no attribute or size setting decides which file is read; a NUL byte rule below does.
    let mut patch = vec!["diff-tree", "-p", "-r", "-U0", "--text", "--full-index", "--diff-algorithm=myers"];
    patch.extend(["--no-color", "--no-ext-diff", "--no-textconv", "--submodule=short"]);
    patch.extend(tree);
    patch.extend([from, to]);
    let patch = git_read(repo, &patch, MAX_DIFF).map_err(|o| if o { big.to_string() } else { "a git call failed".to_string() })?;
    let mut ids: Vec<String> = patch.split(|&b| b == b'\n').flat_map(index_ids).collect();
    ids.sort();
    ids.dedup();
    let bin = nul_blobs(repo, &ids).map_err(|_| "a git call failed")?;
    let lines = added_lines(&patch, &bin).ok_or("the diff could not be read")?;
    Ok(Diff { paths, lines })
}

/// What the lookup reads from one page: the closing front matter line, the retired flag, the match tokens
/// (quotes trimmed), the `always` rule sentence (None when the key is absent or its value is refused),
/// whether the `always` key is present (a refused value included, for the cap) and the problems found.
pub(crate) struct PageRead {
    pub close: Option<usize>,
    pub retired: bool,
    pub tokens: Vec<String>,
    pub always: Option<String>,
    pub has_always: bool,
    pub problems: Vec<String>,
}

/// Read one page (its lines, line ends cut) as the lookup does; shared by `lookup` and `replay`.
pub(crate) fn read_page(lines: &[&str]) -> PageRead {
    let mut problems = Vec::new();
    let close = front_close(lines, &mut problems);
    let mut retired = false;
    let mut always = None;
    let mut has_always = false;
    let tokens = match close {
        Some(close) => {
            let keys = front_pairs(lines, close, &mut problems);
            // STATUS before MATCH before ALWAYS, as the lint orders them; a missing key reads as active.
            match status_of(&keys) {
                Ok(s) => retired = s == Some("retired"),
                Err(p) => problems.push(p),
            }
            has_always = keys.contains_key("always");
            let t = match match_tokens(&keys) {
                Ok(t) => t,
                Err(p) => {
                    problems.push(p);
                    None
                }
            };
            match always_of(&keys) {
                Ok(a) => always = a.map(str::to_string),
                Err(p) => problems.push(p),
            }
            t
        }
        None => None,
    };
    let tokens: Vec<String> = tokens.unwrap_or_default().into_iter().map(|t| t.trim_matches(['"', '\'']).to_string()).collect();
    if tokens.iter().any(|t| t.is_empty()) {
        problems.push("MATCH match has a token with no text between its quotes".to_string());
    }
    PageRead { close, retired, tokens, always, has_always, problems }
}

/// Read the ticket file and the scope file of ticket mode, as `lookup_cmd` does (the ticket to the reader limit,
/// the scope file as `lane claim` reads it); shared with `lesson_brief`. Err holds the exit code (2) with its
/// stderr lines.
pub(crate) fn read_ticket_scope(ticket: &str, scope: &str) -> Result<Input, (i32, Vec<String>)> {
    let one = |m: String| (2, vec![format!("harness: error: {}", esc(&m))]);
    let ticket_bytes = match read_limited(ticket, "ticket file") {
        Ok(b) => b,
        Err(m) => return Err(one(m)),
    };
    let scope = match read_limited(scope, "scope file") {
        Ok(b) if b.len() as u64 > MAX_FILE => return Err(one("the scope file is over 1 MiB".to_string())),
        Ok(b) => match String::from_utf8(b) {
            Ok(t) => crate::lane::parse_scope(&t).map_err(|m| one(format!("scope file: {}", m))),
            Err(_) => return Err(one("the scope file is not valid UTF-8".to_string())),
        },
        Err(m) => return Err(one(m)),
    }?;
    Ok(Input::Ticket(ticket_bytes, scope))
}

/// `harness lesson lookup <lessons-dir> --ticket <ticket-file> --scope-file <file>`, or `--repo-dir <dir> --from <sha> --to <sha>` (diff mode). Exit 0 the matching pages (stdout), 1 a page or the ticket is
/// refused, 2 usage or read error or a page-shaped entry that is not a page and not a folder. The exit 1 lines are printed after all pages are read.
fn lookup_cmd(a: &[String]) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", esc(m));
        2
    };
    let (mut dir, mut ticket, mut scope) = (None, None, None);
    let (mut repo, mut from, mut to) = (None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--ticket" => &mut ticket,
            "--scope-file" => &mut scope,
            "--repo-dir" => &mut repo,
            "--from" => &mut from,
            "--to" => &mut to,
            s if s.starts_with("--") => return fail(&format!("unknown option {}", s)),
            _ => {
                if dir.replace(arg).is_some() {
                    return fail("more than one lessons folder given");
                }
                continue;
            }
        };
        let Some(v) = it.next() else { return fail(&format!("{} needs a value", arg)) };
        if slot.replace(v).is_some() {
            return fail(&format!("{} given twice", arg));
        }
    }
    let Some(dir) = dir else {
        eprintln!("{}", LESSON_USAGE);
        return 2;
    };
    let input = if repo.is_some() || from.is_some() || to.is_some() {
        if ticket.is_some() || scope.is_some() {
            eprintln!("{}", LESSON_USAGE);
            return 2;
        }
        let (Some(repo), Some(from), Some(to)) = (repo, from, to) else {
            eprintln!("{}", LESSON_USAGE);
            return 2;
        };
        let p = Path::new(repo.as_str());
        if !p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) || !sha_ok(from) || !sha_ok(to) {
            eprintln!("{}", LESSON_USAGE);
            return 2;
        }
        match read_diff(p, from, to) {
            Ok(d) => Input::Diff(d),
            Err(m) => return fail(&m),
        }
    } else {
        let Some(ticket) = ticket else { return fail("--ticket is required") };
        let Some(scope) = scope else { return fail("--scope-file is required") };
        match read_ticket_scope(ticket, scope) {
            Ok(i) => i,
            Err((c, ls)) => {
                for l in ls {
                    eprintln!("{}", l);
                }
                return c;
            }
        }
    };
    match match_pages(dir, &input) {
        Ok(answer) => {
            for p in &answer.pages {
                println!("{}", p.line);
            }
            println!("{}", answer.receipt);
            0
        }
        Err((code, lines)) => {
            for l in lines {
                eprintln!("{}", l);
            }
            code
        }
    }
}

/// What the lookup matches the tokens against: the ticket bytes with its scope, or the diff.
pub(crate) enum Input {
    Ticket(Vec<u8>, Vec<String>),
    Diff(Diff),
}

/// One listed page of the lookup: its printed line, its id (the `ISSUE-<3 digits>` start of the file name), its
/// file name, whether it is an `always:` page, and — a matched page only — the page text as it was read.
pub(crate) struct ListedPage {
    pub line: String,
    pub id: String,
    pub name: String,
    pub always: bool,
    pub text: Option<String>,
}

/// The exit 0 answer of the page loop: the listed pages in print order (the `always:` pages, then the `lesson:`
/// pages, each group in byte order of the file names) and the receipt line.
pub(crate) struct PagesAnswer {
    pub pages: Vec<ListedPage>,
    pub receipt: String,
}

/// The one loop over the pages of a lessons folder, for both modes. Takes the folder and the input. Ok holds
/// the listed pages and the receipt line of exit 0; Err holds the exit code (1 a page or the ticket is refused,
/// 2 a read error or an entry the walk refuses) with its stderr lines in print order.
pub(crate) fn match_pages(dir: &str, input: &Input) -> Result<PagesAnswer, (i32, Vec<String>)> {
    let fail = |m: &str| (2, vec![format!("harness: error: {}", esc(m))]);
    let names = match walk(dir) {
        Ok(n) => n,
        Err(ms) => return Err((2, ms.iter().flat_map(|m| fail(m).1).collect())),
    };
    let mut texts: Vec<String> = Vec::new();
    for (n, p) in &names {
        let b = match std::fs::read(p) {
            Ok(b) => b,
            Err(_) => return Err(fail(&format!("the file {} cannot be read", n))),
        };
        match String::from_utf8(b) {
            Ok(t) => texts.push(t),
            Err(_) => return Err(fail(&format!("the file {} is not valid UTF-8", n))),
        }
    }
    let mut refused: Vec<String> = Vec::new();
    let mut ticket = None;
    let mut diff_in: Option<&Diff> = None;
    match input {
        Input::Ticket(b, scope) => match crate::ticket::Ticket::parse_bytes(b) {
            Ok(t) => ticket = Some((t, scope)),
            Err(e) => refused.push(format!("harness: ticket refused: {}", esc(&e))),
        },
        Input::Diff(d) => diff_in = Some(d),
    }
    // Does a token hit: by the ticket and the scope, or by the diff.
    let hits = |tok: &str| -> Option<bool> {
        match (&ticket, diff_in) {
            (Some((t, scope)), _) => Some(token_hits(tok, t, scope)),
            (_, Some(d)) => Some(diff_hits(tok, d)),
            _ => None,
        }
    };
    let (mut listed_always, mut listed_matched) = (Vec::new(), Vec::new());
    let mut ids: Vec<&str> = Vec::new();
    let mut always_ids: Vec<&str> = Vec::new();
    let (mut retired, mut always_pages) = (0usize, 0usize);
    for ((name, _), text) in names.iter().zip(&texts) {
        let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        let PageRead { close, retired: is_retired, tokens, always, has_always, problems } = read_page(&lines);
        for p in &problems {
            refused.push(format!("harness: lesson lookup refused: {}: {}", esc(name), esc(p)));
        }
        let Some(close) = close else { continue };
        // The cap counts each page that carries the key always and is not retired, a refused value included.
        if has_always && !is_retired {
            always_pages += 1;
        }
        if ticket.is_none() && diff_in.is_none() {
            continue;
        }
        if is_retired {
            retired += 1;
            continue;
        }
        // An always page is listed on its own line, not as a lesson: line and not in matched=.
        if let Some(rule) = &always {
            let id = &name[..9];
            listed_always.push(ListedPage {
                line: format!("always: {} {}", id, esc(rule)),
                id: id.to_string(),
                name: name.clone(),
                always: true,
                text: None,
            });
            always_ids.push(id);
            continue;
        }
        if problems.is_empty() && tokens.iter().any(|tok| hits(tok.as_str()) == Some(true)) {
            let id = &name[..9];
            listed_matched.push(ListedPage {
                line: format!("lesson: {} {} {}", id, esc(&heading(&lines, close, name)), esc(name)),
                id: id.to_string(),
                name: name.clone(),
                always: false,
                text: Some(text.clone()),
            });
            ids.push(id);
        }
    }
    if let Some(line) = always_cap(always_pages) {
        refused.push(line);
    }
    if !refused.is_empty() {
        return Err((1, refused));
    }
    let matched = if ids.is_empty() { "none".to_string() } else { ids.join(",") };
    let mut last = format!("lessons: scanned={} matched={}", names.len(), matched);
    if !always_ids.is_empty() {
        last.push_str(&format!(" always={}", always_ids.join(",")));
    }
    if retired > 0 {
        last.push_str(&format!(" retired={}", retired));
    }
    listed_always.extend(listed_matched);
    Ok(PagesAnswer { pages: listed_always, receipt: last })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_kind_checks_utf8_and_digits() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(name_kind(OsStr::from_bytes(b"ISSUE-002-\xff.md")), NameKind::NotUtf8);
        assert_eq!(name_kind(OsStr::from_bytes(b"\xff.md")), NameKind::NotPage);
        assert_eq!(name_kind(OsStr::new("ISSUE-002-a.md")), NameKind::Page);
        for n in ["ISSUE-1-a.md", "ISSUE-02-a.md", "ISSUE-0002-a.md"] {
            assert_eq!(name_kind(OsStr::new(n)), NameKind::Digits, "{}", n);
        }
        for n in ["README.md", "ISSUE-001.md", "ISSUE-001-a.txt", "issue-001-a.md", "ISSUE-001-a.MD", ".ISSUE-001-a.md"] {
            assert_eq!(name_kind(OsStr::new(n)), NameKind::NotPage, "{}", n);
        }
    }
}
