//! `harness ticket words-only` (std only, Unix; `docs/words-only.md` is the contract). Reads two commits of one
//! repo and exits 0 when each change between them is plain comment text in a Rust file or text of a docs Markdown
//! file; else it refuses. Plain comments (`//`, `/* */`) are removed; each doc comment (`///`, `//!`, `/** */`,
//! `/*! */`) is kept as code, whole, so a doc comment change is code-changed (rustdoc may run its text as a
//! doctest). It decides no review and marks no finding: a caller uses it as one input. It does not replace the
//! gate (some checks read comment and docs text), it reads shell files as not-words-file, it reads two commits
//! and not the work tree, and a rebased fix round is refused as not-ancestor.
//! Git runs read only: no inherited GIT_ name, no stdin, no replace refs, raw tree compare, raw blob bytes.
//! Exit codes: 0 words only; 1 refused (one stderr line); 2 usage or setup error (one stderr line, no path).

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const USAGE: &str = "usage: harness ticket words-only --repo-dir <absolute-dir> --from <full-sha> --to <full-sha>";

const MAX_ENTRIES: usize = 200;
const MAX_BLOB: u64 = 1024 * 1024;
const ROOT_WORDS: [&str; 3] = ["README.md", "CONTRIBUTING.md", "SECURITY.md"];

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {m}");
    2
}

fn refuse(path: &str, class: &str) -> i32 {
    eprintln!("harness: words-only refused: {path} {class}");
    1
}

struct Args {
    repo: PathBuf,
    from: String,
    to: String,
}

fn abs_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

fn sha_ok(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse_args(a: &[String]) -> Option<Args> {
    let (mut repo, mut from, mut to): (Option<&str>, Option<&str>, Option<&str>) = (None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--repo-dir" => &mut repo,
            "--from" => &mut from,
            "--to" => &mut to,
            _ => return None,
        };
        let v = it.next()?;
        if slot.replace(v.as_str()).is_some() {
            return None;
        }
    }
    let (repo, from, to) = (repo?, from?, to?);
    if !abs_ok(repo) || !sha_ok(from) || !sha_ok(to) {
        return None;
    }
    Some(Args { repo: PathBuf::from(repo), from: from.to_string(), to: to.to_string() })
}

