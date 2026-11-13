//! The control registry (std only). `controls/registry.json` lists every control: what it guards against, how it is tested, who owns
//! it and when it is removed. `schemas/control.schema.json` mirrors this loader and a test pins the two together.
//! Loading fails closed: a refusal names the row (its id, or `#<index>` when the id is unreadable) and the field.

use std::io::Read;
use std::path::Path;

/// The eight required row keys. The schema's row `required` set equals this one; a test pins the two together.
pub const FIELDS: [&str; 8] = ["id", "kind", "class", "enabled", "failure", "test", "retirement_test", "owner"];
pub const KINDS: [&str; 1] = ["hook"];
pub const CLASSES: [&str; 3] = ["policy", "crutch", "orchestration"];
pub const OWNERS: [&str; 6] =
    ["orchestration_portfolio", "execution_verification", "risk_review", "friction_learning", "knowledge_lessons", "harness_governance"];
pub const EVENTS: [&str; 9] =
    ["PreToolUse", "PostToolUse", "UserPromptSubmit", "Stop", "SubagentStop", "SessionStart", "SessionEnd", "Notification", "PreCompact"];
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_DEPTH: usize = 8;
const HOOK_DIR: &str = "plugins/claude";

/// The hook part of a row: what P11-03 turns into one hooks.json entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub script: String,
    pub manifest: String,
    pub event: String,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub id: String,
    pub kind: String,
    pub class: String,
    pub baseline_ref: Option<String>,
    pub enabled: bool,
    pub failure: String,
    pub test: String,
    pub retirement_test: String,
    pub owner: String,
    pub hook: Option<Hook>,
}

#[derive(Debug)]
pub struct Registry {
    pub rows: Vec<Control>,
}

#[derive(Debug, Clone)]
enum V {
    S(String),
    B(bool),
    N(u64),
    A(Vec<V>),
    O(Vec<(String, V)>),
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}'", c as char))
        }
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
                        b'u' => {
                            let h = self.b.get(self.i..self.i + 4).ok_or("short unicode escape")?;
                            let s = std::str::from_utf8(h).map_err(|_| "bad unicode escape".to_string())?;
                            if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                                return Err("bad unicode escape".to_string());
                            }
                            self.i += 4;
                            char::from_u32(u32::from_str_radix(s, 16).map_err(|_| "bad unicode escape".to_string())?).ok_or("lone surrogate")?
                        }
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

    fn number(&mut self) -> Result<u64, String> {
        let st = self.i;
        while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if st == self.i {
            return Err("not a plain non-negative integer".to_string());
        }
        if self.i - st > 1 && self.b[st] == b'0' {
            return Err("leading zero".to_string());
        }
        std::str::from_utf8(&self.b[st..self.i]).unwrap().parse::<u64>().map_err(|_| "number out of range".to_string())
    }

    fn lit(&mut self, word: &[u8]) -> bool {
        if self.b[self.i..].starts_with(word) {
            self.i += word.len();
            true
        } else {
            false
        }
    }

    /// One value. Nesting is capped (`MAX_DEPTH`) so a hostile file cannot recurse without bound. A duplicate key in one object is refused
    /// here and names the key. `null` and fractions are not part of the contract and are refused as a bad value.
    fn value(&mut self, depth: usize) -> Result<V, String> {
        if depth > MAX_DEPTH {
            return Err("nested too deeply".to_string());
        }
        match self.peek() {
            Some(b'"') => Ok(V::S(self.string()?)),
            Some(b'{') => {
                self.i += 1;
                let mut kv: Vec<(String, V)> = Vec::new();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(V::O(kv));
                }
                loop {
                    let k = self.string().map_err(|e| format!("key: {}", e))?;
                    self.eat(b':').map_err(|e| format!("`{}`: {}", k, e))?;
                    let v = self.value(depth + 1).map_err(|e| if e.starts_with('`') { e } else { format!("`{}`: {}", k, e) })?;
                    if kv.iter().any(|(x, _)| *x == k) {
                        return Err(format!("duplicate key `{}`", k));
                    }
                    kv.push((k.clone(), v));
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(V::O(kv));
                        }
                        _ => return Err(format!("`{}`: expected ',' or '}}'", k)),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut v = Vec::new();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(V::A(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(V::A(v));
                        }
                        _ => return Err("expected ',' or ']'".to_string()),
                    }
                }
            }
            Some(b'0'..=b'9') => Ok(V::N(self.number()?)),
            Some(_) if self.lit(b"true") => Ok(V::B(true)),
            Some(_) if self.lit(b"false") => Ok(V::B(false)),
            _ => Err("not a JSON value".to_string()),
        }
    }
}

