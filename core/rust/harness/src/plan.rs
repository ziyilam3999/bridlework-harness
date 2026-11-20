//! `harness plan check|status` (std only, no cfg(unix); it does not use launcher.rs or ticket_gate.rs). It reads three private
//! inputs passed by path: the plan index (`plan_index.rs`), the folder of ticket files (`<id>.json`) and the folder of done records
//! (`<id>.done`, first line `ticket=<id>`). `check` is red when the index and the finished tickets disagree (a built row with no
//! done ticket, a done ticket with no row and no reason, an incomplete deviation or dropped row, a repeated id, an unknown
//! state); a dropped row (a recorded decision not to build it) is not open and needs no estimate; `status`
//! reports rows per state and progress weighted by size (S=1, M=3, L=9); progress counts linked tickets. In whole-index
//! status the last line `progress-estimated` adds the optional `left_estimate` of open rows; in whole-index check an open row
//! with no estimate is red. Both read only. They read links and states, not code.
//! One unparsable ticket or bad done record makes check, status and ticket mode exit 2. Details: `docs/plan-index.md`.
//! `harness plan stale-notes` (read only) lists lines in named files that hold a pending phrase and a done or built id, and
//! in-build rows that list tickets, all done, with no estimated work left.

use crate::plan_index::{self, Row, State};
use crate::ticket::Ticket;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const USAGE: &str = "usage: harness plan check|status --index <absolute-file> --tickets <absolute-dir> --done <absolute-dir> [--ticket <id>] [--plan <absolute-file> --base-plan <absolute-file> --base-index <absolute-file>]";

const MAX_TICKET_BYTES: u64 = 1024 * 1024 + 1;
const DONE_READ_CAP: u64 = 256;
const DECIDERS: [&str; 2] = ["operator", "orchestrator-reading"];
/// Largest plan file read, in bytes.
const MAX_PLAN_BYTES: u64 = 16 * 1024 * 1024;
/// Characters of a plan line that are compared.
const LINE_CHARS: usize = 40;

/// The three paths of the plan-line comparison: head plan, base plan, base index.
struct CmpArgs {
    plan: PathBuf,
    base_plan: PathBuf,
    base_index: PathBuf,
}

struct Args {
    check: bool,
    index: PathBuf,
    tickets: PathBuf,
    done: PathBuf,
    ticket: Option<String>,
    cmp: Option<CmpArgs>,
}

/// A ticket id or a file stem: the link element rule, not starting with a dot, no two dots in a row.
fn name_ok(s: &str) -> bool {
    plan_index::id_ok(s) && !s.starts_with('.') && !s.contains("..")
}

fn abs_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| c == Component::ParentDir)
}

fn parse_args(a: &[String]) -> Option<Args> {
    let (sub, rest) = a.split_first()?;
    let check = match sub.as_str() {
        "check" => true,
        "status" => false,
        _ => return None,
    };
    let (mut index, mut tickets, mut done, mut ticket): (Option<&String>, Option<&String>, Option<&String>, Option<&String>) = (None, None, None, None);
    let (mut plan, mut base_plan, mut base_index): (Option<&String>, Option<&String>, Option<&String>) = (None, None, None);
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--index" => &mut index,
            "--tickets" => &mut tickets,
            "--done" => &mut done,
            "--ticket" => &mut ticket,
            "--plan" => &mut plan,
            "--base-plan" => &mut base_plan,
            "--base-index" => &mut base_index,
            _ => return None,
        };
        let v = it.next()?;
        if slot.replace(v).is_some() {
            return None;
        }
    }
    let (index, tickets, done) = (index?, tickets?, done?);
    if ![index, tickets, done].iter().all(|p| abs_ok(p)) || ticket.is_some_and(|t| !name_ok(t)) {
        return None;
    }
    // The three comparison options go together, with check and no --ticket.
    let cmp = match (plan, base_plan, base_index) {
        (None, None, None) => None,
        (Some(p), Some(b), Some(i)) if check && ticket.is_none() && [p, b, i].iter().all(|x| abs_ok(x)) => {
            Some(CmpArgs { plan: p.into(), base_plan: b.into(), base_index: i.into() })
        }
        _ => return None,
    };
    Some(Args { check, index: index.into(), tickets: tickets.into(), done: done.into(), ticket: ticket.cloned(), cmp })
}