/// One read-only git call on the repo: no inherited GIT_ name, no stdin, no replace refs. None: git cannot run.
fn git(repo: &Path, args: &[&str]) -> Option<Output> {
    let mut c = Command::new("git");
    c.arg("--no-replace-objects").arg("-C").arg(repo).args(args);
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

/// One changed entry of the raw tree compare.
struct Entry {
    old_mode: String,
    new_mode: String,
    old_oid: String,
    new_oid: String,
    status: String,
    path: Vec<u8>,
}

fn parse_entries(raw: &[u8]) -> Option<Vec<Entry>> {
    let mut toks = raw.split(|&b| b == 0).peekable();
    let mut out = Vec::new();
    while let Some(head) = toks.next() {
        if head.is_empty() && toks.peek().is_none() {
            break;
        }
        let head = std::str::from_utf8(head).ok()?.strip_prefix(':')?;
        let f: Vec<&str> = head.split(' ').collect();
        if f.len() != 5 {
            return None;
        }
        let path = toks.next()?.to_vec();
        out.push(Entry { old_mode: f[0].into(), new_mode: f[1].into(), old_oid: f[2].into(), new_oid: f[3].into(), status: f[4].into(), path });
    }
    Some(out)
}

fn is_words_path(p: &str) -> bool {
    p.ends_with(".rs") || (p.starts_with("docs/") && p.ends_with(".md")) || ROOT_WORDS.contains(&p)
}

fn blob_size(repo: &Path, oid: &str) -> Option<u64> {
    String::from_utf8(git_ok(repo, &["cat-file", "-s", oid])?).ok()?.trim().parse().ok()
}

fn blob(repo: &Path, oid: &str) -> Option<Vec<u8>> {
    git_ok(repo, &["cat-file", "blob", oid])
}

fn is_blank(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn utf8_len(b: u8) -> usize {
    match b {
        0xF0.. => 4,
        0xE0.. => 3,
        0xC0.. => 2,
        _ => 1,
    }
}

/// End of a string literal that opens at `i` (a quote): one past the closing quote. None: unterminated.
fn string_end(s: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    loop {
        match *s.get(j)? {
            b'\\' => j += 2,
            b'"' => return Some(j + 1),
            _ => j += 1,
        }
    }
}

/// A quote at `i`: Some(Some(end)) a char literal, Some(None) a lifetime or label, None an unterminated literal.
fn char_end(s: &[u8], i: usize) -> Option<Option<usize>> {
    let n = *s.get(i + 1)?;
    if n == b'\\' {
        let mut j = i + 3;
        loop {
            match *s.get(j)? {
                b'\'' => return Some(Some(j + 1)),
                b'\n' => return None,
                _ => j += 1,
            }
        }
    }
    let l = utf8_len(n);
    if s.get(i + 1 + l) == Some(&b'\'') {
        return Some(Some(i + 2 + l));
    }
    if n.is_ascii_alphabetic() || n == b'_' || n >= 0x80 {
        Some(None)
    } else {
        None
    }
}

/// End of a raw string whose prefix word ends at `j`: one past the closing quote and hashes. Outer None: not a
/// raw string (a raw identifier such as r#type). Inner None: unterminated.
fn raw_end(s: &[u8], j: usize) -> Option<Option<usize>> {
    let k = s[j..].iter().take_while(|&&b| b == b'#').count();
    if s.get(j + k) != Some(&b'"') {
        return None;
    }
    let mut p = j + k + 1;
    while p < s.len() {
        if s[p] == b'"' && s.get(p + 1..p + 1 + k).is_some_and(|h| h.iter().all(|&b| b == b'#')) {
            return Some(Some(p + 1 + k));
        }
        p += 1;
    }
    Some(None)
}

struct Lexer<'a> {
    s: &'a [u8],
    out: Vec<u8>,
    line_start: usize,
    held: bool,
}

impl Lexer<'_> {
    /// A code newline: a line that held comment text and has only blanks left is dropped with its newline.
    fn newline(&mut self) {
        if self.dropped_line() {
            self.out.truncate(self.line_start);
        } else {
            self.out.push(b'\n');
        }
        self.line_start = self.out.len();
        self.held = false;
    }

    fn dropped_line(&self) -> bool {
        let body = &self.out[self.line_start..];
        let body = body.strip_suffix(b"\r").unwrap_or(body);
        self.held && body.iter().all(|&b| is_blank(b))
    }

    /// Copies a kept span (a literal or a kept comment) as it is; a newline inside it is not dropped.
    fn lit(&mut self, seg: &[u8]) {
        for &b in seg {
            self.out.push(b);
            if b == b'\n' {
                self.line_start = self.out.len();
                self.held = false;
            }
        }
    }

    /// A comment starts here: the blanks before it on its line go.
    fn strip_start(&mut self) {
        while self.out.len() > self.line_start && self.out.last().is_some_and(|&b| is_blank(b)) {
            self.out.pop();
        }
        self.held = true;
    }

    /// A line comment at `i`; returns where lexing goes on (the line end, a CR before an LF stays code).
    fn line_comment(&mut self, i: usize) -> usize {
        let s = self.s;
        let mut e = s[i..].iter().position(|&b| b == b'\n').map_or(s.len(), |p| i + p);
        if e < s.len() && s[e - 1] == b'\r' {
            e -= 1;
        }
        let doc = s.get(i + 2) == Some(&b'!') || (s.get(i + 2) == Some(&b'/') && s.get(i + 3) != Some(&b'/'));
        if !doc {
            self.strip_start();
            return e;
        }
        // A doc comment is kept whole: rustdoc may run its text as a doctest.
        self.out.extend_from_slice(&s[i..e]);
        e
    }

    /// A block comment at `i`; returns where lexing goes on. None: unterminated.
    fn block_comment(&mut self, i: usize) -> Option<usize> {
        let s = self.s;
        let (mut j, mut depth) = (i + 2, 1);
        while depth > 0 {
            match (*s.get(j)?, s.get(j + 1).copied()) {
                (b'/', Some(b'*')) => {
                    depth += 1;
                    j += 2;
                }
                (b'*', Some(b'/')) => {
                    depth -= 1;
                    j += 2;
                }
                _ => j += 1,
            }
        }
        let doc = s.get(i + 2) == Some(&b'!') || (s.get(i + 2) == Some(&b'*') && !matches!(s.get(i + 3), Some(b'*' | b'/')));
        if doc {
            self.lit(&s[i..j]);
            return Some(j);
        }
        self.strip_start();
        let after = s.get(j).is_some_and(|&b| !is_blank(b) && b != b'\n' && b != b'\r');
        if self.out.len() > self.line_start && after {
            self.out.push(b' ');
        }
        Some(j)
    }
}

