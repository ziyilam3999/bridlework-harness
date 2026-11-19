//! `harness lesson replay <lessons-dir> --tickets <absolute-dir> --scopes <absolute-dir>`: replays the
//! ticket-mode lookup over a folder of tickets and counts the offered pages with the `always` pages apart
//! (see `docs/lesson-lint.md`).
//! With `--config` and `--controls` (and `--rows`) it also prints the answer key: whether the page a friction
//! row's group was bound to was offered on that row's own ticket. Read only: the run writes no file in the
//! lessons folder, the tickets folder, the scopes folder, the folders of the three flag files or the current
//! folder. The pages are read once as the lookup reads them, then matched against each ticket.
//! Ticket mode only. std only.

use crate::friction::Row;
use crate::friction_loop::{groups, lint_rows, load_cfg, load_list, read_controls};
use crate::lesson::{always_cap, esc, read_page, token_hits, walk, PageRead, LESSON_USAGE, MAX_FILE};
use crate::ticket::Ticket;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// One ticket of the run: the id (the file name without `.json`) and its path.
struct TicketFile {
    id: String,
    path: PathBuf,
}

/// One page read once: its file name, its id, its tokens, its retired flag and whether it carries the key
/// `always` and is not retired (the cap count and the `always=` count read it the same way).
struct Page {
    id: String,
    retired: bool,
    always: bool,
    tokens: Vec<String>,
}

/// A folder argument of the verb: absolute, with no `..` part.
fn folder_ok(p: &str) -> bool {
    let q = Path::new(p);
    q.is_absolute() && !q.components().any(|c| matches!(c, Component::ParentDir))
}

/// Read a ticket or scope file to the reader limit plus one byte. Err: it is a symbolic link, not a regular
/// file, or it cannot be read.
fn read_input(p: &Path) -> Result<Vec<u8>, ()> {
    use std::io::Read;
    let md = std::fs::symlink_metadata(p).map_err(|_| ())?;
    if !md.file_type().is_file() {
        return Err(());
    }
    let mut b = Vec::new();
    std::fs::File::open(p).and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut b)).map_err(|_| ())?;
    Ok(b)
}

