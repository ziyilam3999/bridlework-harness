//! `harness lesson replay <lessons-dir> --tickets <absolute-dir> --scopes <absolute-dir>`: replays the
//! ticket-mode lookup over a folder of tickets and counts the offered pages (see `docs/lesson-lint.md`).
//! Read only: the run writes no file in the lessons folder, the tickets folder, the scopes folder or the
//! current folder. The pages are read once as the lookup reads them, then matched against each ticket.
//! Ticket mode only. std only.

use crate::lesson::{esc, front_close, front_pairs, match_tokens, status_of, token_hits, walk, LESSON_USAGE, MAX_FILE};
use crate::ticket::Ticket;
use std::path::{Component, Path, PathBuf};

/// One ticket of the run: the id (the file name without `.json`) and its path.
struct TicketFile {
    id: String,
    path: PathBuf,
}

/// One page read once: its file name, its id, its tokens and its retired flag.
struct Page {
    id: String,
    retired: bool,
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
        let mut problems = Vec::new();
        let close = front_close(&lines, &mut problems);
        let mut is_retired = false;
        let tokens = match close {
            Some(close) => {
                let keys = front_pairs(&lines, close, &mut problems);
                // STATUS before MATCH, as the lookup orders them; a missing key reads as active.
                match status_of(&keys) {
                    Ok(s) => is_retired = s == Some("retired"),
                    Err(p) => problems.push(p),
                }
                match match_tokens(&keys) {
                    Ok(t) => t,
                    Err(p) => {
                        problems.push(p);
                        None
                    }
                }
            }
            None => None,
        };
        let tokens: Vec<String> = tokens.unwrap_or_default().into_iter().map(|t| t.trim_matches(['"', '\'']).to_string()).collect();
        if tokens.iter().any(|t| t.is_empty()) {
            problems.push("MATCH match has a token with no text between its quotes".to_string());
        }
        for p in &problems {
            refused.push(format!("harness: lesson lookup refused: {}: {}", esc(name), esc(p)));
        }
        if close.is_some() && is_retired {
            retired += 1;
        }
        pages.push(Page { id: name[..9].to_string(), retired: is_retired, tokens });
    }
    Ok((pages, retired))
}

/// `harness lesson replay <lessons-dir> --tickets <absolute-dir> --scopes <absolute-dir>`: exit 0 the counts
/// (one `replay:` line per ticket, one `page:` line per page, then the `replay ok` line); 1 a refused page,
/// ticket file or scope file (stdout empty, no partial count); 2 a usage or read error.
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
    let mut it = a.iter();
    while let Some(arg) = it.next() {
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
    }
    for l in lines {
        println!("{}", l);
    }
    for (i, pg) in pages.iter().enumerate() {
        println!("page: {} hits={}", esc(&pg.id), hits[i]);
    }
    if retired > 0 {
        println!(
            "replay ok tickets={} skipped_no_scope={} pages={} offered_total={} offered_max={} zero={} retired={}",
            runs.len(),
            skipped,
            pages.len(),
            total,
            maxo,
            zero,
            retired
        );
    } else {
        println!(
            "replay ok tickets={} skipped_no_scope={} pages={} offered_total={} offered_max={} zero={}",
            runs.len(),
            skipped,
            pages.len(),
            total,
            maxo,
            zero
        );
    }
    0
}