fn parse_doc(text: &str) -> Result<V, String> {
    let mut p = P { b: text.as_bytes(), i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err("trailing bytes".to_string());
    }
    Ok(v)
}

fn id_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

/// A plain repo-relative path: `/`-separated, every component a plain name, no `.git` in any case.
fn path_ok(p: &str) -> bool {
    !p.chars().any(|c| c == '\\' || c.is_control())
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.eq_ignore_ascii_case(".git"))
}

/// The value under `key` in an object's pairs, if any.
fn take<'a>(kv: &'a [(String, V)], key: &str) -> Option<&'a V> {
    kv.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn str_of(kv: &[(String, V)], key: &str) -> Result<String, String> {
    match take(kv, key) {
        None => Err(format!("missing field `{}`", key)),
        Some(V::S(s)) if s.is_empty() => Err(format!("`{}`: must not be empty", key)),
        Some(V::S(s)) => Ok(s.clone()),
        Some(_) => Err(format!("`{}`: must be a string", key)),
    }
}

fn enum_of(kv: &[(String, V)], key: &str, list: &[&str]) -> Result<String, String> {
    let s = str_of(kv, key)?;
    if list.contains(&s.as_str()) {
        Ok(s)
    } else {
        Err(format!("`{}`: `{}` is not one of {}", key, s, list.join(", ")))
    }
}

fn no_unknown(kv: &[(String, V)], allowed: &[&str], what: &str) -> Result<(), String> {
    match kv.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
        Some((k, _)) => Err(format!("unknown key `{}`{}", k, what)),
        None => Ok(()),
    }
}

fn parse_hook(v: &V) -> Result<Hook, String> {
    let kv = match v {
        V::O(kv) => kv,
        _ => return Err("`hook`: must be an object".to_string()),
    };
    let inner = |e: String| format!("`hook`: {}", e);
    no_unknown(kv, &["script", "manifest", "event", "tools"], " in `hook`")?;
    let script = str_of(kv, "script").map_err(inner)?;
    let ok_script = script
        .strip_prefix("plugins/claude/")
        .map(|n| n.ends_with("-hook.sh") && !n.contains('/') && path_ok(&script))
        .unwrap_or(false);
    if !ok_script {
        return Err("`hook`: `script` must be plugins/claude/<name>-hook.sh".to_string());
    }
    let manifest = str_of(kv, "manifest").map_err(inner)?;
    if !path_ok(&manifest) {
        return Err("`hook`: `manifest` must be a plain repo-relative path".to_string());
    }
    let event = enum_of(kv, "event", &EVENTS).map_err(inner)?;
    let tools = tools_of(take(kv, "tools")).map_err(|e| format!("`hook`: `tools`: {}", e))?;
    Ok(Hook { script, manifest, event, tools })
}

fn tools_of(v: Option<&V>) -> Result<Vec<String>, String> {
    let a = match v {
        None => return Err("missing".to_string()),
        Some(V::A(a)) => a,
        Some(_) => return Err("must be an array".to_string()),
    };
    let mut out: Vec<String> = Vec::new();
    for x in a {
        match x {
            V::S(s) if !s.is_empty() => {
                if out.contains(s) {
                    return Err(format!("duplicate entry `{}`", s));
                }
                out.push(s.clone());
            }
            _ => return Err("entries must be non-empty strings".to_string()),
        }
    }
    if out.is_empty() {
        return Err("must not be empty".to_string());
    }
    Ok(out)
}

