//! The `gates/` contract (std only). One gate is one JSON file `gates/<id>.json` with exactly six keys; the loader turns the folder into
//! data and runs nothing. Running is the merged runner (`to_check`) or the merged both-ends judge (`with_spec`).
//! Loading fails closed: anything the contract does not name is a refusal that names the file and the field.

use crate::runner::Check;
#[cfg(unix)]
use crate::tree_prep::Spec;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// The six required keys. `schemas/gate.schema.json` mirrors this set; a test pins the two together.
pub const FIELDS: [&str; 6] = ["id", "program", "args", "timeout_ms", "max_output_bytes", "both_ends"];
/// Names in `gates/` that belong to another ticket's data and are never gates.
const RESERVED: [&str; 1] = ["path-map.json"];
const MAX_GATE_BYTES: u64 = 1024 * 1024;

fn id_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

/// A plain repo-relative path: `/`-separated, every component a plain name, no `.git` in any case.
/// The empty string splits into one empty component, so it is refused with the other empty components.
fn path_ok(p: &str) -> bool {
    !p.chars().any(|c| c == '\\' || c.is_control())
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.eq_ignore_ascii_case(".git"))
}

/// One parsed gate definition. Pure data: `parse` validates every value once, and only validated values go onward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
    pub both_ends: bool,
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

    /// A string. Escapes are the plain set (`\" \\ \/ \n \r \t \uXXXX`); a raw control byte and a lone surrogate are refused.
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

    /// A plain non-negative integer: ascii digits only, no sign, no leading zero.
    fn number(&mut self) -> Result<u64, String> {
        self.ws();
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

    fn boolean(&mut self) -> Result<bool, String> {
        self.ws();
        let rest = &self.b[self.i..];
        if rest.starts_with(b"true") {
            self.i += 4;
            Ok(true)
        } else if rest.starts_with(b"false") {
            self.i += 5;
            Ok(false)
        } else {
            Err("not a JSON boolean".to_string())
        }
    }

    fn string_array(&mut self) -> Result<Vec<String>, String> {
        self.eat(b'[')?;
        let mut v = Vec::new();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(v);
        }
        loop {
            v.push(self.string()?);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(v);
                }
                _ => return Err("expected ',' or ']'".to_string()),
            }
        }
    }
}

fn field<T>(name: &str, r: Result<T, String>) -> Result<T, String> {
    r.map_err(|e| format!("`{}`: {}", name, e))
}

fn once<T>(slot: &mut Option<T>, name: &str, v: T) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate key `{}`", name));
    }
    *slot = Some(v);
    Ok(())
}

impl Gate {
    /// Strict parse of the one gate shape: six keys, each exactly once, no unknown key, no optional key, no default. The Err names the
    /// field. `file_stem` is the file name the text came from; `id` must equal it.
    pub fn parse(text: &str, file_stem: &str) -> Result<Gate, String> {
        let mut p = P { b: text.as_bytes(), i: 0 };
        p.eat(b'{').map_err(|_| "not a JSON object".to_string())?;
        let (mut id, mut program, mut args) = (None, None, None);
        let (mut timeout_ms, mut max_output_bytes, mut both_ends) = (None, None, None);
        if p.peek() == Some(b'}') {
            p.i += 1;
        } else {
            loop {
                let key = p.string().map_err(|e| format!("key: {}", e))?;
                p.eat(b':').map_err(|e| format!("`{}`: {}", key, e))?;
                match key.as_str() {
                    "id" => once(&mut id, "id", field("id", p.string())?)?,
                    "program" => once(&mut program, "program", field("program", p.string())?)?,
                    "args" => once(&mut args, "args", field("args", p.string_array())?)?,
                    "timeout_ms" => once(&mut timeout_ms, "timeout_ms", field("timeout_ms", p.number())?)?,
                    "max_output_bytes" => once(&mut max_output_bytes, "max_output_bytes", field("max_output_bytes", p.number())?)?,
                    "both_ends" => once(&mut both_ends, "both_ends", field("both_ends", p.boolean())?)?,
                    _ => return Err(format!("unknown key `{}`", key)),
                }
                match p.peek() {
                    Some(b',') => p.i += 1,
                    Some(b'}') => {
                        p.i += 1;
                        break;
                    }
                    _ => return Err(format!("`{}`: expected ',' or '}}'", key)),
                }
            }
        }
        p.ws();
        if p.i != p.b.len() {
            return Err("trailing bytes".to_string());
        }
        let id = id.ok_or("missing field `id`")?;
        let program = program.ok_or("missing field `program`")?;
        let args = args.ok_or("missing field `args`")?;
        let timeout_ms = timeout_ms.ok_or("missing field `timeout_ms`")?;
        let max_output_bytes = max_output_bytes.ok_or("missing field `max_output_bytes`")?;
        let both_ends = both_ends.ok_or("missing field `both_ends`")?;
        if !id_ok(&id) {
            return Err("`id`: malformed (lowercase letters, digits, '-', '_', '.')".to_string());
        }
        if id != file_stem {
            return Err(format!("`id`: must equal the file name stem `{}`", file_stem));
        }
        if !path_ok(&program) {
            return Err("`program`: must be a plain repo-relative path".to_string());
        }
        if args.iter().any(|a| a.contains('\0')) {
            return Err("`args`: an argument holds a NUL byte".to_string());
        }
        if timeout_ms == 0 {
            return Err("`timeout_ms`: must be above zero".to_string());
        }
        if max_output_bytes == 0 {
            return Err("`max_output_bytes`: must be above zero".to_string());
        }
        Ok(Gate { id, program, args, timeout_ms, max_output_bytes, both_ends })
    }
}