/// One refusal text: the file name is shown only when it follows the id rule.
fn refusal(kind: &str, stem: &str, what: &str) -> String {
    if name_ok(stem) {
        format!("{kind} {stem}: {what}")
    } else {
        format!("a {kind} file: {what}")
    }
}

/// The names in `dir` ending in `ext`, in byte order. No sub-folder is read.
fn list(dir: &Path, ext: &str, kind: &str) -> Result<Vec<String>, String> {
    let gone = || format!("the {kind} folder is missing or cannot be read");
    let mut names = Vec::new();
    for e in std::fs::read_dir(dir).map_err(|_| gone())? {
        let n = e.map_err(|_| gone())?.file_name().to_string_lossy().into_owned();
        if n.ends_with(ext) {
            names.push(n);
        }
    }
    names.sort();
    Ok(names)
}

/// The stem of an entry, after the name rule and the regular-file rule.
fn entry<'a>(dir: &Path, name: &'a str, ext: &str, kind: &str) -> Result<&'a str, String> {
    let stem = name.strip_suffix(ext).unwrap_or(name);
    if !name_ok(stem) {
        return Err(format!("a {kind} file name is outside the id rule"));
    }
    match std::fs::symlink_metadata(dir.join(name)) {
        Ok(m) if m.file_type().is_file() => Ok(stem),
        _ => Err(refusal(kind, stem, "is not a regular file")),
    }
}

struct Inputs {
    rows: Vec<Row>,
    /// Ticket files in byte order of the id.
    tickets: Vec<Ticket>,
    /// Ids that have a done record.
    done: BTreeSet<String>,
}

fn read_inputs(a: &Args) -> Result<Inputs, String> {
    let rows = plan_index::read(&a.index).map_err(|e| format!("plan index: {e}"))?;
    let mut tickets = Vec::new();
    for name in list(&a.tickets, ".json", "tickets")? {
        let stem = entry(&a.tickets, &name, ".json", "ticket")?;
        let mut bytes = Vec::new();
        let got = std::fs::File::open(a.tickets.join(&name)).and_then(|f| f.take(MAX_TICKET_BYTES).read_to_end(&mut bytes));
        if got.is_err() {
            return Err(refusal("ticket", stem, "cannot be read"));
        }
        let t = Ticket::parse_bytes(&bytes).map_err(|_| refusal("ticket", stem, "does not parse"))?;
        if t.id != stem {
            return Err(refusal("ticket", stem, "its id differs from its file name"));
        }
        tickets.push(t);
    }
    // File-name order differs from id order when one id is a prefix of another.
    tickets.sort_by(|a, b| a.id.cmp(&b.id));
    let mut done = BTreeSet::new();
    for name in list(&a.done, ".done", "done")? {
        let stem = entry(&a.done, &name, ".done", "done record")?;
        let mut bytes = Vec::new();
        let got = std::fs::File::open(a.done.join(&name)).and_then(|f| f.take(DONE_READ_CAP).read_to_end(&mut bytes));
        if got.is_err() {
            return Err(refusal("done record", stem, "cannot be read"));
        }
        let first = bytes.split(|b| *b == b'\n').next().unwrap_or(&[]);
        if first != format!("ticket={stem}").as_bytes() {
            return Err(refusal("done record", stem, "its first line is not ticket=<id>"));
        }
        done.insert(stem.to_string());
    }
    Ok(Inputs { rows, tickets, done })
}