fn parse_row(v: &V) -> Result<Control, String> {
    let kv = match v {
        V::O(kv) => kv,
        _ => return Err("not an object".to_string()),
    };
    no_unknown(kv, &["id", "kind", "class", "baseline_ref", "enabled", "failure", "test", "retirement_test", "owner", "hook"], "")?;
    let id = str_of(kv, "id")?;
    if !id_ok(&id) {
        return Err("`id`: malformed (lowercase letters, digits, '-', '_', '.')".to_string());
    }
    let kind = enum_of(kv, "kind", &KINDS)?;
    let class = enum_of(kv, "class", &CLASSES)?;
    let baseline_ref = match take(kv, "baseline_ref") {
        None => None,
        Some(_) => Some(str_of(kv, "baseline_ref")?),
    };
    if class == "orchestration" && baseline_ref.is_none() {
        return Err("missing field `baseline_ref` (required for class orchestration)".to_string());
    }
    if class != "orchestration" && baseline_ref.is_some() {
        return Err("`baseline_ref`: only allowed for class orchestration".to_string());
    }
    let enabled = match take(kv, "enabled") {
        None => return Err("missing field `enabled`".to_string()),
        Some(V::B(b)) => *b,
        Some(_) => return Err("`enabled`: must be a boolean".to_string()),
    };
    let failure = str_of(kv, "failure")?;
    let test = str_of(kv, "test")?;
    if !path_ok(&test) {
        return Err("`test`: must be a plain repo-relative path".to_string());
    }
    let retirement_test = str_of(kv, "retirement_test")?;
    let owner = enum_of(kv, "owner", &OWNERS)?;
    let hook = match (kind.as_str(), take(kv, "hook")) {
        ("hook", None) => return Err("missing field `hook` (required for kind hook)".to_string()),
        ("hook", Some(h)) => Some(parse_hook(h)?),
        (_, Some(_)) => return Err("`hook`: only allowed for kind hook".to_string()),
        (_, None) => None,
    };
    Ok(Control { id, kind, class, baseline_ref, enabled, failure, test, retirement_test, owner, hook })
}

impl Registry {
    /// Strict parse of the registry file. `version` must be 1; rows are refused for an unknown or duplicate key, a wrong type, an empty
    /// string, an out-of-list enum, a duplicate id and the class/hook pairing rules. The Err names the row and the field.
    pub fn parse(text: &str) -> Result<Registry, String> {
        let doc = parse_doc(text)?;
        let kv = match &doc {
            V::O(kv) => kv,
            _ => return Err("registry: not a JSON object".to_string()),
        };
        no_unknown(kv, &["version", "controls"], " in the registry")?;
        match take(kv, "version") {
            None => return Err("registry: missing field `version`".to_string()),
            Some(V::N(1)) => {}
            Some(_) => return Err("registry: `version`: must be 1".to_string()),
        }
        let rows = match take(kv, "controls") {
            None => return Err("registry: missing field `controls`".to_string()),
            Some(V::A(a)) => a,
            Some(_) => return Err("registry: `controls`: must be an array".to_string()),
        };
        let mut out: Vec<Control> = Vec::new();
        for (n, r) in rows.iter().enumerate() {
            let name = match r {
                V::O(kv) => match take(kv, "id") {
                    Some(V::S(s)) if !s.is_empty() => s.clone(),
                    _ => format!("#{}", n),
                },
                _ => format!("#{}", n),
            };
            let c = parse_row(r).map_err(|e| format!("row {}: {}", name, e))?;
            if out.iter().any(|x| x.id == c.id) {
                return Err(format!("row {}: `id`: duplicate id", c.id));
            }
            out.push(c);
        }
        Ok(Registry { rows: out })
    }

    /// Read `<repo_root>/controls/registry.json`: a regular file, at most 1 MiB, UTF-8, then `parse`.
    pub fn load(repo_root: &Path) -> Result<Registry, String> {
        let path = repo_root.join("controls").join("registry.json");
        let meta = std::fs::metadata(&path).map_err(|_| "controls/registry.json: missing or unreadable".to_string())?;
        if !meta.is_file() {
            return Err("controls/registry.json: not a regular file".to_string());
        }
        let f = std::fs::File::open(&path).map_err(|_| "controls/registry.json: unreadable".to_string())?;
        let mut bytes = Vec::new();
        f.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "controls/registry.json: unreadable".to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("controls/registry.json: too large".to_string());
        }
        let text = String::from_utf8(bytes).map_err(|_| "controls/registry.json: not utf-8".to_string())?;
        Registry::parse(&text).map_err(|e| format!("controls/registry.json: {}", e))
    }

    pub fn ids(&self) -> Vec<&str> {
        self.rows.iter().map(|r| r.id.as_str()).collect()
    }
}

fn read_small(path: &Path, what: &str) -> Result<String, String> {
    let f = std::fs::File::open(path).map_err(|_| format!("{}: unreadable", what))?;
    let mut bytes = Vec::new();
    f.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| format!("{}: unreadable", what))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{}: too large", what));
    }
    String::from_utf8(bytes).map_err(|_| format!("{}: not utf-8", what))
}

