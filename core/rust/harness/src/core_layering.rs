//! Core-layering invariant (G4a), std only: no crate under `core/` may depend on code under `providers/`, `plugins/` or `mcp/`.
//! The graph comes from `cargo metadata` (the build's own view), never from a text scan. `graph` reads and normalises it once and
//! refuses anything it cannot trust; `judge` is pure over the result.

use std::path::Path;
use std::process::{Command, Stdio};

pub const GATE_ID: &str = "core-layering";
/// The ONE workspace the gate reads (PRE-3 layout). A second workspace is a follow-up.
pub const WORKSPACE_REL: &str = "core/rust";
/// The three adapter folders (G4a). Compared ASCII-case-insensitively on the FIRST repo-relative component of a dependency path.
pub const FORBIDDEN: [&str; 3] = ["providers", "plugins", "mcp"];

const MAX_STDOUT: usize = 1 << 20;
const STDERR_TAIL: usize = 600;
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub name: String,
    /// "normal" | "dev" | "build" (kind null -> "normal").
    pub kind: String,
    /// Some(repo-relative, canonical, '/'-joined) for path deps; None = registry.
    pub path_rel: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub manifest_rel: String,
    pub deps: Vec<Dep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Graph {
    pub members: Vec<Member>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    /// The first finding, deterministically (members in name order, deps in declared order).
    Violation { member: String, dep: String, kind: String, folder: String, dep_rel: String },
}

/// Pure over the graph: no subprocess, no filesystem. Clean only when no core member holds a forbidden edge.
pub fn judge(g: &Graph) -> Verdict {
    let mut members: Vec<&Member> = g.members.iter().collect();
    members.sort_by(|a, b| a.name.cmp(&b.name));
    for m in members {
        if m.manifest_rel.split('/').next() != Some("core") {
            continue;
        }
        for d in &m.deps {
            let rel = match &d.path_rel {
                Some(p) => p,
                None => continue,
            };
            let first = rel.split('/').next().unwrap_or("");
            if let Some(f) = FORBIDDEN.iter().find(|f| first.eq_ignore_ascii_case(f)) {
                return Verdict::Violation {
                    member: m.name.clone(),
                    dep: d.name.clone(),
                    kind: d.kind.clone(),
                    folder: (*f).to_string(),
                    dep_rel: rel.clone(),
                };
            }
        }
    }
    Verdict::Clean
}

/// Run `cargo metadata` ONCE (the only subprocess this module spawns), strict-read the consumed fields and normalise every consumed
/// path against the ONE root. Err = the graph could not be read or trusted; nothing else runs after it.
pub fn graph(repo_root: &Path) -> Result<Graph, String> {
    let root = repo_root.canonicalize().map_err(|_| "core-layering: repo root unreadable".to_string())?;
    let ws = root.join(WORKSPACE_REL);
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1", "--no-deps", "--offline", "--manifest-path"])
        .arg(ws.join("Cargo.toml"))
        .current_dir(&root)
        .stdin(Stdio::null());
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    let out = cmd.output().map_err(|e| format!("core-layering: cargo could not run: {}", e.kind()))?;
    if !out.status.success() {
        let tail = &out.stderr[out.stderr.len().saturating_sub(STDERR_TAIL)..];
        return Err(format!("core-layering: cargo metadata failed: {}", String::from_utf8_lossy(tail).trim()));
    }
    if out.stdout.len() > MAX_STDOUT {
        return Err("core-layering: cargo metadata output over the 1 MiB cap".to_string());
    }
    let text = String::from_utf8(out.stdout).map_err(|_| "core-layering: cargo metadata output is not utf-8".to_string())?;
    let doc = parse(&text).map_err(|e| format!("core-layering: cargo metadata output unreadable: {}", e))?;
    let top = obj(&doc, "document")?;

    let reported = text_of(req(top, "workspace_root")?, "workspace_root")?;
    let reported_canon = Path::new(reported)
        .canonicalize()
        .map_err(|_| "core-layering: workspace_root unreadable".to_string())?;
    let ws_canon = ws.canonicalize().map_err(|_| "core-layering: workspace folder unreadable".to_string())?;
    if reported_canon != ws_canon {
        return Err(format!(
            "core-layering: workspace root mismatch: cargo reports `{}` but expected `{}`",
            reported_canon.display(),
            ws_canon.display()
        ));
    }

    let pkgs = match req(top, "packages")? {
        J::Arr(a) => a,
        _ => return Err("core-layering: `packages` is not an array".to_string()),
    };
    let mut members: Vec<Member> = Vec::new();
    for p in pkgs {
        let po = obj(p, "package")?;
        let name = text_of(req(po, "name")?, "package name")?.to_string();
        let manifest_rel = rel_of(&root, text_of(req(po, "manifest_path")?, "manifest_path")?, &format!("manifest of `{}`", name))?;
        let deps_j = match req(po, "dependencies")? {
            J::Arr(a) => a,
            _ => return Err("core-layering: `dependencies` is not an array".to_string()),
        };
        let mut deps: Vec<Dep> = Vec::new();
        for d in deps_j {
            let dobj = obj(d, "dependency")?;
            let dname = text_of(req(dobj, "name")?, "dependency name")?.to_string();
            let kind = match req(dobj, "kind")? {
                J::Null => "normal".to_string(),
                J::Str(s) if s == "dev" || s == "build" => s.clone(),
                _ => return Err(format!("core-layering: dependency `{}` of `{}` has an unknown kind", dname, name)),
            };
            let path_rel = match field(dobj, "path")? {
                None | Some(J::Null) => None,
                Some(J::Str(s)) => Some(rel_of(&root, s, &format!("dependency `{}` of `{}`", dname, name))?),
                Some(_) => return Err(format!("core-layering: dependency `{}` of `{}` has a non-text path", dname, name)),
            };
            deps.push(Dep { name: dname, kind, path_rel });
        }
        members.push(Member { name, manifest_rel, deps });
    }
    members.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Graph { members })
}