fn state_name(s: State) -> &'static str {
    match s {
        State::NotBuilt => "not-built",
        State::InBuild => "in-build",
        State::Built => "built",
        State::Deviation => "deviation",
        State::Later => "later",
        State::Dropped => "dropped",
        State::Unknown => "unknown",
    }
}

const STATES: [State; 6] = [State::NotBuilt, State::InBuild, State::Built, State::Deviation, State::Later, State::Unknown];

/// A title or reason with each control character replaced by one blank.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// Indices of the rows a ticket links to: it names the row id, or the row lists it.
fn linked(rows: &[Row], t: &Ticket) -> Vec<usize> {
    let named = |r: &Row| t.main_plan_ref.as_ref().is_some_and(|m| m.contains(&r.id));
    (0..rows.len()).filter(|&i| named(&rows[i]) || rows[i].tickets.contains(&t.id)).collect()
}

fn points(t: &Ticket) -> u64 {
    match t.size.as_str() {
        "S" => 1,
        "M" => 3,
        _ => 9,
    }
}

fn pct(part: u64, whole: u64) -> String {
    (100 * part).checked_div(whole).map_or("none".to_string(), |p| p.to_string())
}

/// The deviation classes of one row (none unless its state is deviation or dropped).
fn deviation_classes(r: &Row) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !matches!(r.state, State::Deviation | State::Dropped) {
        return out;
    }
    let field = |f: fn(&plan_index::Deviation) -> &Option<String>| r.deviation.as_ref().and_then(|d| f(d).as_deref()).filter(|s| !s.trim().is_empty());
    if field(|d| &d.reason).is_none() {
        out.push("deviation-no-reason");
    }
    if field(|d| &d.decided_by).is_none() {
        out.push("deviation-no-decided-by");
    }
    if field(|d| &d.date).is_none() {
        out.push("deviation-no-date");
    }
    if field(|d| &d.decided_by).is_some_and(|v| !DECIDERS.contains(&v)) {
        out.push("deviation-bad-decided-by");
    }
    out
}

/// True when row `i` is the second row of an id (a third sighting is not named again).
fn second_of_id(rows: &[Row], i: usize) -> bool {
    rows[..i].iter().filter(|e| e.id == rows[i].id).count() == 1
}

/// A not-built or in-build row without a left_estimate key (a missing number is not zero).
fn open_no_estimate(r: &Row) -> bool {
    matches!(r.state, State::NotBuilt | State::InBuild) && r.left_estimate.is_none()
}

/// The inputs of the plan-line comparison, read.
struct Cmp {
    base_rows: Vec<Row>,
    head_lines: Vec<String>,
    base_lines: Vec<String>,
}

/// One plan file as text, or the reason it is refused.
fn read_plan(path: &Path) -> Result<String, &'static str> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Err("is not a regular file"),
        Err(_) => return Err("is missing or cannot be read"),
    }
    let mut bytes = Vec::new();
    if std::fs::File::open(path).and_then(|f| f.take(MAX_PLAN_BYTES + 1).read_to_end(&mut bytes)).is_err() {
        return Err("cannot be read");
    }
    if bytes.len() as u64 > MAX_PLAN_BYTES {
        return Err("is over 16777216 bytes");
    }
    let text = String::from_utf8(bytes).map_err(|_| "is not valid UTF-8")?;
    if text.is_empty() {
        return Err("is empty");
    }
    Ok(text)
}

/// Lines end at LF; a final LF adds no empty last line.
fn plan_lines(text: &str) -> Vec<String> {
    let mut v: Vec<&str> = text.split('\n').collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v.into_iter().map(str::to_string).collect()
}

