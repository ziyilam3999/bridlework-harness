//! `harness ticket body-cites` (std only, Unix; `docs/body-cites.md` is the contract). Reads a PR body and checks
//! each strict line cite (`<sha>:<path>:<n>[-<m>] "<fragment>"`) against the file at the cite sha in the repo: the
//! sha must be --head or --base, the path a regular UTF-8 file, the range inside the file, its first and last line
//! not blank, and the fragment (compared without case) on one of its lines. It prints one line per cite, then a
//! count line. A `:<digits>` that looks like a line cite but is not a strict one is printed as an unparsed token.
//! It decides no review: a caller uses it as one input. It writes no file and runs no network call.
//! Git runs read only: no inherited GIT_ name, no stdin, no replace refs, no lazy fetch, raw blob bytes.
//! Exit codes: 0 no FLAG and unparsed=0; 1 at least one FLAG (after clearing) or unparsed > 0 or an unused
//! --cite-ok; 2 usage or setup error (one stderr line, nothing on stdout).

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::rc::Rc;

pub const USAGE: &str = "usage: harness ticket body-cites --repo-dir <absolute-dir> --head <full-sha> --base <full-sha> --body-file <absolute-file> [--cite-ok \"<cite or token>\"]...";

const MAX_BYTES: u64 = 1024 * 1024;

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {m}");
    2
}

struct Args {
    repo: PathBuf,
    head: String,
    base: String,
    body: PathBuf,
    cite_ok: Vec<String>,
}

fn abs_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

fn sha_ok(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse_args(a: &[String]) -> Option<Args> {
    let (mut repo, mut head, mut base, mut body): (Option<&str>, Option<&str>, Option<&str>, Option<&str>) = (None, None, None, None);
    let mut cite_ok = Vec::new();
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--repo-dir" => &mut repo,
            "--head" => &mut head,
            "--base" => &mut base,
            "--body-file" => &mut body,
            "--cite-ok" => {
                cite_ok.push(it.next()?.clone());
                continue;
            }
            _ => return None,
        };
        let v = it.next()?;
        if slot.replace(v.as_str()).is_some() {
            return None;
        }
    }
    let (repo, head, base, body) = (repo?, head?, base?, body?);
    if !abs_ok(repo) || !abs_ok(body) || !sha_ok(head) || !sha_ok(base) {
        return None;
    }
    Some(Args { repo: PathBuf::from(repo), head: head.into(), base: base.into(), body: PathBuf::from(body), cite_ok })
}

/// One read-only git call on the repo: no inherited GIT_ name, no stdin, no replace refs, no lazy fetch.
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

/// A strict cite found in one body line.
struct Cite<'a> {
    /// The cite as written in the body.
    text: &'a str,
    sha: &'a str,
    path: &'a str,
    n: u64,
    m: u64,
    frag: &'a str,
}

enum Item<'a> {
    Cite(Cite<'a>),
    /// An unparsed token, as written.
    Unparsed(String),
}

fn path_ok(p: &str) -> bool {
    !p.is_empty() && p.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
}

fn digits(b: &[u8], s: usize) -> Option<(u64, usize)> {
    let mut e = s;
    let mut v: u64 = 0;
    while e < b.len() && b[e].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add(u64::from(b[e] - b'0'));
        e += 1;
    }
    (e > s).then_some((v, e))
}

/// A strict cite that starts at byte `i` of `line` (a hex digit): sha of 7 to 40 hex characters, `:`, a repo
/// relative path, `:`, `<n>[-<m>]`, one space or tab, then a quoted fragment that is not empty.
fn cite_at(line: &str, i: usize) -> Option<Cite<'_>> {
    let b = line.as_bytes();
    let mut j = i;
    while j < b.len() && b[j].is_ascii_hexdigit() {
        j += 1;
    }
    if !(7..=40).contains(&(j - i)) || b.get(j) != Some(&b':') {
        return None;
    }
    let p = j + 1;
    let mut k = p;
    while k < b.len() && b[k] != b':' {
        if b[k].is_ascii_whitespace() || b[k] == b'"' || b[k] == b'`' {
            return None;
        }
        k += 1;
    }
    if k >= b.len() || !path_ok(&line[p..k]) {
        return None;
    }
    let (n, mut e) = digits(b, k + 1)?;
    let mut m = n;
    if b.get(e) == Some(&b'-') {
        if let Some((mm, e2)) = digits(b, e + 1) {
            m = mm;
            e = e2;
        }
    }
    if !matches!(b.get(e), Some(b' ' | b'\t')) || b.get(e + 1) != Some(&b'"') {
        return None;
    }
    let fs = e + 2;
    let q = fs + line[fs..].find('"')?;
    if q == fs {
        return None;
    }
    Some(Cite { text: &line[i..=q], sha: &line[i..j], path: &line[p..k], n, m, frag: &line[fs..q] })
}