/// Canonicalise through the filesystem and require the result under the canonical root; the repo-relative form comes from the
/// canonical value, never from cargo's spelling.
fn rel_of(root: &Path, raw: &str, what: &str) -> Result<String, String> {
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err(format!("core-layering: {} has a relative path", what));
    }
    let canon = p.canonicalize().map_err(|_| format!("core-layering: {} is unreadable", what))?;
    let rel = canon
        .strip_prefix(root)
        .map_err(|_| format!("core-layering: {} resolves outside the repo root", what))?;
    let mut parts: Vec<&str> = Vec::new();
    for c in rel.components() {
        parts.push(c.as_os_str().to_str().ok_or_else(|| format!("core-layering: {} has a non-utf-8 path", what))?);
    }
    Ok(parts.join("/"))
}

// ---- a minimal JSON reader: whole document parsed, only consumed fields are ever read ----

enum J {
    Null,
    Bool,
    Num,
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

fn obj<'a>(j: &'a J, what: &str) -> Result<&'a [(String, J)], String> {
    match j {
        J::Obj(o) => Ok(o),
        _ => Err(format!("core-layering: {} is not an object", what)),
    }
}

fn text_of<'a>(j: &'a J, what: &str) -> Result<&'a str, String> {
    match j {
        J::Str(s) => Ok(s),
        _ => Err(format!("core-layering: {} is not text", what)),
    }
}

/// A consumed key may appear at most once.
fn field<'a>(o: &'a [(String, J)], k: &str) -> Result<Option<&'a J>, String> {
    let mut it = o.iter().filter(|(n, _)| n == k);
    let first = it.next().map(|(_, v)| v);
    if it.next().is_some() {
        return Err(format!("core-layering: duplicate key `{}`", k));
    }
    Ok(first)
}

fn req<'a>(o: &'a [(String, J)], k: &str) -> Result<&'a J, String> {
    field(o, k)?.ok_or_else(|| format!("core-layering: key `{}` missing", k))
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

fn parse(s: &str) -> Result<J, String> {
    let mut p = P { b: s.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err("trailing bytes".to_string());
    }
    Ok(v)
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}'", c as char))
        }
    }

    fn lit(&mut self, word: &[u8], v: J) -> Result<J, String> {
        if self.b[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(v)
        } else {
            Err("bad literal".to_string())
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.b.get(self.i..self.i + 4).ok_or("short unicode escape")?;
        let s = std::str::from_utf8(h).map_err(|_| "bad unicode escape".to_string())?;
        if !h.iter().all(|c| c.is_ascii_hexdigit()) {
            return Err("bad unicode escape".to_string());
        }
        let n = u32::from_str_radix(s, 16).map_err(|_| "bad unicode escape".to_string())?;
        self.i += 4;
        Ok(n)
    }

    fn unicode(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        let cp = if (0xD800..0xDC00).contains(&hi) {
            if self.b.get(self.i) != Some(&b'\\') || self.b.get(self.i + 1) != Some(&b'u') {
                return Err("lone surrogate".to_string());
            }
            self.i += 2;
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err("lone surrogate".to_string());
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            hi
        };
        char::from_u32(cp).ok_or_else(|| "bad code point".to_string())
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("unterminated escape")?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => self.unicode()?,
                        _ => return Err("bad escape".to_string()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err("raw control byte in string".to_string()),
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid utf-8".to_string())
    }

    fn value(&mut self, depth: usize) -> Result<J, String> {
        if depth > MAX_DEPTH {
            return Err("nesting too deep".to_string());
        }
        self.ws();
        match self.b.get(self.i) {
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(b'n') => self.lit(b"null", J::Null),
            Some(b't') => self.lit(b"true", J::Bool),
            Some(b'f') => self.lit(b"false", J::Bool),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(J::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(J::Arr(items));
                        }
                        _ => return Err("expected ',' or ']'".to_string()),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(J::Obj(items));
                }
                loop {
                    let k = self.string()?;
                    self.eat(b':')?;
                    items.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(J::Obj(items));
                        }
                        _ => return Err("expected ',' or '}'".to_string()),
                    }
                }
            }
            Some(c) if c.is_ascii_digit() || *c == b'-' => {
                let st = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.i += 1;
                }
                if self.i == st {
                    return Err("bad number".to_string());
                }
                Ok(J::Num)
            }
            _ => Err("unexpected value".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reader_keeps_unknown_keys_and_refuses_broken_documents() {
        let j = parse(r#"{"a":[1,-2.5e3,true,false,null,{"x":"é\n"}],"b":{}}"#).unwrap();
        assert!(matches!(j, J::Obj(ref o) if o.len() == 2));
        for bad in ["", "{", r#"{"a":1,}"#, r#"{"a":1} x"#, r#"{"a":"\q"}"#, "[1 2]"] {
            assert!(parse(bad).is_err(), "should refuse {:?}", bad);
        }
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 5), "]".repeat(MAX_DEPTH + 5));
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn a_consumed_key_given_twice_is_refused() {
        let j = parse(r#"{"k":1,"k":2}"#).unwrap();
        let o = obj(&j, "t").unwrap();
        assert!(field(o, "k").is_err());
        assert!(req(o, "missing").is_err());
    }
}