/// Read the base index, the head plan file and the base plan file, in this order, then the same-file refusal.
fn read_cmp(c: &CmpArgs) -> Result<Cmp, String> {
    let base_rows = plan_index::read(&c.base_index).map_err(|e| format!("base plan index: {e}"))?;
    if !plan_index::repeated_ids(&base_rows).is_empty() {
        return Err("base plan index: an id is on more than one row".to_string());
    }
    let head = read_plan(&c.plan).map_err(|e| format!("plan file: {e}"))?;
    let base = read_plan(&c.base_plan).map_err(|e| format!("base plan file: {e}"))?;
    match (std::fs::canonicalize(&c.plan), std::fs::canonicalize(&c.base_plan)) {
        (Ok(a), Ok(b)) if a == b => return Err("plan file: is the same file as the base plan file".to_string()),
        _ => {}
    }
    Ok(Cmp { base_rows, head_lines: plan_lines(&head), base_lines: plan_lines(&base) })
}

/// The first 40 characters of a line, after a trailing run of CR, space and tab is dropped.
fn key(l: &str) -> impl Iterator<Item = char> + '_ {
    l.trim_end_matches(['\r', ' ', '\t']).chars().take(LINE_CHARS)
}

/// None when the row is not compared (no base row with the same id and plan_line); else whether its line is stale.
fn line_stale(c: &Cmp, r: &Row) -> Option<bool> {
    c.base_rows.iter().find(|b| b.id == r.id && b.plan_line == r.plan_line)?;
    let at = |lines: &[String]| usize::try_from(r.plan_line).ok().and_then(|n| n.checked_sub(1)).and_then(|i| lines.get(i).cloned());
    Some(match (at(&c.base_lines), at(&c.head_lines)) {
        (Some(b), Some(h)) => !key(&b).eq(key(&h)),
        _ => true,
    })
}

fn red(findings: &[String]) -> i32 {
    for f in findings {
        println!("{f}");
    }
    println!("plan check red findings={}", findings.len());
    1
}

fn check(inp: &Inputs, cmp: Option<&Cmp>) -> i32 {
    let (rows, tickets) = (&inp.rows, &inp.tickets);
    let mut f: Vec<String> = Vec::new();
    let mut compared = 0usize;
    for (i, r) in rows.iter().enumerate() {
        if second_of_id(rows, i) {
            f.push(format!("red duplicate-id {}", r.id));
        }
        if r.state == State::Unknown {
            f.push(format!("red unknown-state {}", r.id));
        }
        if r.state == State::Built {
            let any_done = tickets.iter().any(|t| inp.done.contains(&t.id) && linked(rows, t).contains(&i));
            if !any_done {
                f.push(format!("red built-no-done-ticket {}", r.id));
            }
        }
        f.extend(deviation_classes(r).into_iter().map(|c| format!("red {c} {}", r.id)));
        if open_no_estimate(r) {
            f.push(format!("red open-row-no-estimate {}", r.id));
        }
        if let Some(stale) = cmp.and_then(|c| line_stale(c, r)) {
            compared += 1;
            if stale {
                f.push(format!("red plan-line-stale {}", r.id));
            }
        }
    }
    for t in tickets {
        for el in t.main_plan_ref.iter().flatten().filter(|el| !rows.iter().any(|r| &r.id == *el)) {
            f.push(format!("red ticket-names-unknown-row {} {el}", t.id));
        }
        if inp.done.contains(&t.id) && t.off_plan.is_none() && linked(rows, t).is_empty() {
            f.push(format!("red done-ticket-no-row {}", t.id));
        }
    }
    for d in inp.done.iter().filter(|d| !tickets.iter().any(|t| &t.id == *d)) {
        f.push(format!("red done-record-no-ticket {d}"));
    }
    if f.is_empty() {
        let done = tickets.iter().filter(|t| inp.done.contains(&t.id)).count();
        let tail = if cmp.is_some() { format!(" compared={compared}") } else { String::new() };
        println!("plan check ok rows={} tickets={} done={}{tail}", rows.len(), tickets.len(), done);
        0
    } else {
        red(&f)
    }
}