/// The `<id>.json` entries directly inside the tickets folder, by id in byte order. Err is the one message
/// for exit 2; a `.json` entry that is not a regular file is pushed to `refused` (exit 1) with its file name.
fn list_tickets(dir: &str, refused: &mut Vec<String>) -> Result<Vec<TicketFile>, String> {
    let p = Path::new(dir);
    let md = std::fs::metadata(p).map_err(|_| "the tickets folder does not exist or cannot be read".to_string())?;
    if !md.is_dir() {
        return Err("the tickets argument is a file, not a folder".to_string());
    }
    let rd = std::fs::read_dir(p).map_err(|_| "the tickets folder cannot be read".to_string())?;
    let mut out: Vec<TicketFile> = Vec::new();
    for ent in rd {
        let ent = ent.map_err(|_| "the tickets folder cannot be read".to_string())?;
        let raw = ent.file_name();
        let lossy = raw.to_string_lossy();
        if !lossy.ends_with(".json") {
            continue;
        }
        let ft = ent.file_type().map_err(|_| format!("the entry {} cannot be inspected", esc(&lossy)))?;
        if ft.is_symlink() || !ft.is_file() {
            refused.push(format!("harness: lesson replay refused: {}", esc(&lossy)));
            continue;
        }
        out.push(TicketFile { id: lossy[..lossy.len() - 5].to_string(), path: ent.path() });
    }
    if out.is_empty() && refused.is_empty() {
        return Err("the tickets folder holds no ticket file".to_string());
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Read and check the pages once, as the lookup does: the walk names, the front matter rules, the retired
/// skip. Ok holds the pages and the retired count (once per run); a page problem is pushed to `refused` with
/// the lookup's own line; Err is a message for exit 2.
fn read_pages(names: &[(String, PathBuf)], refused: &mut Vec<String>) -> Result<(Vec<Page>, usize), String> {
    let mut texts: Vec<String> = Vec::new();
    for (n, p) in names {
        let b = std::fs::read(p).map_err(|_| format!("the file {} cannot be read", n))?;
        match String::from_utf8(b) {
            Ok(t) => texts.push(t),
            Err(_) => return Err(format!("the file {} is not valid UTF-8", n)),
        }
    }
    let mut pages: Vec<Page> = Vec::new();
    let mut retired = 0usize;
    for ((name, _), text) in names.iter().zip(&texts) {
        let lines: Vec<&str> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
        let PageRead { close, retired: is_retired, tokens, has_always, problems, .. } = read_page(&lines);
        for p in &problems {
            refused.push(format!("harness: lesson lookup refused: {}: {}", esc(name), esc(p)));
        }
        if close.is_some() && is_retired {
            retired += 1;
        }
        pages.push(Page { id: name[..9].to_string(), retired: is_retired, always: has_always && !is_retired, tokens });
    }
    Ok((pages, retired))
}

// ---------------------------------------------------------------- the answer key (--config, --controls, --rows)

/// A rows file is read no further than this; past it the run refuses (exit 2).
const ROWS_CAP: u64 = 1024 * 1024;

/// The answer key half of a replay run: the inbox rows, the row indexes looked at, the page id of each
/// looked-at row's group's controls line (absent when its group has no line), and the id count of a --rows file.
struct KeyInput {
    rows: Vec<Row>,
    considered: Vec<usize>,
    page_of: HashMap<String, String>,
    listed: Option<usize>,
}

/// The id lines of a --rows file: one id per line, the final line feed optional, nothing trimmed. A blank
/// line and an empty file refuse (1); a missing path, a non-regular entry (a symbolic link included), a file
/// over the cap or a file that is not UTF-8 refuse (2). No id is echoed by the caller.
fn read_rows_lines(path: &str) -> Result<Vec<String>, (i32, String)> {
    let p = Path::new(path);
    let md = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(_) => return Err((2, "the rows file cannot be read".to_string())),
    };
    if !md.file_type().is_file() {
        return Err((2, "the rows path is not a regular file".to_string()));
    }
    let mut b = Vec::new();
    let read = std::fs::File::open(p).map_err(|_| (2, "the rows file cannot be read".to_string())).and_then(|f| {
        use std::io::Read;
        f.take(ROWS_CAP + 1).read_to_end(&mut b).map_err(|_| (2, "the rows file cannot be read".to_string()))
    });
    read?;
    if b.len() as u64 > ROWS_CAP {
        return Err((2, format!("the rows file is over the {ROWS_CAP}-byte cap")));
    }
    let text = String::from_utf8(b).map_err(|_| (2, "the rows file is not utf-8".to_string()))?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    if lines.is_empty() {
        return Err((1, "rows line 1 the file holds no row id".to_string()));
    }
    for (i, l) in lines.iter().enumerate() {
        if l.is_empty() {
            return Err((1, format!("rows line {} is blank", i + 1)));
        }
    }
    Ok(lines.into_iter().map(String::from).collect())
}

/// Load the answer key: the config and the privacy list as `friction rank` loads them, the inbox after the
/// full lint (the privacy scan of stored rows runs with the test mode off), the groups of
/// `friction_loop::groups` (they follow `breaks` links only), the controls lines read over the run's lessons
/// folder, and with `--rows` the rows the file picks out of the whole-inbox groups. Err is the exit code
/// after the refusal lines are printed.
fn key_setup(lessons: &str, config: &str, controls: &str, rows_file: Option<&str>) -> Result<KeyInput, i32> {
    let refuse = |m: &str| {
        eprintln!("harness: lesson replay refused: {}", esc(m));
        1
    };
    let setup_err = |m: &str| {
        eprintln!("harness: error: {}", esc(m));
        2
    };
    let cfg = load_cfg(Some(config))?;
    let list = load_list("the inbox is not replayed unscanned")?;
    let ctx = crate::scan::Ctx { list: &list, testmode: false };
    let rows = lint_rows(&cfg, &ctx)?;
    let all = match groups(&rows) {
        Ok(g) => g,
        Err(m) => {
            eprintln!("{m}");
            return Err(1);
        }
    };
    let lines = match read_controls(Path::new(controls), Path::new(lessons)) {
        Ok(v) => v,
        Err((c, m)) => return Err(if c == 1 { refuse(&m) } else { setup_err(&m) }),
    };
    // Two lines that name one group id would make the page of a row ambiguous: the later line refuses.
    let mut page_of_group: HashMap<&str, &str> = HashMap::new();
    for c in &lines {
        if page_of_group.insert(c.id.as_str(), c.lesson.as_str()).is_some() {
            return Err(refuse(&format!("controls line {} names the group of an earlier line", c.line)));
        }
    }
    let (considered, listed) = match rows_file {
        None => ((0..rows.len()).collect::<Vec<usize>>(), None),
        Some(path) => match read_rows_lines(path) {
            Ok(ids) => {
                let mut picked: Vec<usize> = Vec::new();
                let mut seen: HashSet<&str> = HashSet::new();
                for (i, id) in ids.iter().enumerate() {
                    let Some(ix) = rows.iter().position(|r| r.id == *id) else {
                        return Err(refuse(&format!("rows line {} is not the id of an inbox row", i + 1)));
                    };
                    if !seen.insert(id.as_str()) {
                        return Err(refuse(&format!("rows line {} names a row twice", i + 1)));
                    }
                    picked.push(ix);
                }
                (picked, Some(ids.len()))
            }
            Err((c, m)) => return Err(if c == 1 { refuse(&m) } else { setup_err(&m) }),
        },
    };
    let mut page_of: HashMap<String, String> = HashMap::new();
    for g in &all {
        if let Some(page) = page_of_group.get(g.id.as_str()) {
            for &m in &g.members {
                page_of.insert(rows[m].id.clone(), page.to_string());
            }
        }
    }
    Ok(KeyInput { rows, considered, page_of, listed })
}

/// `harness lesson replay <lessons-dir> --tickets <absolute-dir> --scopes <absolute-dir>
/// [--config <absolute-file> --controls <absolute-file> [--rows <absolute-file>]]`: exit 0 the counts
/// (one `replay:` line per ticket, one `page:` line per page, the `key:` lines and the `key ok` line when the
/// answer-key flags are given, then the `replay ok` line); 1 a refused page, ticket file, scope file, inbox,
/// controls file or rows file, or more than 5 `always` pages that are not retired (the lookup's own cap line,
/// stdout empty, no partial count); 2 a usage or read error.
pub fn replay_cmd(a: &[String]) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", esc(m));
        2
    };
    let usage = || {
        eprintln!("{}", LESSON_USAGE);
        2
    };
    let (mut dir, mut tickets, mut scopes) = (None, None, None);
    let (mut config, mut controls, mut rowsf) = (None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        // The three answer-key flags go through their own slot rule: given twice is the usage text.
        if matches!(arg.as_str(), "--config" | "--controls" | "--rows") {
            let slot = match arg.as_str() {
                "--config" => &mut config,
                "--controls" => &mut controls,
                _ => &mut rowsf,
            };
            let Some(v) = it.next() else { return fail(&format!("{} needs a value", arg)) };
            if slot.replace(v).is_some() {
                return usage();
            }
            continue;
        }
        let slot = match arg.as_str() {
            "--tickets" => &mut tickets,
            "--scopes" => &mut scopes,
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
    let Some(dir) = dir else { return usage() };
    let (Some(tdir), Some(sdir)) = (tickets, scopes) else {
        return if tickets.is_none() { fail("--tickets is required") } else { fail("--scopes is required") };
    };
    if !folder_ok(dir) || !folder_ok(tdir) || !folder_ok(sdir) {
        return usage();
    }
    // --config and --controls only together, --rows only beside them; each names an absolute file with no
    // `..` part, on pain of the usage text.
    let key_flags = match (config, controls, rowsf) {
        (None, None, None) => None,
        (Some(c), Some(k), r) => {
            if !folder_ok(c) || !folder_ok(k) || r.is_some_and(|v| !folder_ok(v)) {
                return usage();
            }
            Some((c, k, r))
        }
        _ => return usage(),
    };
    // The key's own reads run before the pages walk, so its setup errors exit 2 before any replay refusal.
    let key = match key_flags {
        Some((c, k, r)) => match key_setup(dir, c, k, r.map(String::as_str)) {
            Ok(k) => Some(k),
            Err(code) => return code,
        },
        None => None,
    };
    let names = match walk(dir) {
        Ok(n) => n,
        Err(ms) => {
            for m in &ms {
                eprintln!("harness: error: {}", esc(m));
            }
            return 2;
        }
    };
    let mut refused: Vec<String> = Vec::new();
    let files = match list_tickets(tdir, &mut refused) {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    let sp = Path::new(sdir);
    match std::fs::metadata(sp) {
        Ok(md) if md.is_dir() => {}
        Ok(_) => return fail("the scopes argument is a file, not a folder"),
        Err(_) => return fail("the scopes folder does not exist or cannot be read"),
    }
    // The scope file of a ticket is `<id>.paths` inside the scopes folder; a ticket without one is skipped.
    let mut kept: Vec<(&TicketFile, Vec<u8>)> = Vec::new();
    let mut skipped = 0usize;
    for t in &files {
        let spath = sp.join(format!("{}.paths", t.id));
        if std::fs::symlink_metadata(&spath).is_err() {
            skipped += 1;
            continue;
        }
        match read_input(&spath) {
            Ok(b) => kept.push((t, b)),
            Err(()) => refused.push(format!("harness: lesson replay refused: {}.paths", esc(&t.id))),
        }
    }
    if kept.is_empty() && refused.is_empty() {
        return fail("no ticket left after the no-scope skip");
    }
    let mut page_refused: Vec<String> = Vec::new();
    let (pages, retired) = match read_pages(&names, &mut page_refused) {
        Ok(p) => p,
        Err(m) => return fail(&m),
    };
    // The tickets that were kept: the scope text is parsed, then the ticket file is read with its reader.
    let say = |f: &str| format!("harness: lesson replay refused: {}", f);
    let mut runs: Vec<(String, Ticket, Vec<String>)> = Vec::new();
    for (t, sbytes) in kept {
        let sname = format!("{}.paths", t.id);
        if sbytes.len() as u64 > MAX_FILE {
            refused.push(say(&sname));
            continue;
        }
        let Ok(stext) = String::from_utf8(sbytes) else {
            refused.push(say(&sname));
            continue;
        };
        let Ok(scope) = crate::lane::parse_scope(&stext) else {
            refused.push(say(&sname));
            continue;
        };
        let tname = format!("{}.json", t.id);
        match read_input(&t.path).map_err(|()| say(&tname)).and_then(|b| Ticket::parse_bytes(&b).map_err(|_| say(&tname))) {
            Ok(tk) => runs.push((t.id.clone(), tk, scope)),
            Err(line) => refused.push(line),
        }
    }
    refused.extend(page_refused);
    // The cap counts as the lookup counts, its line comes last; a page with a refused value counts too.
    let always_pages = pages.iter().filter(|p| p.always).count();
    if let Some(line) = always_cap(always_pages) {
        refused.push(line);
    }
    if !refused.is_empty() {
        for l in refused {
            eprintln!("{}", l);
        }
        return 1;
    }
    // Count: each kept ticket applies the match of the lookup over the pages read once.
    let mut hits: Vec<usize> = vec![0; pages.len()];
    let (mut total, mut maxo, mut zero) = (0usize, 0usize, 0usize);
    let mut lines: Vec<String> = Vec::new();
    let mut offered: Vec<(String, Vec<String>)> = Vec::new();
    for (id, tk, scope) in &runs {
        let mut ids: Vec<String> = Vec::new();
        for (i, pg) in pages.iter().enumerate() {
            if pg.retired {
                continue;
            }
            if pg.tokens.iter().any(|tok| token_hits(tok, tk, scope)) {
                ids.push(pg.id.clone());
                hits[i] += 1;
            }
        }
        total += ids.len();
        maxo = maxo.max(ids.len());
        if ids.is_empty() {
            zero += 1;
        }
        let joined = if ids.is_empty() { "none".to_string() } else { ids.join(",") };
        lines.push(format!("replay: ticket={} offered={} pages={}", esc(id), ids.len(), joined));
        offered.push((id.clone(), ids));
    }
    for l in lines {
        println!("{}", l);
    }
    for (i, pg) in pages.iter().enumerate() {
        // An always page is in no pages= list, so its hits stay 0; a retired one keeps today's line.
        if pg.always {
            println!("page: {} hits=0 always", esc(&pg.id));
        } else {
            println!("page: {} hits={}", esc(&pg.id), hits[i]);
        }
    }
    if let Some(key) = &key {
        // A row is tied to its ticket byte for byte; it counts when its group has a controls line and its
        // ticket was replayed, and is offered when that line's page is in the ticket's pages= list. A row on
        // an always page that is not retired counts in offered_always= and is not offered.
        let replayed: HashMap<&str, &Vec<String>> = offered.iter().map(|(id, ids)| (id.as_str(), ids)).collect();
        let always_ids: HashSet<&str> = pages.iter().filter(|p| p.always).map(|p| p.id.as_str()).collect();
        let mut per_page: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
        let (mut n_rows, mut n_off, mut n_always, mut no_ticket, mut no_line) = (0usize, 0usize, 0usize, 0usize, 0usize);
        for &i in &key.considered {
            let r = &key.rows[i];
            let Some(page) = key.page_of.get(&r.id) else {
                no_line += 1;
                continue;
            };
            match replayed.get(r.ticket.as_str()) {
                None => no_ticket += 1,
                Some(pages) => {
                    let hit = pages.iter().any(|p| p == page);
                    let on_always = always_ids.contains(page.as_str());
                    n_rows += 1;
                    if hit {
                        n_off += 1;
                    }
                    if on_always {
                        n_always += 1;
                    }
                    let e = per_page.entry(page.as_str()).or_insert((0, 0, 0));
                    e.0 += 1;
                    if hit {
                        e.1 += 1;
                    }
                    if on_always {
                        e.2 += 1;
                    }
                }
            }
        }
        for (page, (n, h, a)) in &per_page {
            if *a > 0 {
                println!("key: {} rows={} offered=0 always={}", esc(page), n, a);
            } else {
                println!("key: {} rows={} offered={}", esc(page), n, h);
            }
        }
        let mut last = format!("key ok rows={} offered={}", n_rows, n_off);
        if n_always > 0 {
            last.push_str(&format!(" offered_always={}", n_always));
        }
        last.push_str(&format!(" no_ticket={} no_line={}", no_ticket, no_line));
        if let Some(k) = key.listed {
            last.push_str(&format!(" listed={}", k));
        }
        println!("{}", last);
    }
    let mut last = format!(
        "replay ok tickets={} skipped_no_scope={} pages={} offered_total={} offered_max={} zero={}",
        runs.len(),
        skipped,
        pages.len(),
        total,
        maxo,
        zero
    );
    if always_pages > 0 {
        last.push_str(&format!(" always={}", always_pages));
    }
    if retired > 0 {
        last.push_str(&format!(" retired={}", retired));
    }
    println!("{}", last);
    0
}