fn manifest_event_tools(root: &Path, rel: &str) -> Result<(String, Vec<String>), String> {
    let text = read_small(&root.join(rel), rel)?;
    let doc = parse_manifest(&text).map_err(|e| format!("{}: {}", rel, e))?;
    Ok(doc)
}

/// A manifest holds more keys than the registry cares about; only `event` and `tools` are read.
fn parse_manifest(text: &str) -> Result<(String, Vec<String>), String> {
    let doc = parse_doc_loose(text)?;
    let event = match take(&doc, "event") {
        Some(V::S(s)) => s.clone(),
        _ => return Err("`event`: missing or not a string".to_string()),
    };
    let tools = tools_of(take(&doc, "tools")).map_err(|e| format!("`tools`: {}", e))?;
    Ok((event, tools))
}

/// Manifests carry `exit_allow: 0` style numbers and arrays of strings, all inside the value grammar above.
fn parse_doc_loose(text: &str) -> Result<Vec<(String, V)>, String> {
    match parse_doc(text)? {
        V::O(kv) => Ok(kv),
        _ => Err("not a JSON object".to_string()),
    }
}

fn is_file(root: &Path, rel: &str) -> bool {
    std::fs::symlink_metadata(root.join(rel)).map(|m| m.is_file()).unwrap_or(false)
}