fn check_ticket(inp: &Inputs, t: &Ticket) -> i32 {
    let rows = &inp.rows;
    let links = linked(rows, t);
    let mut f: Vec<String> = Vec::new();
    if t.off_plan.is_none() && links.is_empty() {
        f.push(format!("red no-plan-link {}", t.id));
    }
    for el in t.main_plan_ref.iter().flatten().filter(|el| !rows.iter().any(|r| &r.id == *el)) {
        f.push(format!("red ticket-names-unknown-row {} {el}", t.id));
    }
    let mut dup_seen: Vec<&str> = Vec::new();
    for &i in &links {
        let r = &rows[i];
        if rows.iter().filter(|e| e.id == r.id).count() > 1 && !dup_seen.contains(&r.id.as_str()) {
            dup_seen.push(&r.id);
            f.push(format!("red duplicate-id {}", r.id));
        }
        if r.state == State::Unknown {
            f.push(format!("red unknown-state {}", r.id));
        }
        f.extend(deviation_classes(r).into_iter().map(|c| format!("red {c} {}", r.id)));
    }
    if f.is_empty() {
        println!("plan check ok ticket={}", t.id);
        0
    } else {
        red(&f)
    }
}

fn status(inp: &Inputs) -> i32 {
    let (rows, tickets) = (&inp.rows, &inp.tickets);
    for s in STATES {
        let ids: Vec<&str> = rows.iter().filter(|r| r.state == s).map(|r| r.id.as_str()).collect();
        let list = if ids.is_empty() { "-".to_string() } else { ids.join(",") };
        println!("state {} rows={} ids={}", state_name(s), ids.len(), list);
    }
    // The dropped line is printed only when a row is dropped, so an index without one reads as before.
    let dropped: Vec<&str> = rows.iter().filter(|r| r.state == State::Dropped).map(|r| r.id.as_str()).collect();
    if !dropped.is_empty() {
        println!("state dropped rows={} ids={}", dropped.len(), dropped.join(","));
    }
    let links: Vec<Vec<usize>> = tickets.iter().map(|t| linked(rows, t)).collect();
    let listed = |t: &Ticket| rows.iter().any(|r| r.tickets.contains(&t.id));
    for t in tickets {
        if let Some(reason) = &t.off_plan {
            if !listed(t) {
                let d = if inp.done.contains(&t.id) { "yes" } else { "no" };
                println!("off-plan ticket={} done={} reason={}", t.id, d, clean(reason));
            }
        }
    }
    for (i, r) in rows.iter().enumerate() {
        if r.state != State::Later {
            continue;
        }
        let ids: Vec<&str> = tickets.iter().zip(&links).filter(|(_, l)| l.contains(&i)).map(|(t, _)| t.id.as_str()).collect();
        if !ids.is_empty() {
            println!("leaned-on row={} tickets={}", r.id, ids.join(","));
        }
    }
    let (mut done_pts, mut left_pts, mut all_done) = (0u64, 0u64, 0u64);
    for (t, l) in tickets.iter().zip(&links) {
        let is_done = inp.done.contains(&t.id);
        if is_done {
            all_done += points(t);
        }
        if !l.is_empty() {
            if is_done {
                done_pts += points(t);
            } else {
                left_pts += points(t);
            }
        }
    }
    println!("progress done={} left={} percent={}", done_pts, left_pts, pct(done_pts, done_pts + left_pts));
    println!("on-plan done={} all-done={} percent={}", done_pts, all_done, pct(done_pts, all_done));
    // Row estimates: open rows only, one count per sighting.
    let mut missing = 0usize;
    let mut estimate = 0u64;
    for r in rows {
        if open_no_estimate(r) {
            println!("no-estimate row={}", r.id);
            missing += 1;
        }
        if matches!(r.state, State::NotBuilt | State::InBuild) {
            estimate += r.left_estimate.unwrap_or(0);
        }
    }
    let left_all = left_pts + estimate;
    let percent = if missing > 0 { "none".to_string() } else { pct(done_pts, done_pts + left_all) };
    println!("progress-estimated done={} left={} estimate={} percent={}", done_pts, left_all, estimate, percent);
    0
}