/// Reads Rust source and returns it with plain comments removed; each doc comment (line or block) is kept as
/// code, whole. None: the lexer cannot read it to its end.
fn strip_comments(s: &[u8]) -> Option<Vec<u8>> {
    let mut x = Lexer { s, out: Vec::with_capacity(s.len()), line_start: 0, held: false };
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            b'\n' => {
                x.newline();
                i += 1;
            }
            b'/' if s.get(i + 1) == Some(&b'/') => i = x.line_comment(i),
            b'/' if s.get(i + 1) == Some(&b'*') => i = x.block_comment(i)?,
            b'"' => {
                let e = string_end(s, i)?;
                x.lit(&s[i..e]);
                i = e;
            }
            b'\'' => match char_end(s, i)? {
                Some(e) => {
                    x.lit(&s[i..e]);
                    i = e;
                }
                None => {
                    x.out.push(c);
                    i += 1;
                }
            },
            _ if ident_byte(c) => {
                let mut j = i;
                while j < s.len() && ident_byte(s[j]) {
                    j += 1;
                }
                let word = &s[i..j];
                if matches!(word, b"r" | b"br" | b"cr") {
                    if let Some(end) = raw_end(s, j) {
                        let end = end?;
                        x.lit(&s[i..end]);
                        i = end;
                        continue;
                    }
                }
                x.out.extend_from_slice(word);
                i = j;
            }
            _ => {
                x.out.push(c);
                i += 1;
            }
        }
    }
    if x.dropped_line() {
        x.out.truncate(x.line_start);
    }
    Some(x.out)
}

fn path_bad(p: &[u8]) -> bool {
    match std::str::from_utf8(p) {
        Ok(s) => s.chars().any(|c| c.is_control()),
        Err(_) => true,
    }
}

pub fn words_only_cmd(a: &[String]) -> i32 {
    let Some(args) = parse_args(a) else { return usage() };
    let repo = args.repo.as_path();
    if !std::fs::metadata(repo).map(|m| m.is_dir()).unwrap_or(false) {
        return fail("the repo dir is missing or not a folder");
    }
    match git(repo, &["rev-parse", "--git-dir"]) {
        None => return fail("git cannot run"),
        Some(o) if !o.status.success() => return fail("the repo dir is not a git repo"),
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
        return refuse("-", "no-change");
    }
    match git(repo, &["merge-base", "--is-ancestor", &args.from, &args.to]) {
        None => return fail("git cannot run"),
        Some(o) => match o.status.code() {
            Some(0) => {}
            Some(1) => return refuse("-", "not-ancestor"),
            _ => return fail("a git call failed"),
        },
    }
    let Some(raw) = git_ok(repo, &["diff-tree", "-r", "-z", "--raw", "--no-renames", &args.from, &args.to]) else {
        return fail("a git call failed");
    };
    let Some(entries) = parse_entries(&raw) else { return fail("the tree compare could not be read") };
    if entries.len() > MAX_ENTRIES {
        return refuse("-", "too-many-files");
    }
    for e in &entries {
        if let Some(class) = check_entry(repo, e) {
            return match class {
                Err(()) => fail("a git call failed"),
                Ok("bad-path") => refuse("<unprintable path>", "bad-path"),
                Ok(c) => refuse(&String::from_utf8_lossy(&e.path), c),
            };
        }
    }
    println!("words-only yes from={} to={} files={}", args.from, args.to, entries.len());
    0
}

/// None: the entry is words only. Some(Ok(class)): refused. Some(Err): a git call failed.
fn check_entry(repo: &Path, e: &Entry) -> Option<Result<&'static str, ()>> {
    let Some(path) = std::str::from_utf8(&e.path).ok().filter(|_| !path_bad(&e.path)) else { return Some(Ok("bad-path")) };
    if e.status == "A" {
        return Some(Ok("new-file"));
    }
    if e.status == "D" {
        return Some(Ok("deleted-file"));
    }
    if e.status == "T" || e.old_mode != e.new_mode {
        return Some(Ok("mode"));
    }
    if e.old_mode == "120000" || e.new_mode == "120000" {
        return Some(Ok("link"));
    }
    if e.old_mode == "160000" || e.new_mode == "160000" {
        return Some(Ok("submodule"));
    }
    if !is_words_path(path) {
        return Some(Ok("not-words-file"));
    }
    let (Some(a), Some(b)) = (blob_size(repo, &e.old_oid), blob_size(repo, &e.new_oid)) else { return Some(Err(())) };
    if a > MAX_BLOB || b > MAX_BLOB {
        return Some(Ok("too-large"));
    }
    let (Some(old), Some(new)) = (blob(repo, &e.old_oid), blob(repo, &e.new_oid)) else { return Some(Err(())) };
    if std::str::from_utf8(&old).is_err() || std::str::from_utf8(&new).is_err() {
        return Some(Ok("not-utf8"));
    }
    if path.ends_with(".md") {
        return None;
    }
    match (strip_comments(&old), strip_comments(&new)) {
        (Some(x), Some(y)) if x == y => None,
        (Some(_), Some(_)) => Some(Ok("code-changed")),
        _ => Some(Ok("unparsed")),
    }
}