/// The registry check. Every `*-hook.sh` regular file directly under `plugins/claude/` needs exactly one hook row naming it; every hook
/// row's script, manifest and test must exist; and its event and tools must equal its manifest's. The first failure is returned and
/// names the file or the row. Files are read as data only; nothing under `plugins/` is a crate dependency.
pub fn check_hooks(repo_root: &Path, reg: &Registry) -> Result<(), String> {
    let mut names: Vec<String> = Vec::new();
    for e in std::fs::read_dir(repo_root.join(HOOK_DIR)).map_err(|_| format!("{}: missing or unreadable", HOOK_DIR))? {
        let e = e.map_err(|_| format!("{}: unreadable", HOOK_DIR))?;
        let name = e.file_name().into_string().map_err(|_| format!("{}: an entry name is not utf-8", HOOK_DIR))?;
        if name.ends_with("-hook.sh") && e.path().symlink_metadata().map(|m| m.is_file()).unwrap_or(false) {
            names.push(name);
        }
    }
    names.sort();
    for name in &names {
        let rel = format!("{}/{}", HOOK_DIR, name);
        let n = reg.rows.iter().filter(|r| r.hook.as_ref().map(|h| h.script == rel).unwrap_or(false)).count();
        match n {
            1 => {}
            0 => return Err(format!("{}: hook has no registry row", name)),
            _ => return Err(format!("{}: hook has more than one registry row", name)),
        }
    }
    for r in &reg.rows {
        let h = match &r.hook {
            Some(h) => h,
            None => continue,
        };
        for (what, rel) in [("script", &h.script), ("manifest", &h.manifest), ("test", &r.test)] {
            if !is_file(repo_root, rel) {
                return Err(format!("row {}: `{}` {} does not exist", r.id, what, rel));
            }
        }
        let (event, tools) = manifest_event_tools(repo_root, &h.manifest).map_err(|e| format!("row {}: {}", r.id, e))?;
        if event != h.event {
            return Err(format!("row {}: `event` `{}` differs from {} (`{}`)", r.id, h.event, h.manifest, event));
        }
        if tools != h.tools {
            return Err(format!("row {}: `tools` differ from {}", r.id, h.manifest));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("hg-ctl-{}-{}-{}", name, std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    /// A temp copy of the two inputs the check reads: `controls/` and the flat `plugins/claude/` folder.
    fn copy_repo(name: &str) -> Scratch {
        let s = Scratch::new(name);
        for d in ["controls", "plugins/claude"] {
            std::fs::create_dir_all(s.0.join(d)).unwrap();
            for e in std::fs::read_dir(repo().join(d)).unwrap() {
                let e = e.unwrap();
                if e.path().is_file() {
                    std::fs::copy(e.path(), s.0.join(d).join(e.file_name())).unwrap();
                }
            }
        }
        s
    }

    const HEAD: &str = r#"{"id":"x-hook","kind":"hook","class":"policy","enabled":true,"failure":"f","test":"plugins/claude/t.sh","retirement_test":"r","owner":"execution_verification""#;
    const HOOK: &str = r#","hook":{"script":"plugins/claude/x-hook.sh","manifest":"plugins/claude/x-manifest.json","event":"PreToolUse","tools":["Bash"]}"#;

    fn row() -> String {
        format!("{}{}}}", HEAD, HOOK)
    }

    fn doc(rows: &[String]) -> String {
        format!(r#"{{"version":1,"controls":[{}]}}"#, rows.join(","))
    }

    fn mutate(from: &str, to: &str) -> String {
        let r = row();
        assert!(r.contains(from), "row holds {}", from);
        doc(&[r.replacen(from, to, 1)])
    }

    #[test]
    fn a_bad_row_is_refused_naming_row_and_field() {
        assert!(Registry::parse(&doc(&[row()])).is_ok(), "the unmutated row is Ok");
        let cases: Vec<(String, &str)> = vec![
            (mutate(r#""failure":"f","#, ""), "failure"),
            (mutate(r#""test":"plugins/claude/t.sh","#, ""), "`test`"),
            (mutate(r#""retirement_test":"r","#, ""), "retirement_test"),
            (mutate(r#""owner":"execution_verification","#, ""), "owner"),
            (mutate(r#""id":"x-hook","#, r#""id":"x-hook","zz":1,"#), "zz"),
            (mutate(r#""enabled":true,"#, r#""enabled":true,"enabled":true,"#), "enabled"),
            (mutate(r#""enabled":true"#, r#""enabled":"yes""#), "enabled"),
            (mutate(r#""failure":"f""#, r#""failure":7"#), "failure"),
            (mutate(r#""class":"policy""#, r#""class":"nope""#), "class"),
            (mutate(r#""owner":"execution_verification""#, r#""owner":"nobody""#), "owner"),
            (mutate(r#""kind":"hook""#, r#""kind":"gate""#), "kind"),
            (mutate(r#""event":"PreToolUse""#, r#""event":"Sometimes""#), "event"),
            (mutate(r#""class":"policy""#, r#""class":"orchestration""#), "baseline_ref"),
            (mutate(r#""id":"x-hook","#, r#""id":"x-hook","baseline_ref":"b","#), "baseline_ref"),
            (mutate(HOOK, ""), "hook"),
            (mutate(r#""tools":["Bash"]"#, r#""tools":[]"#), "tools"),
            (mutate(r#""tools":["Bash"]"#, r#""tools":["Bash","Bash"]"#), "tools"),
            (mutate(r#""failure":"f""#, r#""failure":"""#), "failure"),
            (mutate(r#""script":"plugins/claude/x-hook.sh""#, r#""script":"plugins/claude/x.sh""#), "script"),
            (mutate(r#""id":"x-hook""#, r#""id":"X Hook""#), "id"),
            (doc(&[row(), row()]), "duplicate id"),
            (doc(&[row()]).replacen("\"version\":1", "\"version\":2", 1), "version"),
            (doc(&[row()]).replacen("\"version\":1,", "", 1), "version"),
            (doc(&[row()]).replacen("\"version\":1", "\"version\":1,\"extra\":true", 1), "extra"),
            ("{".to_string(), ""),
        ];
        for (text, field) in cases {
            let e = Registry::parse(&text).expect_err(&format!("a bad row must be refused: {}", text));
            assert!(e.contains(field), "`{}` must name `{}`: {}", e, field, text);
            if e.starts_with("row ") && field != "id" {
                assert!(e.contains("x-hook"), "`{}` must name the row", e);
            }
        }
        let deep = format!("{}1{}", "[".repeat(50), "]".repeat(50));
        assert!(Registry::parse(&deep).is_err(), "deep nesting is refused, not recursed");
        let ok = Registry::parse(&mutate(r#""class":"policy""#, r#""class":"crutch""#)).unwrap();
        assert_eq!(ok.rows[0].class, "crutch");
    }

    /// The strings of the first `"required": [...]` array at or after `from` in a JSON text.
    fn required_after(schema: &str, from: usize) -> Vec<String> {
        let at = from + schema[from..].find("\"required\"").expect("schema has a required array");
        let open = at + schema[at..].find('[').unwrap();
        let close = open + schema[open..].find(']').unwrap();
        schema[open + 1..close].split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
    }

    /// The strings of the first `"enum": [...]` array after the first `"<key>":` at or after `from`.
    fn enum_after(schema: &str, from: usize, key: &str) -> Vec<String> {
        let k = from + schema[from..].find(&format!("\"{}\":", key)).unwrap_or_else(|| panic!("schema names {}", key));
        let at = k + schema[k..].find("\"enum\"").expect("key has an enum");
        let open = at + schema[at..].find('[').unwrap();
        let close = open + schema[open..].find(']').unwrap();
        schema[open + 1..close].split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
    }

    fn sorted(v: &[&str]) -> Vec<String> {
        let mut w: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        w.sort();
        w
    }

    #[test]
    fn the_committed_schema_matches_the_loader() {
        let schema = std::fs::read_to_string(repo().join("schemas/control.schema.json")).expect("the schema is committed and utf-8");
        let row = schema.find("\"row\": {").expect("schema defines the row");
        let mut got = required_after(&schema, row);
        got.sort();
        assert_eq!(got, sorted(&FIELDS), "the schema's row required set equals FIELDS");
        for (key, list) in [("kind", &KINDS[..]), ("class", &CLASSES[..]), ("owner", &OWNERS[..])] {
            let mut got = enum_after(&schema, row, key);
            got.sort();
            assert_eq!(got, sorted(list), "the schema's {} enum equals the loader's", key);
        }
        let hook = row + schema[row..].find("\"hook\": {").expect("schema defines the hook object");
        let mut got = enum_after(&schema, hook, "event");
        got.sort();
        assert_eq!(got, sorted(&EVENTS), "the schema's event enum equals the loader's");
        let mut got = required_after(&schema, hook);
        got.sort();
        assert_eq!(got, sorted(&["script", "manifest", "event", "tools"]));
    }

    #[test]
    fn the_committed_registry_has_one_row_per_plugin_hook() {
        let repo = repo();
        let reg = Registry::load(&repo).expect("the committed registry loads");
        assert_eq!(reg.rows.len(), 3, "one row per hook: {:?}", reg.ids());
        if let Err(e) = check_hooks(&repo, &reg) {
            panic!("registry check failed: {}", e);
        }
    }

    #[test]
    fn a_hook_with_no_registry_row_fails_the_check() {
        let s = copy_repo("norow");
        let reg = Registry::load(&s.0).unwrap();
        assert!(check_hooks(&s.0, &reg).is_ok());
        std::fs::write(s.0.join("plugins/claude/zz-hook.sh"), "#!/bin/sh\n").unwrap();
        let e = check_hooks(&s.0, &reg).unwrap_err();
        assert!(e.contains("zz-hook.sh"), "{}", e);
        std::fs::remove_file(s.0.join("plugins/claude/zz-hook.sh")).unwrap();
        let mut dropped = Registry::load(&s.0).unwrap();
        dropped.rows.retain(|r| r.id != "gh-text-hook");
        let e = check_hooks(&s.0, &dropped).unwrap_err();
        assert!(e.contains("gh-text-hook.sh"), "{}", e);
        // a non-hook script is not a hook
        std::fs::write(s.0.join("plugins/claude/zz-smoke.sh"), "#!/bin/sh\n").unwrap();
        assert!(check_hooks(&s.0, &reg).is_ok());
    }

    #[test]
    fn a_dangling_or_drifted_row_fails_the_check() {
        let s = copy_repo("drift");
        std::fs::remove_file(s.0.join("plugins/claude/pre-completion-hook.sh")).unwrap();
        let reg = Registry::load(&s.0).unwrap();
        let e = check_hooks(&s.0, &reg).unwrap_err();
        assert!(e.contains("pre-completion-hook.sh"), "{}", e);
        let s = copy_repo("drift2");
        let reg_path = s.0.join("controls/registry.json");
        let text = std::fs::read_to_string(&reg_path).unwrap();
        let drifted = text.replacen(r#""tools": ["Bash"]"#, r#""tools": ["Bash", "Read"]"#, 1);
        assert_ne!(text, drifted);
        std::fs::write(&reg_path, drifted).unwrap();
        let reg = Registry::load(&s.0).unwrap();
        let e = check_hooks(&s.0, &reg).unwrap_err();
        assert!(e.contains("tools") && e.contains("gh-text-hook"), "{}", e);
        let s = copy_repo("drift3");
        std::fs::remove_file(s.0.join("plugins/claude/text-rule-smoke.sh")).unwrap();
        let reg = Registry::load(&s.0).unwrap();
        let e = check_hooks(&s.0, &reg).unwrap_err();
        assert!(e.contains("text-rule-smoke.sh"), "{}", e);
    }
}
