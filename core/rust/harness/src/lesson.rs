//! `harness lesson lint <path> [<path> ...]`: checks the shape of lesson pages (see `docs/lesson-lint.md`). The lint
//! checks shape only: it does not check that a sentence is true, that a link resolves, or that an eval result exists.
//! std only; the front matter is parsed by hand (the crate has no dependency).

use crate::fix_tier::{PROVEN_ABSENT, TIERS};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const LESSON_USAGE: &str = "usage: harness lesson lint <path> [<path> ...]";

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

/// A control character or a blank other than a plain space is written `\u{hex}` in a printed line.
fn esc(s: &str) -> String {
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
    for p in &pages {
        let path = p.to_string_lossy().to_string();
        let file = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
        let bytes = std::fs::read(p).map_err(|_| format!("the file {} cannot be read", path))?;
        let text = String::from_utf8(bytes).map_err(|_| format!("the file {} is not valid UTF-8", path))?;
        for problem in check_page(&text, &file, &mut seen) {
            out.push(format!("{}: {}", file, problem));
        }
    }
    Ok((pages.len(), out))
}

/// Check one page (`file` is its file name, not its path) and return its `<RULE> <detail>` lines. `seen` maps an id to the first file that carried it.
fn check_page(text: &str, file: &str, seen: &mut HashMap<String, String>) -> Vec<String> {
    let mut out = Vec::new();
    let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    // FRONT-MATTER: line 1 opens, a later `---` closes; without a well-formed block nothing later can be checked.
    if lines.first() != Some(&"---") {
        out.push("FRONT-MATTER line 1 is not ---".to_string());
        return out;
    }
    let Some(close) = lines[1..].iter().position(|&l| l == "---").map(|i| i + 1) else {
        out.push("FRONT-MATTER no closing ---".to_string());
        return out;
    };
    let mut keys: HashMap<&str, &str> = HashMap::new();
    for (i, &l) in lines[1..close].iter().enumerate() {
        match l.split_once(':') {
            // Only ASCII spaces and tabs are trimmed around the key and the value.
            Some((k, v)) if !k.trim_matches([' ', '\t']).is_empty() => {
                let (k, v) = (k.trim_matches([' ', '\t']), v.trim_matches([' ', '\t']));
                if keys.insert(k, v).is_some() {
                    out.push(format!("FRONT-MATTER line {} repeats the key {}", i + 2, k));
                }
            }
            _ => out.push(format!("FRONT-MATTER line {} is not key: value", i + 2)),
        }
    }
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
    match keys.get("status").copied() {
        None => out.push("STATUS status is missing".to_string()),
        Some(v @ ("unvalidated" | "validated" | "retired")) => status = Some(v),
        Some(v) => out.push(format!("STATUS status {} is not unvalidated, validated or retired", v)),
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
    if let Some(&v) = keys.get("match") {
        if !(v.starts_with('[') && v.ends_with(']')) {
            out.push(format!("MATCH match {} is not an inline list [..]", v));
        } else {
            let tokens = split_list(v);
            if tokens.is_empty() {
                out.push("MATCH match is an empty list".to_string());
            } else if tokens.iter().any(|t| t.is_empty()) {
                out.push("MATCH match has an empty token".to_string());
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
/// 2 usage or read error (nothing on stdout). Any other verb, or no path, prints the usage and exits 2.
pub fn lesson_cmd(a: &[String]) -> i32 {
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