/// The one shape of a per-file refusal: it always names the file.
fn refuse(name: &str, why: &str) -> String {
    format!("gates/{}: {}", name, why)
}

/// Every gate under `<repo_root>/gates/`, sorted by id.
#[derive(Debug)]
pub struct Gates {
    gates: Vec<Gate>,
}

impl Gates {
    /// The one load root: `<repo_root>/gates/`, repo root canonicalized once. Entries are visited in file-name order, so the first
    /// refusal is the same on every filesystem. Dot-prefixed names and the reserved `path-map.json` are skipped; every other entry must
    /// be a regular file (a subdirectory or any other shape is refused); non-`.json` files are ignored; each `.json` file must
    /// canonicalize back under the repo root, be UTF-8, be at most 1 MiB, and parse as a gate. No gate file at all is refused.
    pub fn load(repo_root: &Path) -> Result<Gates, String> {
        let root = repo_root.canonicalize().map_err(|_| "gates folder: repo root unreadable".to_string())?;
        let dir = root.join("gates");
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&dir).map_err(|_| "gates folder missing or unreadable".to_string())? {
            let e = e.map_err(|_| "gates folder unreadable".to_string())?;
            names.push(e.file_name().into_string().map_err(|_| "gates folder: an entry name is not utf-8".to_string())?);
        }
        names.sort();
        let mut gates: Vec<Gate> = Vec::new();
        for name in names {
            if name.starts_with('.') || RESERVED.contains(&name.as_str()) {
                continue;
            }
            let path = dir.join(&name);
            let meta = std::fs::metadata(&path).map_err(|_| refuse(&name, "unreadable"))?;
            if !meta.is_file() {
                return Err(refuse(&name, "not a regular file"));
            }
            let stem = match name.strip_suffix(".json") {
                Some(s) => s,
                None => continue,
            };
            let real = path.canonicalize().map_err(|_| refuse(&name, "unreadable"))?;
            if !real.starts_with(&root) {
                return Err(refuse(&name, "escapes the repo"));
            }
            let f = std::fs::File::open(&real).map_err(|_| refuse(&name, "unreadable"))?;
            let mut bytes = Vec::new();
            f.take(MAX_GATE_BYTES + 1).read_to_end(&mut bytes).map_err(|_| refuse(&name, "unreadable"))?;
            if bytes.len() as u64 > MAX_GATE_BYTES {
                return Err(refuse(&name, "too large"));
            }
            let text = String::from_utf8(bytes).map_err(|_| refuse(&name, "not utf-8"))?;
            gates.push(Gate::parse(&text, stem).map_err(|e| refuse(&name, &e))?);
        }
        if gates.is_empty() {
            return Err("gates folder holds no gate files".to_string());
        }
        gates.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Gates { gates })
    }

    pub fn ids(&self) -> Vec<&str> {
        self.gates.iter().map(|g| g.id.as_str()).collect()
    }

    pub fn get(&self, id: &str) -> Option<&Gate> {
        self.gates.iter().find(|g| g.id == id)
    }
}

/// Every map check id must be a declared gate id; the first unknown one is named. An empty slice is Ok (that policy is the caller's).
pub fn resolve_map_ids(gates: &Gates, map_ids: &[String]) -> Result<(), String> {
    match map_ids.iter().find(|id| gates.get(id).is_none()) {
        Some(id) => Err(format!("path map names `{}`, which is not a declared gate", id)),
        None => Ok(()),
    }
}

fn limit(gate: &Gate) -> Result<(Duration, usize), String> {
    let max = usize::try_from(gate.max_output_bytes).map_err(|_| "`max_output_bytes`: does not fit this platform".to_string())?;
    Ok((Duration::from_millis(gate.timeout_ms), max))
}