fn is_delim(c: char) -> bool {
    matches!(c, ' ' | '\t' | '"' | '\'' | '`' | '(' | ')' | '[' | ']' | ',' | '\u{201c}' | '\u{201d}' | '\u{2018}' | '\u{2019}')
}

/// The unparsed tokens of one text segment (a line part with the strict cites taken out): each `:<digits>` whose
/// name (the run before the colon back to a delimiter) holds a `.` or a `/`, or whose character before the colon
/// is not a letter or digit. A clock time such as 16:35 has neither, so it does not count.
fn unparsed(seg: &str, items: &mut Vec<Item<'_>>) {
    let c: Vec<char> = seg.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i] == ':' && c.get(i + 1).is_some_and(char::is_ascii_digit) {
            let mut j = i + 1;
            while j < c.len() && c[j].is_ascii_digit() {
                j += 1;
            }
            if c.get(j) == Some(&'-') && c.get(j + 1).is_some_and(char::is_ascii_digit) {
                j += 1;
                while j < c.len() && c[j].is_ascii_digit() {
                    j += 1;
                }
            }
            let mut s = i;
            while s > 0 && !is_delim(c[s - 1]) {
                s -= 1;
            }
            let bad = c[s..i].iter().any(|&x| x == '.' || x == '/') || i == 0 || !c[i - 1].is_alphanumeric();
            if bad {
                items.push(Item::Unparsed(c[s..j].iter().collect()));
            }
            i = j;
            continue;
        }
        i += 1;
    }
}

fn scan(body: &str) -> Vec<Item<'_>> {
    let mut items = Vec::new();
    for line in body.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let b = line.as_bytes();
        let (mut seg, mut i) = (0, 0);
        while i < b.len() {
            if b[i].is_ascii_hexdigit() && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
                if let Some(c) = cite_at(line, i) {
                    unparsed(&line[seg..i], &mut items);
                    let end = i + c.text.len();
                    items.push(Item::Cite(c));
                    i = end;
                    seg = end;
                    continue;
                }
            }
            i += 1;
        }
        unparsed(&line[seg..], &mut items);
    }
    items
}

struct Entry {
    mode: String,
    oid: String,
    size: Option<u64>,
}

/// Every blob and submodule entry of one commit, and every folder that holds one.
struct Listing {
    files: HashMap<String, Entry>,
    dirs: HashSet<String>,
}

fn list_tree(repo: &Path, sha: &str) -> Option<Listing> {
    let raw = git_ok(repo, &["ls-tree", "-r", "-z", "-l", "--full-tree", sha])?;
    let (mut files, mut dirs) = (HashMap::new(), HashSet::new());
    for rec in raw.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        let tab = rec.iter().position(|&b| b == b'\t')?;
        let meta = std::str::from_utf8(&rec[..tab]).ok()?;
        let Ok(path) = std::str::from_utf8(&rec[tab + 1..]) else { continue };
        let f: Vec<&str> = meta.split_whitespace().collect();
        if f.len() != 4 {
            return None;
        }
        let mut p = path;
        while let Some(k) = p.rfind('/') {
            p = &p[..k];
            if !dirs.insert(p.to_string()) {
                break;
            }
        }
        files.insert(path.to_string(), Entry { mode: f[0].into(), oid: f[2].into(), size: f[3].parse().ok() });
    }
    Some(Listing { files, dirs })
}

struct Ctx<'a> {
    repo: &'a Path,
    head: &'a str,
    base: &'a str,
    listings: HashMap<String, Listing>,
    blobs: HashMap<String, Option<Rc<String>>>,
}

