//! `harness lesson brief <lessons-dir> --ticket <ticket-file> --scope-file <file> --out <absolute-dir>`:
//! prints the lessons block of a builder brief on stdout and makes a copy of each matched page inside the
//! `--out` folder (see `docs/lesson-lint.md`, section `harness lesson brief`). Ticket mode only. The page
//! loop, the page rules and the matching are the lookup's own (the shared function of `lesson.rs`); this file
//! holds no copy of them. The privacy rules are the engine scanner's own (the deny list of the privacy gate,
//! the test mode off). std only.

use crate::friction_loop::load_list;
use crate::lesson::{esc, match_pages, read_ticket_scope, ListedPage, LESSON_USAGE};
use crate::scan::{self, Ctx};
use std::io::Write;
use std::path::{Component, Path};

/// Exit 2 with one `harness: error:` line, as the lookup prints it.
fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {}", esc(m));
    2
}

/// Exit 2 with the lesson usage text.
fn usage() -> i32 {
    eprintln!("{}", LESSON_USAGE);
    2
}

/// `harness lesson brief`. Stages, the first that fails prints its lines and ends the run: (1) the arguments
/// (exit 2), (2) the privacy list (exit 2), (3) a thing at the `--out` path (exit 1), (4) the lookup with its
/// own order (exit 2 or 1), (5) the NAME lines then the privacy hits (exit 1), (6) the out folder and the
/// copies (exit 2 on a failure), (7) the block on stdout, exit 0. All reads, checks and scans end before the
/// first write, so a refusal writes nothing. A failed copy ends the run at stage (6) before any stdout byte,
/// and a failed stdout write ends it at stage (7); each removes the copies the run made, then the out folder
/// (which stays when a file is left in it), and exits 2. A thing that appears at the `--out` path between
/// stages (3) and (6) ends the run as stage (3) does.
pub(crate) fn brief_cmd(a: &[String]) -> i32 {
    // (1) The arguments are read from left to right; the first bad one ends the run with the message of the
    // lookup (the diff mode flags are unknown options here).
    let (mut dir, mut ticket, mut scope, mut out) = (None, None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--ticket" => &mut ticket,
            "--scope-file" => &mut scope,
            "--out" => &mut out,
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
    let dir = match dir {
        Some(d) => d,
        None => return usage(),
    };
    let ticket = match ticket {
        Some(t) => t,
        None => return fail("--ticket is required"),
    };
    let scope = match scope {
        Some(s) => s,
        None => return fail("--scope-file is required"),
    };
    let out = match out {
        Some(o) => o,
        None => return fail("--out is required"),
    };
    let q = Path::new(out.as_str());
    if !q.is_absolute() || q.components().any(|c| matches!(c, Component::ParentDir)) || esc(out) != *out {
        return usage();
    }
    // (2) The privacy list, before a page, the ticket or the scope file is read.
    let list = match load_list("a lesson brief is not written unscanned") {
        Ok(l) => l,
        Err(c) => return c,
    };
    // (3) A thing found at the --out path refuses (a link is not followed); a path not found goes on.
    if std::fs::symlink_metadata(q).is_ok() {
        eprintln!("harness: lesson brief refused: the out folder exists already");
        return 1;
    }
    // (4) The lookup: its own reads, checks, refusals and exit codes.
    let input = match read_ticket_scope(ticket, scope) {
        Ok(i) => i,
        Err((c, ls)) => {
            for l in ls {
                eprintln!("{}", l);
            }
            return c;
        }
    };
    let answer = match match_pages(dir, &input) {
        Err((c, ls)) => {
            for l in ls {
                eprintln!("{}", l);
            }
            return c;
        }
        Ok(a) => a,
    };
    let always: Vec<&ListedPage> = answer.pages.iter().filter(|p| p.always).collect();
    let matched: Vec<&ListedPage> = answer.pages.iter().filter(|p| !p.always).collect();
    // (5) The file names the escape would rewrite, then the privacy hits (the fixed block lines, the folder
    // line and the text of a page that gets no copy are not scanned).
    let mut lines: Vec<String> = Vec::new();
    for p in &matched {
        if esc(&p.name) != p.name {
            lines.push(format!("harness: lesson brief refused: {}: NAME the file name cannot be printed as it is", p.id));
        }
    }
    let ctx = Ctx { list: &list, testmode: false };
    for p in &answer.pages {
        for h in scan::scan_text(&ctx, "", &p.line) {
            lines.push(format!("harness: lesson brief refused: {}: PRIVACY brief line {}", p.id, h.class));
        }
        if let Some(text) = &p.text {
            for h in scan::scan_text(&ctx, "", text) {
                lines.push(format!("harness: lesson brief refused: {}: PRIVACY page line {} {}", p.id, h.line, h.class));
            }
        }
    }
    if !lines.is_empty() {
        for l in &lines {
            eprintln!("{}", l);
        }
        return 1;
    }
    // (6) The out folder (a create call that fails when a thing is at the path; the parent is not made), then
    // one copy per matched page (a create call that fails when the name exists; the text as it was read).
    if let Err(e) = std::fs::create_dir(q) {
        // A thing that appeared at the path since stage (3) ends the run as stage (3) does.
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            eprintln!("harness: lesson brief refused: the out folder exists already");
            return 1;
        }
        return fail("the out folder cannot be made");
    }
    // A file counts as written once its create call worked, so a failed write is cleaned up too.
    let mut written: Vec<std::path::PathBuf> = Vec::new();
    let mut ok = true;
    for p in &matched {
        let path = q.join(&p.name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                written.push(path);
                if f.write_all(p.text.as_deref().unwrap_or("").as_bytes()).is_err() {
                    ok = false;
                }
            }
            Err(_) => ok = false,
        }
        if !ok {
            break;
        }
    }
    // A failed copy ends the run before any stdout byte.
    if !ok {
        return clean_up(&written, q);
    }
    // (7) The block, in one write; a failed write removes what the run wrote.
    let mut block = format!("Lessons for this ticket (harness lesson brief): always={} matched={}\n", always.len(), matched.len());
    if !always.is_empty() {
        block.push_str("Rules from earlier mistakes, kept for this change:\n");
        for p in &always {
            block.push_str(&p.line);
            block.push('\n');
        }
    }
    if !matched.is_empty() {
        block.push_str("Pages on earlier mistakes that match this ticket. Read each file before you write code.\n");
        block.push_str(&format!("folder: {}\n", out));
        for p in &matched {
            block.push_str(&p.line);
            block.push('\n');
        }
    }
    if std::io::stdout().lock().write_all(block.as_bytes()).is_err() {
        return clean_up(&written, q);
    }
    0
}

/// Removes the copies the run made, then the folder with a call that fails on a folder that is not empty;
/// exit 2 with the write line.
fn clean_up(written: &[std::path::PathBuf], q: &Path) -> i32 {
    for path in written.iter().rev() {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_dir(q);
    fail("the out folder cannot be written")
}