fn status_ticket(inp: &Inputs, t: &Ticket) -> i32 {
    let links = linked(&inp.rows, t);
    for &i in &links {
        let r = &inp.rows[i];
        println!("row={} state={} plan_line={} title={}", r.id, state_name(r.state), r.plan_line, clean(&r.title));
    }
    if links.is_empty() {
        match &t.off_plan {
            Some(reason) => println!("off-plan reason={}", clean(reason)),
            None => println!("no-plan-link ticket={}", t.id),
        }
    }
    0
}

pub const STALE_USAGE: &str = "usage: harness plan stale-notes --index <absolute-file> --tickets <absolute-dir> --done <absolute-dir> <absolute-file> [<absolute-file> ...]";

/// Largest file `stale-notes` reads, in bytes.
const MAX_NOTE_BYTES: u64 = 4 * 1024 * 1024;
/// Pending phrases, lower case, split into words.
const PHRASES: [&[&str]; 6] = [&["until"], &["being", "built"], &["not", "built"], &["not", "yet", "built"], &["waits", "for"], &["todo"]];

fn word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn blank(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// True when a phrase starts at `i` with no word byte directly before or after it.
fn phrase_at(l: &[u8], i: usize) -> bool {
    if i > 0 && word_byte(l[i - 1]) {
        return false;
    }
    'phrase: for words in PHRASES {
        let mut p = i;
        for (k, w) in words.iter().enumerate() {
            if k > 0 {
                let s = p;
                while p < l.len() && blank(l[p]) {
                    p += 1;
                }
                if p == s {
                    continue 'phrase;
                }
            }
            if !l.get(p..p + w.len()).is_some_and(|x| x.eq_ignore_ascii_case(w.as_bytes())) {
                continue 'phrase;
            }
            p += w.len();
        }
        if p >= l.len() || !word_byte(l[p]) {
            return true;
        }
    }
    false
}

fn has_phrase(l: &[u8]) -> bool {
    (0..l.len()).any(|i| phrase_at(l, i))
}

fn id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

/// True when `id` stands as a whole id somewhere in the line.
fn has_id(l: &[u8], id: &str) -> bool {
    let n = id.len();
    (0..l.len()).any(|i| {
        if l.get(i..i + n) != Some(id.as_bytes()) {
            return false;
        }
        if i > 0 && (id_byte(l[i - 1]) || l[i - 1] == b'.') {
            return false;
        }
        match l.get(i + n) {
            None => true,
            Some(&c) if id_byte(c) => false,
            Some(b'.') => !l.get(i + n + 1).is_some_and(|&d| id_byte(d)),
            Some(_) => true,
        }
    })
}

fn stale_args(a: &[String]) -> Option<(Args, Vec<String>)> {
    let (mut index, mut tickets, mut done): (Option<&String>, Option<&String>, Option<&String>) = (None, None, None);
    let mut files: Vec<String> = Vec::new();
    let mut it = a.iter();
    while let Some(w) = it.next() {
        if w.starts_with("--") {
            let slot = match w.as_str() {
                "--index" => &mut index,
                "--tickets" => &mut tickets,
                "--done" => &mut done,
                _ => return None,
            };
            if slot.replace(it.next()?).is_some() {
                return None;
            }
        } else {
            if files.contains(w) || !abs_ok(w) {
                return None;
            }
            files.push(w.clone());
        }
    }
    let (index, tickets, done) = (index?, tickets?, done?);
    if files.is_empty() || ![index, tickets, done].iter().all(|p| abs_ok(p)) {
        return None;
    }
    Some((Args { check: true, index: index.into(), tickets: tickets.into(), done: done.into(), ticket: None, cmp: None }, files))
}