impl Ctx<'_> {
    /// The text of the file at the cite sha, or the reason it cannot be read. Err: a git call failed.
    fn text(&mut self, c: &Cite<'_>) -> Result<Result<Rc<String>, &'static str>, ()> {
        let sha = c.sha.to_ascii_lowercase();
        let full = if self.head.starts_with(&sha) {
            self.head
        } else if self.base.starts_with(&sha) {
            self.base
        } else {
            return Ok(Err("sha-not-head-or-base"));
        };
        if !self.listings.contains_key(full) {
            let l = list_tree(self.repo, full).ok_or(())?;
            self.listings.insert(full.to_string(), l);
        }
        let Some(e) = self.listings[full].files.get(c.path) else {
            return Ok(Err(if self.listings[full].dirs.contains(c.path) { "not-a-file" } else { "file-missing" }));
        };
        let oid = match (e.mode.starts_with("100"), e.size) {
            (true, Some(s)) if s <= MAX_BYTES => e.oid.clone(),
            _ => return Ok(Err("not-a-file")),
        };
        if !self.blobs.contains_key(&oid) {
            let t = git_ok(self.repo, &["cat-file", "blob", &oid]).ok_or(())?;
            self.blobs.insert(oid.clone(), String::from_utf8(t).ok().map(Rc::new));
        }
        Ok(self.blobs[&oid].clone().ok_or("not-a-file"))
    }

    /// The reasons a cite is flagged, in the fixed order; empty: the cite checks.
    fn reasons(&mut self, c: &Cite<'_>) -> Result<Vec<&'static str>, ()> {
        let text = match self.text(c)? {
            Ok(t) => t,
            Err(r) => return Ok(vec![r]),
        };
        let mut lines: Vec<&str> = text.split('\n').collect();
        if text.is_empty() || text.ends_with('\n') {
            lines.pop();
        }
        if c.n < 1 || c.m < c.n || c.m > lines.len() as u64 {
            return Ok(vec!["out-of-range"]);
        }
        let range = &lines[(c.n - 1) as usize..c.m as usize];
        let blank = |l: &str| l.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\r'));
        let mut r = Vec::new();
        if blank(range[0]) || blank(range[range.len() - 1]) {
            r.push("blank-line");
        }
        let f = c.frag.to_lowercase();
        if !range.iter().any(|l| l.to_lowercase().contains(&f)) {
            r.push("fragment-absent");
        }
        Ok(r)
    }
}

/// The body file: a regular file (not a link), at most 1 MiB, UTF-8.
fn read_body(p: &Path) -> Result<String, &'static str> {
    let md = std::fs::symlink_metadata(p).map_err(|_| "the body file is missing")?;
    if md.file_type().is_symlink() {
        return Err("the body file is a link");
    }
    if !md.is_file() {
        return Err("the body file is not a regular file");
    }
    if md.len() > MAX_BYTES {
        return Err("the body file is over 1048576 bytes");
    }
    let mut buf = Vec::new();
    let read = std::fs::File::open(p).and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut buf));
    if read.is_err() {
        return Err("the body file cannot be read");
    }
    if buf.len() as u64 > MAX_BYTES {
        return Err("the body file is over 1048576 bytes");
    }
    String::from_utf8(buf).map_err(|_| "the body file is not UTF-8")
}

pub fn body_cites_cmd(a: &[String]) -> i32 {
    let Some(args) = parse_args(a) else { return usage() };
    let repo = args.repo.as_path();
    if !std::fs::metadata(repo).map(|m| m.is_dir()).unwrap_or(false) {
        return fail("the repo dir is missing or not a folder");
    }
    match git(repo, &["rev-parse", "--is-inside-work-tree"]) {
        None => return fail("git cannot run"),
        Some(o) if o.status.success() && o.stdout == b"true\n" => {}
        Some(_) => return fail("the repo dir is not a git work tree"),
    }
    for (flag, sha) in [("--head", &args.head), ("--base", &args.base)] {
        match git(repo, &["cat-file", "-t", sha]) {
            None => return fail("git cannot run"),
            Some(o) if o.status.success() && o.stdout == b"commit\n" => {}
            Some(_) => return fail(&format!("{flag} is not a commit of the repo")),
        }
    }
    let body = match read_body(&args.body) {
        Ok(b) => b,
        Err(m) => return fail(m),
    };
    let mut cx = Ctx { repo, head: &args.head, base: &args.base, listings: HashMap::new(), blobs: HashMap::new() };
    let mut used = vec![false; args.cite_ok.len()];
    let (mut cites, mut ok, mut flag, mut unp) = (0usize, 0usize, 0usize, 0usize);
    let mut out = String::new();
    for item in scan(&body) {
        let (text, reasons) = match &item {
            Item::Cite(c) => {
                cites += 1;
                match cx.reasons(c) {
                    Ok(r) => (c.text, r),
                    Err(()) => return fail("a git call failed"),
                }
            }
            Item::Unparsed(t) => (t.as_str(), vec!["unparsed"]),
        };
        let is_cite = matches!(item, Item::Cite(_));
        if reasons.is_empty() {
            ok += 1;
            out.push_str(&format!("OK {text}\n"));
            continue;
        }
        let mut cleared = false;
        for (i, v) in args.cite_ok.iter().enumerate() {
            if v == text {
                used[i] = true;
                cleared = true;
            }
        }
        if cleared {
            out.push_str(&format!("CLEARED {text}\n"));
            continue;
        }
        if is_cite {
            flag += 1;
        } else {
            unp += 1;
        }
        out.push_str(&format!("FLAG {} {text}\n", reasons.join(",")));
    }
    let mut unused = 0;
    for (v, u) in args.cite_ok.iter().zip(&used) {
        if !u {
            unused += 1;
            out.push_str(&format!("FLAG cite-ok-unused {v}\n"));
        }
    }
    out.push_str(&format!("cites={cites} ok={ok} flag={flag} unparsed={unp}\n"));
    print!("{out}");
    i32::from(flag > 0 || unp > 0 || unused > 0)
}
