//! Reading git state with read-only plumbing. Every failure is an error (the caller exits >= 2).
use crate::scan::{scan_lines, scan_text, Ctx, Hit};
use std::process::Command;

const MAX_BYTES: usize = 1024 * 1024;

fn git(args: &[&str]) -> Result<Vec<u8>, String> {
    let o = Command::new("git").args(args).output().map_err(|_| "git not runnable".to_string())?;
    if !o.status.success() {
        return Err("git command failed".to_string());
    }
    Ok(o.stdout)
}

fn utf8(b: Vec<u8>) -> Result<String, String> {
    String::from_utf8(b).map_err(|_| "invalid utf-8".to_string())
}

struct Added {
    path: String,
    lines: Vec<(usize, String)>,
    bytes: usize,
}

fn parse_diff(text: &str) -> Result<Vec<Added>, String> {
    let mut files: Vec<Added> = Vec::new();
    let mut in_hunk = false;
    let mut ln = 0usize;
    for l in text.lines() {
        if l.starts_with("diff --git ") {
            in_hunk = false;
            files.push(Added { path: String::new(), lines: Vec::new(), bytes: 0 });
            continue;
        }
        if !in_hunk {
            if l.starts_with("Binary files ") || l.starts_with("GIT binary patch") {
                return Err("binary file".to_string());
            }
            if let Some(p) = l.strip_prefix("+++ b/") {
                if let Some(f) = files.last_mut() {
                    f.path = p.to_string();
                }
            } else if let Some(h) = l.strip_prefix("@@ ") {
                let plus = h.split(" +").nth(1).ok_or("bad hunk")?;
                let num: String = plus.chars().take_while(|c| c.is_ascii_digit()).collect();
                ln = num.parse::<usize>().map_err(|_| "bad hunk".to_string())?;
                in_hunk = true;
            }
            continue;
        }
        if l.starts_with("@@ ") {
            let plus = l[3..].split(" +").nth(1).ok_or("bad hunk")?;
            let num: String = plus.chars().take_while(|c| c.is_ascii_digit()).collect();
            ln = num.parse::<usize>().map_err(|_| "bad hunk".to_string())?;
            continue;
        }
        if let Some(t) = l.strip_prefix('+') {
            if let Some(f) = files.last_mut() {
                f.bytes += t.len();
                if f.bytes > MAX_BYTES {
                    return Err("file too large".to_string());
                }
                f.lines.push((ln, t.to_string()));
            }
            ln += 1;
        } else if l.starts_with(' ') {
            ln += 1;
        }
    }
    Ok(files)
}

fn scan_diff(ctx: &Ctx, text: &str, out: &mut Vec<Hit>) -> Result<(), String> {
    for f in parse_diff(text)? {
        let name = if f.path.is_empty() { "unknown".to_string() } else { f.path.clone() };
        scan_lines(ctx, &name, &f.lines, out);
    }
    Ok(())
}

pub fn staged(ctx: &Ctx) -> Result<Vec<Hit>, String> {
    let d = utf8(git(&["-c", "core.quotepath=false", "diff", "--cached", "-U0", "--no-color", "--no-ext-diff", "--no-renames"])?)?;
    let mut out = Vec::new();
    scan_diff(ctx, &d, &mut out)?;
    Ok(out)
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

fn meta(ctx: &Ctx, sha: &str, out: &mut Vec<Hit>) -> Result<(), String> {
    let raw = utf8(git(&["log", "-1", "--format=%an%x00%ae%x00%cn%x00%ce%x00%B", sha])?)?;
    let parts: Vec<&str> = raw.splitn(5, '\0').collect();
    if parts.len() != 5 {
        return Err("bad metadata".to_string());
    }
    let s = short(sha);
    let fields: [(&str, String); 3] = [
        ("author", format!("{}\n{}", parts[0], parts[1])),
        ("committer", format!("{}\n{}", parts[2], parts[3])),
        ("message", parts[4].to_string()),
    ];
    for (f, t) in fields {
        for h in scan_text(ctx, &format!("{}.{}", s, f), &t) {
            let h = Hit { line: 1, ..h };
            if !out.contains(&h) {
                out.push(h);
            }
        }
    }
    Ok(())
}

/// Every commit `git rev-list <args>` lists: its tree diff and its metadata. Returns the hits and the commit count.
pub fn range(ctx: &Ctx, args: &[String]) -> Result<(Vec<Hit>, usize), String> {
    if args.is_empty() {
        return Err("no range given".to_string());
    }
    let mut a: Vec<&str> = vec!["rev-list"];
    a.extend(args.iter().map(|s| s.as_str()));
    let shas = utf8(git(&a)?)?;
    let mut out = Vec::new();
    let mut n = 0;
    for sha in shas.lines().filter(|l| !l.is_empty()) {
        n += 1;
        meta(ctx, sha, &mut out)?;
        let d = utf8(git(&[
            "-c", "core.quotepath=false", "diff-tree", "--root", "-r", "-m", "-p", "-U0", "--no-color", "--no-ext-diff", "--no-renames", "--no-commit-id", sha,
        ])?)?;
        scan_diff(ctx, &d, &mut out)?;
    }
    Ok((out, n))
}

pub fn tag(ctx: &Ctx, obj: &str) -> Result<Vec<Hit>, String> {
    let ty = utf8(git(&["cat-file", "-t", obj])?)?;
    if ty.trim() != "tag" {
        return Err("not a tag object".to_string());
    }
    let sha = utf8(git(&["rev-parse", obj])?)?;
    let body = utf8(git(&["cat-file", "-p", obj])?)?;
    let name = format!("{}.tag", short(sha.trim()));
    let mut out = Vec::new();
    for h in scan_text(ctx, &name, &body) {
        let h = Hit { line: 1, ..h };
        if !out.contains(&h) {
            out.push(h);
        }
    }
    Ok(out)
}

/// Every tracked file at HEAD. Returns the hits and the file count.
pub fn tree(ctx: &Ctx) -> Result<(Vec<Hit>, usize), String> {
    let list = git(&["ls-tree", "-r", "-z", "--name-only", "HEAD"])?;
    let mut out = Vec::new();
    let mut n = 0;
    for p in list.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8(p.to_vec()).map_err(|_| "invalid utf-8".to_string())?;
        let spec = format!("HEAD:{}", path);
        let blob = git(&["cat-file", "blob", &spec])?;
        if blob.len() > MAX_BYTES {
            return Err("file too large".to_string());
        }
        if blob.contains(&0) {
            return Err("binary file".to_string());
        }
        let text = utf8(blob)?;
        n += 1;
        let mut hits = scan_text(ctx, &path, &text);
        out.append(&mut hits);
    }
    Ok((out, n))
}