/// One named file as text, or the reason it is refused.
fn read_note(path: &str) -> Result<String, &'static str> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Err("is not a regular file"),
        Err(_) => return Err("is missing or cannot be read"),
    }
    let mut bytes = Vec::new();
    let got = std::fs::File::open(path).and_then(|f| f.take(MAX_NOTE_BYTES + 1).read_to_end(&mut bytes));
    if got.is_err() {
        return Err("cannot be read");
    }
    if bytes.len() as u64 > MAX_NOTE_BYTES {
        return Err("is over 4194304 bytes");
    }
    String::from_utf8(bytes).map_err(|_| "is not valid UTF-8")
}

fn stale_notes(a: &[String]) -> i32 {
    let Some((args, files)) = stale_args(a) else {
        eprintln!("{STALE_USAGE}");
        return 2;
    };
    let inp = match read_inputs(&args) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("harness: error: {e}");
            return 2;
        }
    };
    let mut texts = Vec::new();
    for (n, f) in files.iter().enumerate() {
        match read_note(f) {
            Ok(t) => texts.push(t),
            Err(what) => {
                eprintln!("harness: error: file {}: {what}", n + 1);
                return 2;
            }
        }
    }
    // id -> (done, built)
    let mut ids: std::collections::BTreeMap<&str, (bool, bool)> = std::collections::BTreeMap::new();
    for d in &inp.done {
        ids.entry(d).or_default().0 = true;
    }
    for r in inp.rows.iter().filter(|r| r.state == State::Built) {
        ids.entry(&r.id).or_default().1 = true;
    }
    let (mut notes, mut rows) = (0usize, 0usize);
    for (f, text) in files.iter().zip(&texts) {
        for (n, line) in text.split('\n').enumerate() {
            let l = line.strip_suffix('\r').unwrap_or(line).as_bytes();
            if !has_phrase(l) {
                continue;
            }
            for (id, (d, b)) in &ids {
                if has_id(l, id) {
                    let by = match (d, b) {
                        (true, true) => "done,built",
                        (true, false) => "done",
                        _ => "built",
                    };
                    println!("stale-note id={id} by={by} line={} file={}", n + 1, clean(f));
                    notes += 1;
                }
            }
        }
    }
    for r in &inp.rows {
        if r.state == State::InBuild && !r.tickets.is_empty() && r.tickets.iter().all(|t| inp.done.contains(t)) && r.left_estimate.unwrap_or(0) == 0 {
            let est = r.left_estimate.map_or("none".to_string(), |v| v.to_string());
            println!("stale-row id={} tickets={} left_estimate={est}", r.id, r.tickets.len());
            rows += 1;
        }
    }
    println!("stale-notes notes={notes} rows={rows} files={}", files.len());
    i32::from(notes > 0 || rows > 0)
}

pub fn plan_cmd(a: &[String]) -> i32 {
    if a.first().map(String::as_str) == Some("stale-notes") {
        return stale_notes(&a[1..]);
    }
    let Some(args) = parse_args(a) else {
        eprintln!("{USAGE}");
        return 2;
    };
    let inp = match read_inputs(&args) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("harness: error: {e}");
            return 2;
        }
    };
    let one = match &args.ticket {
        Some(id) => match inp.tickets.iter().find(|t| &t.id == id) {
            Some(t) => Some(t),
            None => {
                eprintln!("harness: error: the ticket file is missing");
                return 2;
            }
        },
        None => None,
    };
    let cmp = match args.cmp.as_ref().map(read_cmp) {
        Some(Ok(c)) => Some(c),
        Some(Err(e)) => {
            eprintln!("harness: error: {e}");
            return 2;
        }
        None => None,
    };
    match (args.check, one) {
        (true, None) => check(&inp, cmp.as_ref()),
        (true, Some(t)) => check_ticket(&inp, t),
        (false, None) => status(&inp),
        (false, Some(t)) => status_ticket(&inp, t),
    }
}