/// Compose with the merged runner, unchanged: `Check::load` re-resolves `program` with realpath under the same repo root and its
/// refusal passes through verbatim.
pub fn to_check(repo_root: &Path, gate: &Gate) -> Result<Check, String> {
    let (budget, max) = limit(gate)?;
    let args: Vec<&str> = gate.args.iter().map(|s| s.as_str()).collect();
    Check::load(repo_root, &gate.id, &gate.program, &args, budget, max)
}

/// Compose with the merged both-ends judge: hands `f` the tree-preparation `Spec` built from the gate (the Spec borrows its argument
/// slice, so it only lives inside the call). Base, head and recorded head stay the caller's inputs.
#[cfg(unix)]
pub fn with_spec<R>(gate: &Gate, f: impl FnOnce(&Spec) -> R) -> Result<R, String> {
    let (budget, max_output) = limit(gate)?;
    let args: Vec<&str> = gate.args.iter().map(|s| s.as_str()).collect();
    Ok(f(&Spec { id: &gate.id, program: &gate.program, args: &args, budget, max_output }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate_result::Status;
    use crate::runner;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    static N: AtomicU64 = AtomicU64::new(0);
    /// Tests that write a script and then run it hold this, so no sibling test forks while a script's write handle is open.
    static EXEC: Mutex<()> = Mutex::new(());
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
    const VALID: &str = r#"{"id":"g1","program":"ci/c.sh","args":[],"timeout_ms":1000,"max_output_bytes":4096,"both_ends":false}"#;

    /// A scratch directory removed on drop, even when an assertion fails.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("hg-{}-{}-{}", name, std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
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

    fn put(dir: &Path, rel: &str, body: &[u8]) {
        let f = dir.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, body).unwrap();
    }

    #[cfg(unix)]
    fn put_exec(dir: &Path, rel: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        put(dir, rel, body.as_bytes());
        std::fs::set_permissions(dir.join(rel), std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Tester")
            .env("GIT_AUTHOR_EMAIL", ["1+t", "@", "users.", "noreply.example"].concat())
            .env("GIT_COMMITTER_NAME", "Tester")
            .env("GIT_COMMITTER_EMAIL", ["1+t", "@", "users.", "noreply.example"].concat())
            .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git failed: {:?} {}", args, String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    /// A scratch root holding exactly one gate file `gates/<name>` with these bytes.
    fn root_with(name: &str, bytes: &[u8]) -> Scratch {
        let s = Scratch::new("one");
        put(&s.0, &format!("gates/{}", name), bytes);
        s
    }

    fn load_one(name: &str, bytes: &[u8]) -> Result<Gates, String> {
        let s = root_with(name, bytes);
        Gates::load(&s.0)
    }

    fn sw(text: &str, from: &str, to: &str) -> String {
        assert!(text.contains(from), "template holds {}", from);
        text.replacen(from, to, 1)
    }

    #[cfg(unix)]
    #[test]
    fn a_valid_gate_file_loads_and_runs_through_the_runner() {
        let _g = EXEC.lock().unwrap_or_else(|e| e.into_inner());
        for (code, want) in [(0, Status::Pass), (3, Status::Fail)] {
            let s = Scratch::new("run");
            put(&s.0, "gates/check-ok.json", sw(VALID, r#""id":"g1""#, r#""id":"check-ok""#).replace("ci/c.sh", "check.sh").replace("1000", "20000").as_bytes());
            put_exec(&s.0, "check.sh", &format!("#!/bin/sh\nexit {}\n", code));
            let gates = Gates::load(&s.0).expect("a valid gate loads");
            assert_eq!(gates.ids(), vec!["check-ok"]);
            let gate = gates.get("check-ok").unwrap();
            let check = to_check(&s.0, gate).expect("the gate becomes a runnable check");
            let r = runner::run(&s.0, &check, SHA);
            assert_eq!(r.status, want, "exit {}: {:?} {}", code, r.failure_class, r.message);
            assert_eq!(r.gate_id, "check-ok");
            assert_eq!(r.head_sha, SHA);
            assert!(r.validate().is_ok());
            if code != 0 {
                assert_eq!(r.failure_class.as_deref(), Some("check-failed"));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_gates_budget_and_cap_reach_the_runner() {
        let _g = EXEC.lock().unwrap_or_else(|e| e.into_inner());
        // output past the cap is cut and marked; a check past its budget is a FAIL
        let s = Scratch::new("budget");
        put(&s.0, "gates/capped.json", r#"{"id":"capped","program":"cap.sh","args":[],"timeout_ms":20000,"max_output_bytes":8,"both_ends":false}"#.as_bytes());
        put(&s.0, "gates/slow.json", r#"{"id":"slow","program":"slow.sh","args":["5"],"timeout_ms":200,"max_output_bytes":4096,"both_ends":false}"#.as_bytes());
        put_exec(&s.0, "cap.sh", "#!/bin/sh\nprintf 0123456789abcdef\nexit 3\n");
        // the executable bit is not checked at load: a program that cannot be started loads, then fails when it runs
        put(&s.0, "gates/plain.json", r#"{"id":"plain","program":"plain.sh","args":[],"timeout_ms":20000,"max_output_bytes":4096,"both_ends":false}"#.as_bytes());
        put(&s.0, "plain.sh", b"#!/bin/sh\nexit 0\n");
        put_exec(&s.0, "slow.sh", "#!/bin/sh\nsleep \"$1\"\n");
        let gates = Gates::load(&s.0).unwrap();
        let r = runner::run(&s.0, &to_check(&s.0, gates.get("capped").unwrap()).unwrap(), SHA);
        assert_eq!(r.failure_class.as_deref(), Some("check-failed"), "{}", r.message);
        assert!(r.message.contains("[output cut at 8 bytes]"), "{}", r.message);
        let r = runner::run(&s.0, &to_check(&s.0, gates.get("plain").unwrap()).unwrap(), SHA);
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.failure_class.as_deref(), Some("spawn-error"), "{}", r.message);
        let r = runner::run(&s.0, &to_check(&s.0, gates.get("slow").unwrap()).unwrap(), SHA);
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.failure_class.as_deref(), Some("timeout"), "{}", r.message);
    }

    #[test]
    fn a_gate_file_missing_a_required_field_is_refused_naming_file_and_field() {
        for f in FIELDS {
            let kept: Vec<&str> = [
                r#""id":"g1""#,
                r#""program":"ci/c.sh""#,
                r#""args":[]"#,
                r#""timeout_ms":1000"#,
                r#""max_output_bytes":4096"#,
                r#""both_ends":false"#,
            ]
            .iter()
            .copied()
            .filter(|kv| !kv.starts_with(&format!("\"{}\"", f)))
            .collect();
            let text = format!("{{{}}}", kept.join(","));
            let e = load_one("g1.json", text.as_bytes()).unwrap_err();
            assert!(e.contains("g1.json"), "arm {}: the file is named: {}", f, e);
            assert!(e.contains(&format!("`{}`", f)), "arm {}: the field is named: {}", f, e);
            assert!(e.contains("missing"), "arm {}: {}", f, e);
        }
        let e = load_one("g1.json", b"{}").unwrap_err();
        assert!(e.contains("missing") && e.contains("g1.json"), "an empty object: {}", e);
    }

    #[test]
    fn malformed_gate_values_are_refused() {
        let arms: Vec<(&str, String, &str)> = vec![
            ("id bad", sw(VALID, r#""id":"g1""#, r#""id":"Bad Id!""#), "malformed"),
            ("id empty", sw(VALID, r#""id":"g1""#, r#""id":"""#), "malformed"),
            ("id differs from the stem", sw(VALID, r#""id":"g1""#, r#""id":"other""#), "must equal"),
            ("program absolute", sw(VALID, "ci/c.sh", "/bin/true"), "plain repo-relative"),
            ("program dot-dot", sw(VALID, "ci/c.sh", "../x"), "plain repo-relative"),
            ("program dot", sw(VALID, "ci/c.sh", "."), "plain repo-relative"),
            ("program dot inside", sw(VALID, "ci/c.sh", "a/./b"), "plain repo-relative"),
            ("program dot-dot inside", sw(VALID, "ci/c.sh", "a/../b"), "plain repo-relative"),
            ("program empty component", sw(VALID, "ci/c.sh", "a//b"), "plain repo-relative"),
            ("program trailing slash", sw(VALID, "ci/c.sh", "x/"), "plain repo-relative"),
            ("program backslash", sw(VALID, "ci/c.sh", r"a\\b"), "plain repo-relative"),
            ("program .git", sw(VALID, "ci/c.sh", ".git/x"), "plain repo-relative"),
            ("program .GIT", sw(VALID, "ci/c.sh", ".GIT/x"), "plain repo-relative"),
            ("program empty", sw(VALID, "ci/c.sh", ""), "plain repo-relative"),
            ("program NUL escape", sw(VALID, "ci/c.sh", r"a\u0000b"), "plain repo-relative"),
            ("program raw newline", sw(VALID, "ci/c.sh", "a\nb"), "control byte"),
            ("timeout zero", sw(VALID, "1000", "0"), "above zero"),
            ("timeout negative", sw(VALID, "1000", "-1"), "not a plain"),
            ("timeout string", sw(VALID, "1000", r#""500""#), "not a plain"),
            ("timeout leading zero", sw(VALID, "1000", "0100"), "leading zero"),
            ("timeout fraction", sw(VALID, "1000", "1.5"), "expected ',' or"),
            ("timeout too large", sw(VALID, "1000", "99999999999999999999999"), "out of range"),
            ("max output zero", sw(VALID, "4096", "0"), "above zero"),
            ("both_ends string", sw(VALID, "false", r#""true""#), "not a JSON boolean"),
            ("both_ends number", sw(VALID, "false", "1"), "not a JSON boolean"),
            ("both_ends null", sw(VALID, "false", "null"), "not a JSON boolean"),
            ("args number", sw(VALID, r#""args":[]"#, r#""args":[1]"#), "expected '\"'"),
            ("args raw NUL", sw(VALID, r#""args":[]"#, "\"args\":[\"a\u{0}b\"]"), "control byte"),
            ("args NUL escape", sw(VALID, r#""args":[]"#, r#""args":["a\u0000b"]"#), "NUL byte"),
            ("args not an array", sw(VALID, r#""args":[]"#, r#""args":"x""#), "expected '['"),
            ("unknown key", sw(VALID, r#""both_ends":false"#, r#""both_ends":false,"extra":1"#), "unknown key"),
            ("missing colon after a key", sw(VALID, r#""id":"g1""#, r#""id" "g1""#), "expected ':'"),
            ("duplicate key", sw(VALID, r#""both_ends":false"#, r#""both_ends":false,"id":"g1""#), "duplicate key"),
            ("trailing garbage", format!("{}x", VALID), "trailing bytes"),
            ("bad escape", sw(VALID, "ci/c.sh", r"a\qb"), "bad escape"),
            ("lone surrogate", sw(VALID, "ci/c.sh", r"a\ud800b"), "lone surrogate"),
            ("signed unicode escape", sw(VALID, "ci/c.sh", r"a\u+041b"), "bad unicode escape"),
            ("short unicode escape", sw(VALID, "ci/c.sh", r"a\u00"), "unicode escape"),
            ("array root", "[]".to_string(), "object"),
            ("bare string", "\"x\"".to_string(), "object"),
            ("empty file", String::new(), "object"),
        ];
        for (name, text, why) in &arms {
            let e = load_one("g1.json", text.as_bytes()).expect_err(name);
            assert!(e.contains("g1.json"), "arm {}: file named: {}", name, e);
            assert!(e.contains(why), "arm {} refused for the wrong reason: {}", name, e);
            let field = match name.split(' ').next().unwrap() {
                "id" | "missing" | "duplicate" => Some("`id`"),
                "program" | "bad" | "lone" | "signed" | "short" => Some("`program`"),
                "timeout" => Some("`timeout_ms`"),
                "max" => Some("`max_output_bytes`"),
                "both_ends" => Some("`both_ends`"),
                "args" => Some("`args`"),
                _ => None,
            };
            if let Some(f) = field {
                assert!(e.contains(f), "arm {} names its field {}: {}", name, f, e);
            }
        }
        let e = load_one("g1.json", b"{\"id\":\"g\xff1\"}").unwrap_err();
        assert!(e.contains("g1.json") && e.contains("utf-8"), "non-utf-8: {}", e);
        let exact = format!("{}{}", VALID, " ".repeat(1_048_576 - VALID.len()));
        assert_eq!(exact.len(), 1_048_576);
        assert!(load_one("g1.json", exact.as_bytes()).is_ok(), "exactly 1 MiB loads");
        let big = format!("{} ", exact);
        let e = load_one("g1.json", big.as_bytes()).unwrap_err();
        assert!(e.contains("g1.json") && e.contains("too large"), "over 1 MiB: {}", e);
        let max = sw(&sw(VALID, "1000", "18446744073709551615"), "4096", "18446744073709551615");
        let g = load_one("g1.json", max.as_bytes()).expect("the largest 64-bit values load");
        assert_eq!((g.get("g1").unwrap().timeout_ms, g.get("g1").unwrap().max_output_bytes), (u64::MAX, u64::MAX));
        let spaced = "{ \"id\" : \"g1\" ,\n \"program\" : \"ci/c.sh\" , \"args\" : [ \"a\" , \"b\\\"c\" ] , \"timeout_ms\" : 1000 ,\n\"max_output_bytes\" : 4096 , \"both_ends\" : true }\n";
        let ok = load_one("g1.json", spaced.as_bytes()).expect("the control arm loads");
        let g = ok.get("g1").unwrap();
        assert_eq!((g.args.as_slice(), g.both_ends), (&["a".to_string(), "b\"c".to_string()][..], true));
    }

    #[test]
    fn the_gates_folder_contract_is_enforced() {
        let junk = b"this is not json";
        // the reserved path-map.json is skipped, junk or not, and the load still succeeds beside a valid gate
        let s = root_with("g1.json", VALID.as_bytes());
        put(&s.0, "gates/path-map.json", junk);
        put(&s.0, "gates/.gitkeep", b"");
        put(&s.0, "gates/README.md", junk);
        put(&s.0, "gates/.hidden.json", junk);
        put(&s.0, "gates/JUNK.JSON", junk);
        assert_eq!(Gates::load(&s.0).expect("reserved, carrier and dot names are skipped").ids(), vec!["g1"]);
        // an unrecognized json file is refused, naming it
        put(&s.0, "gates/weird.json", junk);
        let e = Gates::load(&s.0).unwrap_err();
        assert!(e.contains("weird.json"), "{}", e);
        // the first refusal is in file-name order: a.json is refused before weird.json is reached
        put(&s.0, "gates/a.json", junk);
        assert!(Gates::load(&s.0).unwrap_err().contains("a.json"));
        // a subdirectory is refused, naming it
        let d = Scratch::new("sub");
        put(&d.0, "gates/g1.json", VALID.as_bytes());
        std::fs::create_dir_all(d.0.join("gates/sub")).unwrap();
        let e = Gates::load(&d.0).unwrap_err();
        assert!(e.contains("sub") && e.contains("not a regular file"), "{}", e);
        // only non-json carriers: no gate files
        let c = Scratch::new("carriers");
        put(&c.0, "gates/.gitkeep", b"");
        put(&c.0, "gates/README.md", junk);
        put(&c.0, "gates/path-map.json", junk);
        let e = Gates::load(&c.0).unwrap_err();
        assert!(e.contains("gates folder") && e.contains("no gate files"), "{}", e);
        // a missing gates folder, and gates as a plain file
        let m = Scratch::new("missing");
        assert!(Gates::load(&m.0).unwrap_err().contains("gates folder"), "no gates folder");
        put(&m.0, "gates", b"x");
        assert!(Gates::load(&m.0).unwrap_err().contains("gates folder"), "gates is a plain file");
        let nowhere = std::env::temp_dir().join(format!("hg-nowhere-{}", std::process::id()));
        assert!(Gates::load(&nowhere).unwrap_err().contains("gates folder"), "a repo root that does not exist");
        // an uppercase stem is refused by the id rule, naming the file
        let e = load_one("Cargo-Test.json", sw(VALID, r#""id":"g1""#, r#""id":"Cargo-Test""#).as_bytes()).unwrap_err();
        assert!(e.contains("Cargo-Test.json") && e.contains("`id`"), "{}", e);
        #[cfg(unix)]
        {
            let d = Scratch::new("dangling");
            put(&d.0, "gates/g1.json", VALID.as_bytes());
            std::os::unix::fs::symlink(d.0.join("nowhere"), d.0.join("gates/d.json")).unwrap();
            let e = Gates::load(&d.0).unwrap_err();
            assert!(e.contains("gates/d.json"), "a dangling symlink is refused naming the file: {}", e);
            let z = Scratch::new("noread");
            put(&z.0, "gates/g1.json", VALID.as_bytes());
            put(&z.0, "gates/z.json", sw(VALID, r#""id":"g1""#, r#""id":"z""#).as_bytes());
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(z.0.join("gates/z.json"), std::fs::Permissions::from_mode(0o000)).unwrap();
            }
            // a process that can read a mode-000 file (root) cannot plant this arm
            if std::fs::File::open(z.0.join("gates/z.json")).is_err() {
                let e = Gates::load(&z.0).unwrap_err();
                assert!(e.contains("gates/z.json"), "an unreadable file is refused naming the file: {}", e);
            }
            let f = Scratch::new("fifo");
            put(&f.0, "gates/g1.json", VALID.as_bytes());
            let made = Command::new("mkfifo").arg(f.0.join("gates/pipe")).status().expect("mkfifo runs");
            assert!(made.success());
            let e = Gates::load(&f.0).unwrap_err();
            assert!(e.contains("pipe") && e.contains("not a regular file"), "a FIFO is refused, not opened: {}", e);
            let l = Scratch::new("dirlink");
            put(&l.0, "gates/g1.json", VALID.as_bytes());
            std::os::unix::fs::symlink(&l.0, l.0.join("gates/s.json")).unwrap();
            let e = Gates::load(&l.0).unwrap_err();
            assert!(e.contains("s.json") && e.contains("not a regular file"), "a symlink to a directory is refused: {}", e);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_gate_file_or_program_outside_the_repo_is_refused() {
        let out = Scratch::new("out");
        put(&out.0, "gate.json", sw(VALID, r#""id":"g1""#, r#""id":"out""#).as_bytes());
        put_exec(&out.0, "prog.sh", "#!/bin/sh\nexit 0\n");
        let root = Scratch::new("root");
        put(&root.0, "gates/g1.json", VALID.as_bytes());
        std::os::unix::fs::symlink(out.0.join("gate.json"), root.0.join("gates/out.json")).unwrap();
        let e = Gates::load(&root.0).unwrap_err();
        assert!(e.contains("out.json") && e.contains("escapes"), "a gate file symlinked out is refused: {}", e);
        std::fs::remove_file(root.0.join("gates/out.json")).unwrap();
        assert_eq!(Gates::load(&root.0).expect("the in-root file loads").ids(), vec!["g1"]);
        // an in-root symlink to an in-root gate file is fine; only an escape is refused
        put(&root.0, "real/g2.json", sw(VALID, r#""id":"g1""#, r#""id":"g2""#).as_bytes());
        std::os::unix::fs::symlink(root.0.join("real/g2.json"), root.0.join("gates/g2.json")).unwrap();
        assert_eq!(Gates::load(&root.0).unwrap().ids(), vec!["g1", "g2"]);
        // a gates folder symlinked outward refuses every candidate under it
        let o2 = Scratch::new("gout");
        put(&o2.0, "g1.json", VALID.as_bytes());
        let r2 = Scratch::new("gate-link");
        std::os::unix::fs::symlink(&o2.0, r2.0.join("gates")).unwrap();
        assert!(Gates::load(&r2.0).unwrap_err().contains("escapes"), "a gates folder symlinked out is refused");
        // a program symlinked out of the root: the merged Check::load refuses, and its message passes through verbatim
        let p = Scratch::new("prog");
        put(&p.0, "gates/g1.json", sw(VALID, "ci/c.sh", "link.sh").as_bytes());
        std::os::unix::fs::symlink(out.0.join("prog.sh"), p.0.join("link.sh")).unwrap();
        let gates = Gates::load(&p.0).unwrap();
        let gate = gates.get("g1").unwrap();
        let got = to_check(&p.0, gate).unwrap_err();
        let direct = Check::load(&p.0, "g1", "link.sh", &[], Duration::from_millis(1000), 4096).unwrap_err();
        assert_eq!(got, direct, "the composition passes the runner's refusal through");
    }

    #[cfg(unix)]
    #[test]
    fn the_both_ends_flag_selects_the_both_ends_judge() {
        use crate::both_ends;
        let _g = EXEC.lock().unwrap_or_else(|e| e.into_inner());
        let gate_text = |both: bool| format!(r#"{{"id":"both-ends","program":"ci/check.sh","args":[],"timeout_ms":20000,"max_output_bytes":4000,"both_ends":{}}}"#, both);
        let check = "#!/bin/sh\ntest \"$(cat value.txt)\" = good\n";
        let only_main = |repo: &Path| {
            let list = git(repo, &["worktree", "list", "--porcelain"]);
            assert_eq!(list.lines().filter(|l| l.starts_with("worktree ")).count(), 1, "stale worktrees:\n{}", list);
        };
        // red at base (value bad), green at head (value good, check committed): judged PASS naming the rev-parse head
        let a = Scratch::new("red");
        git(&a.0, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&a.0, &["config", k, v]);
        }
        put(&a.0, "value.txt", b"bad");
        git(&a.0, &["add", "-A"]);
        git(&a.0, &["commit", "-q", "-m", "base"]);
        let base = git(&a.0, &["rev-parse", "HEAD"]);
        put(&a.0, "value.txt", b"good");
        put_exec(&a.0, "ci/check.sh", check);
        git(&a.0, &["add", "-A"]);
        git(&a.0, &["commit", "-q", "-m", "head"]);
        let head = git(&a.0, &["rev-parse", "HEAD"]);
        put(&a.0, "gates/both-ends.json", gate_text(true).as_bytes());
        let gates = Gates::load(&a.0).unwrap();
        let gate = gates.get("both-ends").unwrap();
        assert!(gate.both_ends);
        let r = with_spec(gate, |spec| both_ends::judge(&a.0, &base, &head, &head, spec)).unwrap().unwrap();
        assert_eq!(r.status, Status::Pass, "{:?} {}", r.failure_class, r.message);
        assert_eq!(r.head_sha, head);
        assert_eq!(r.gate_id, "both-ends");
        assert!(r.validate().is_ok());
        only_main(&a.0);
        // green at base too: the judged path fails it with passes-at-base
        let b = Scratch::new("green");
        git(&b.0, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&b.0, &["config", k, v]);
        }
        put(&b.0, "value.txt", b"good");
        put_exec(&b.0, "ci/check.sh", check);
        git(&b.0, &["add", "-A"]);
        git(&b.0, &["commit", "-q", "-m", "base"]);
        let base_b = git(&b.0, &["rev-parse", "HEAD"]);
        put(&b.0, "note.txt", b"x");
        git(&b.0, &["add", "-A"]);
        git(&b.0, &["commit", "-q", "-m", "head"]);
        let head_b = git(&b.0, &["rev-parse", "HEAD"]);
        put(&b.0, "gates/both-ends.json", gate_text(true).as_bytes());
        let gates = Gates::load(&b.0).unwrap();
        let r = with_spec(gates.get("both-ends").unwrap(), |spec| both_ends::judge(&b.0, &base_b, &head_b, &head_b, spec)).unwrap().unwrap();
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.failure_class.as_deref(), Some("passes-at-base"), "{}", r.message);
        only_main(&b.0);
        // the same gate with the flag false goes through the plain runner: not judged, so the same repo PASSES
        put(&b.0, "gates/both-ends.json", gate_text(false).as_bytes());
        let gates = Gates::load(&b.0).unwrap();
        let gate = gates.get("both-ends").unwrap();
        assert!(!gate.both_ends);
        let check = to_check(&b.0, gate).unwrap();
        let r = runner::run(&b.0, &check, &head_b);
        assert_eq!(r.status, Status::Pass, "{:?} {}", r.failure_class, r.message);
        assert_eq!(r.head_sha, head_b);
        assert!(r.validate().is_ok());
        // the spec carries the gate's own values
        with_spec(gate, |spec| {
            assert_eq!((spec.id, spec.program, spec.args.len()), ("both-ends", "ci/check.sh", 0));
            assert_eq!((spec.budget, spec.max_output), (Duration::from_millis(20000), 4000));
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn loading_and_building_a_check_run_nothing() {
        let s = Scratch::new("norun");
        let marker = s.0.join("ran.txt");
        put(&s.0, "gates/mark.json", sw(VALID, r#""id":"g1""#, r#""id":"mark""#).replace("ci/c.sh", "mark.sh").as_bytes());
        put_exec(&s.0, "mark.sh", &format!("#!/bin/sh\ntouch {}\n", marker.display()));
        let gates = Gates::load(&s.0).expect("the gate loads");
        assert!(!marker.exists(), "load ran a gate's program");
        let _check = to_check(&s.0, gates.get("mark").unwrap()).expect("the check builds");
        assert!(!marker.exists(), "building a check ran the program");
        let _ = with_spec(gates.get("mark").unwrap(), |_| ()).unwrap();
        assert!(!marker.exists(), "building a spec ran the program");
    }

    #[test]
    fn map_check_ids_resolve_against_declared_gate_ids() {
        let s = Scratch::new("ids");
        put(&s.0, "gates/one.json", sw(VALID, r#""id":"g1""#, r#""id":"one""#).as_bytes());
        put(&s.0, "gates/two.json", sw(VALID, r#""id":"g1""#, r#""id":"two""#).as_bytes());
        let gates = Gates::load(&s.0).unwrap();
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(resolve_map_ids(&gates, &ids(&["one", "two"])).is_ok());
        let e = resolve_map_ids(&gates, &ids(&["one", "nope", "two"])).unwrap_err();
        assert!(e.contains("nope"), "{}", e);
        assert!(resolve_map_ids(&gates, &[]).is_ok(), "the empty slice is the caller's policy");
        assert_eq!(gates.ids(), vec!["one", "two"], "sorted by id");
        let many = Scratch::new("many");
        let names = ["m", "k", "t", "b", "z", "d", "q", "a", "x", "f", "p", "h", "a-b"];
        for n in names {
            put(&many.0, &format!("gates/{}.json", n), sw(VALID, r#""id":"g1""#, &format!(r#""id":"{}""#, n)).as_bytes());
        }
        let mut sorted = names.to_vec();
        sorted.sort();
        assert_eq!(Gates::load(&many.0).unwrap().ids(), sorted, "ids are sorted whatever order the folder lists them in");
    }

    /// The strings of the first `"required": [...]` array in a JSON text.
    fn required_of(schema: &str) -> Vec<String> {
        let at = schema.find("\"required\"").expect("schema has a required array");
        let open = at + schema[at..].find('[').unwrap();
        let close = open + schema[open..].find(']').unwrap();
        schema[open + 1..close].split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
    }

    #[test]
    fn the_committed_seed_gate_and_schema_match_the_loader() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let gates = Gates::load(&repo).expect("the committed gates folder loads");
        assert_eq!(gates.ids(), vec!["cargo-test", "core-layering"], "path-map.json is reserved, the carrier and README are ignored");
        let seed = gates.get("cargo-test").unwrap();
        assert!(!seed.both_ends);
        assert!(to_check(&repo, seed).is_ok(), "the seed's program resolves under the repo");
        let map = crate::path_map::PathMap::load(&repo).expect("the committed path map loads beside the gates");
        for path in ["core/rust/harness/src/lib.rs", "gates/cargo-test.json"] {
            assert!(resolve_map_ids(&gates, &map.checks_for(path).to_vec()).is_ok(), "the map's ids for {} are declared gates", path);
        }
        let schema = std::fs::read_to_string(repo.join("schemas/gate.schema.json")).expect("the schema is committed and utf-8");
        let mut got = required_of(&schema);
        let mut want: Vec<String> = FIELDS.iter().map(|s| s.to_string()).collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "the schema's required set equals the loader's six fields");
        assert_eq!(FIELDS.len(), 6);
    }
}
