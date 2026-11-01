//! Path-to-check map (std only). `gates/path-map.json` says which checks watch which paths; a changed path no entry covers is a FAIL.
//! The module runs no check. It reads the map, asks git (one call) which paths a diff touched, and judges coverage.

use crate::gate_result::{GateResult, Status};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const GATE_ID: &str = "path-map";
const MAX_MAP_BYTES: u64 = 1024 * 1024;

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn id_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

/// A plain repo-relative path: `/`-separated, every component a plain name, no `.git` in any case.
/// The empty string splits into one empty component, so it is refused with the other empty components.
fn path_ok(p: &str) -> bool {
    !p.chars().any(|c| c == '\\' || c.is_control())
        && p.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.eq_ignore_ascii_case(".git"))
}

#[derive(Debug)]
pub struct PathMap {
    /// Validated, sorted by path.
    entries: Vec<(String, Vec<String>)>,
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

    /// A string with no escapes and no control byte: nothing in a path or a check id needs either.
    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => return Err("escape in string".to_string()),
                c if c < 0x20 => return Err("raw control byte in string".to_string()),
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid utf-8".to_string())
    }
}

impl PathMap {
    /// Strict parse of the one shape `{"<path>": ["<check-id>", ...], ...}`. A zero-entry map is refused.
    pub fn parse(text: &str) -> Result<PathMap, String> {
        let mut p = P { b: text.as_bytes(), i: 0 };
        p.eat(b'{')?;
        if p.peek() == Some(b'}') {
            return Err("map has no entries".to_string());
        }
        let mut entries: Vec<(String, Vec<String>)> = Vec::new();
        loop {
            let path = p.string()?;
            if !path_ok(&path) {
                return Err("map path malformed".to_string());
            }
            p.eat(b':')?;
            p.eat(b'[')?;
            if p.peek() == Some(b']') {
                return Err("map entry has no checks".to_string());
            }
            let mut ids: Vec<String> = Vec::new();
            loop {
                let id = p.string()?;
                if !id_ok(&id) {
                    return Err("check id malformed".to_string());
                }
                ids.push(id);
                match p.peek() {
                    Some(b',') => p.i += 1,
                    Some(b']') => {
                        p.i += 1;
                        break;
                    }
                    _ => return Err("expected ',' or ']'".to_string()),
                }
            }
            if entries.iter().any(|(k, _)| *k == path) {
                return Err("duplicate key".to_string());
            }
            entries.push((path, ids));
            match p.peek() {
                Some(b',') => p.i += 1,
                Some(b'}') => {
                    p.i += 1;
                    break;
                }
                _ => return Err("expected ',' or '}'".to_string()),
            }
        }
        p.ws();
        if p.i != p.b.len() {
            return Err("trailing bytes".to_string());
        }
        entries.sort();
        Ok(PathMap { entries })
    }

    /// The one load root: `<repo_root>/gates/path-map.json`, repo root canonicalized once.
    pub fn load(repo_root: &Path) -> Result<PathMap, String> {
        let root = repo_root.canonicalize().map_err(|_| "repo root unreadable".to_string())?;
        PathMap::load_canonical(&root)
    }

    fn load_canonical(root: &Path) -> Result<PathMap, String> {
        let real = root.join("gates").join("path-map.json").canonicalize().map_err(|_| "path map missing".to_string())?;
        if !real.starts_with(root) {
            return Err("path map escapes the repo".to_string());
        }
        if !std::fs::metadata(&real).map_err(|_| "path map unreadable".to_string())?.is_file() {
            return Err("path map is not a file".to_string());
        }
        let f = std::fs::File::open(&real).map_err(|_| "path map unreadable".to_string())?;
        let mut bytes = Vec::new();
        f.take(MAX_MAP_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "path map unreadable".to_string())?;
        if bytes.len() as u64 > MAX_MAP_BYTES {
            return Err("path map too large".to_string());
        }
        let text = String::from_utf8(bytes).map_err(|_| "path map is not utf-8".to_string())?;
        PathMap::parse(&text)
    }

    /// The checks `changed` maps to (empty when none). Byte-exact, case-sensitive, whole components: an entry covers itself and
    /// everything strictly below it, never a substring. When entries nest, the longest (most specific) one answers.
    pub fn checks_for(&self, changed: &str) -> &[String] {
        let mut best: Option<&(String, Vec<String>)> = None;
        for e in &self.entries {
            let hit = changed == e.0 || (changed.starts_with(e.0.as_str()) && changed.as_bytes().get(e.0.len()) == Some(&b'/'));
            if hit && best.map_or(true, |b| e.0.len() > b.0.len()) {
                best = Some(e);
            }
        }
        best.map_or(&[], |e| e.1.as_slice())
    }
}

/// Pure judgement. FAIL (class `unmapped-path`, file = the first unmapped path in the given order) or PASS; Err on an invalid head
/// sha or an invalid changed path. An empty list is PASS.
pub fn judge(map: &PathMap, changed: &[String], head_sha: &str) -> Result<GateResult, String> {
    if !sha_ok(head_sha) {
        return Err("head is not a full lowercase hex sha".to_string());
    }
    if changed.iter().any(|c| !path_ok(c)) {
        return Err("changed path malformed".to_string());
    }
    let r = match changed.iter().find(|c| map.checks_for(c).is_empty()) {
        Some(p) => GateResult {
            status: Status::Fail,
            gate_id: GATE_ID.to_string(),
            head_sha: head_sha.to_string(),
            failure_class: Some("unmapped-path".to_string()),
            message: format!("changed path {} is not covered by the path map", p),
            file: Some(p.clone()),
            line: None,
        },
        None => GateResult {
            status: Status::Pass,
            gate_id: GATE_ID.to_string(),
            head_sha: head_sha.to_string(),
            failure_class: None,
            message: "every changed path is covered by the path map".to_string(),
            file: None,
            line: None,
        },
    };
    r.validate()?;
    Ok(r)
}

/// One git call: no hooks, no inherited `GIT_*` name, no stdin, run from the canonical root and pinned to that root's own `.git`
/// (git never walks up to a parent repo, so the map root and the root paths are reported against are the same).
pub(crate) fn changed_paths(root: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let mut c = Command::new("git");
    c.current_dir(root)
        .arg("--git-dir")
        .arg(root.join(".git"))
        .arg("--work-tree")
        .arg(root)
        .args(["-c", "core.hooksPath=/dev/null", "diff", "--name-only", "-z", "--no-renames", "--ignore-submodules=none"])
        .arg(format!("{}^{{commit}}", base))
        .arg(format!("{}^{{commit}}", head))
        .arg("--");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    let out = c.stdin(Stdio::null()).output().map_err(|_| "git could not be started".to_string())?;
    if !out.status.success() {
        return Err("git refused the request".to_string());
    }
    let mut parts: Vec<&[u8]> = out.stdout.split(|b| *b == 0).collect();
    if parts.last().map_or(false, |l| l.is_empty()) {
        parts.pop();
    }
    parts
        .into_iter()
        .map(|p| {
            if p.is_empty() {
                return Err("git printed an empty path".to_string());
            }
            String::from_utf8(p.to_vec()).map_err(|_| "git printed a non-utf-8 path".to_string())
        })
        .collect()
}

/// The one composition entry. Every refusal precedes any verdict: shas, then the root (it must hold `.git`, so the map root and the
/// root git reports paths against are the same), then the map, then the one git call, then `judge`.
pub fn judge_diff(repo_root: &Path, base: &str, head: &str) -> Result<GateResult, String> {
    if !sha_ok(base) {
        return Err("base is not a full lowercase hex sha".to_string());
    }
    if !sha_ok(head) {
        return Err("head is not a full lowercase hex sha".to_string());
    }
    if base == head {
        return Err("base and head are the same commit".to_string());
    }
    let root: PathBuf = repo_root.canonicalize().map_err(|_| "repo root unreadable".to_string())?;
    if !root.join(".git").exists() {
        return Err("repo root is not a repository top level".to_string());
    }
    let map = PathMap::load_canonical(&root)?;
    let changed = changed_paths(&root, base, head)?;
    judge(&map, &changed, head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn map(text: &str) -> PathMap {
        PathMap::parse(text).expect("planted map parses")
    }

    /// A scratch directory removed on drop, even when an assertion fails.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("hpm-{}-{}-{}", name, std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
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

    fn put(dir: &Path, rel: &str, body: &str) {
        let f = dir.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, body).unwrap();
    }

    fn put_map(dir: &Path, text: &str) {
        put(dir, "gates/path-map.json", text);
    }

    #[test]
    fn a_changed_path_not_in_the_map_fails_naming_it() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let m = map(r#"{"docs":["cargo-test"]}"#);
        let r = judge(&m, &s(&["docs/a.md", "notes/x.md"]), SHA).unwrap();
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.failure_class.as_deref(), Some("unmapped-path"));
        assert_eq!(r.file.as_deref(), Some("notes/x.md"));
        assert!(r.message.contains("notes/x.md"));
        assert_eq!(r.head_sha, SHA);
        assert_eq!(r.gate_id, "path-map");
        assert!(r.validate().is_ok());
        let first = judge(&m, &s(&["a/one", "b/two"]), SHA).unwrap();
        assert_eq!(first.file.as_deref(), Some("a/one"), "first unmapped in the given order");
    }

    #[test]
    fn the_same_diff_with_the_path_mapped_passes() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let m = map(r#"{"docs":["cargo-test"],"notes":["cargo-test"]}"#);
        let r = judge(&m, &s(&["docs/a.md", "notes/x.md"]), SHA).unwrap();
        assert_eq!(r.status, Status::Pass);
        assert_eq!(r.failure_class, None);
        assert_eq!(r.file, None);
        assert_eq!(r.head_sha, SHA);
        assert!(r.validate().is_ok());
        let empty = judge(&m, &[], SHA).unwrap();
        assert_eq!(empty.status, Status::Pass, "an empty list is a pass");
        assert!(empty.validate().is_ok());
    }

    #[test]
    fn an_entry_maps_only_whole_components_below_it() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let sub = map(r#"{"core/rus":["c"]}"#);
        let r = judge(&sub, &s(&["core/rust/x.rs"]), SHA).unwrap();
        assert_eq!(r.status, Status::Fail, "a substring is not a prefix");
        assert_eq!(r.file.as_deref(), Some("core/rust/x.rs"));
        let deep = map(r#"{"core/rust":["c"]}"#);
        for ok in ["core/rust/harness/src/lib.rs", "core/rust/x.rs", "core/rust"] {
            assert_eq!(judge(&deep, &s(&[ok]), SHA).unwrap().status, Status::Pass, "{} is covered", ok);
        }
        for no in ["core/rustic/x.rs", "Core/rust/x.rs", "Core/rust", "CORE/RUST", "core", "core/rus"] {
            assert_eq!(judge(&deep, &s(&[no]), SHA).unwrap().status, Status::Fail, "{} is not covered", no);
        }
        let nested = map(r#"{"a":["wide"],"a/b":["narrow"]}"#);
        assert_eq!(nested.checks_for("a/b/c"), ["narrow".to_string()]);
        assert_eq!(nested.checks_for("a/x"), ["wide".to_string()]);
    }

    #[test]
    fn malformed_map_entries_are_refused_at_parse() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let arms: &[(&str, &str, &str)] = &[
            ("absolute", r#"{"/etc/x":["c"]}"#, "map path"),
            ("dot-dot", r#"{"..":["c"]}"#, "map path"),
            ("dot-dot inside", r#"{"a/../b":["c"]}"#, "map path"),
            ("dot", r#"{".":["c"]}"#, "map path"),
            ("empty component", r#"{"a//b":["c"]}"#, "map path"),
            ("trailing slash", r#"{"docs/":["c"]}"#, "map path"),
            ("backslash", r#"{"a\\b":["c"]}"#, "escape"),
            (".git", r#"{".git":["c"]}"#, "map path"),
            (".GIT", r#"{".GIT":["c"]}"#, "map path"),
            (".Git inside", r#"{"src/.Git/x":["c"]}"#, "map path"),
            ("empty path", r#"{"":["c"]}"#, "map path"),
            ("empty id list", r#"{"a":[]}"#, "no checks"),
            ("bad id", r#"{"a":["BAD ID!"]}"#, "check id"),
            ("empty id", r#"{"a":[""]}"#, "check id"),
            ("duplicate key", r#"{"a":["c"],"a":["d"]}"#, "duplicate key"),
            ("zero entries", "{}", "no entries"),
            ("array root", "[]", ""),
            ("string value", r#"{"a":"c"}"#, ""),
            ("raw newline in string", "{\"a\nb\":[\"c\"]}", "control byte"),
            ("raw NUL in string", "{\"a\u{0}b\":[\"c\"]}", "control byte"),
            ("escaped NUL", r#"{"a\u0000b":["c"]}"#, "escape"),
            ("trailing garbage", r#"{"a":["c"]}x"#, "trailing bytes"),
            ("unterminated", r#"{"a":["c"]"#, ""),
        ];
        for (name, text, why) in arms {
            let e = PathMap::parse(text).expect_err(name);
            assert!(e.contains(why), "arm {} refused for the wrong reason: {}", name, e);
        }
        let ok = PathMap::parse("{ \"a\" : [ \"c\" , \"d\" ] ,\n \"b/c\" : [\"e\"] }\n").unwrap();
        assert_eq!(ok.checks_for("a/x"), ["c".to_string(), "d".to_string()]);
        let seed = PathMap::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")).expect("the committed seed map loads");
        for probe in ["core/rust/harness/src/lib.rs", "gates/path-map.json"] {
            let got = seed.checks_for(probe);
            assert!(
                got.contains(&"cargo-test".to_string()) && got.contains(&"core-layering".to_string()),
                "{} must select both cargo-test and core-layering, got {:?}",
                probe,
                got
            );
        }
    }

    #[test]
    fn a_map_file_outside_or_missing_is_refused() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let good = r#"{"docs":["cargo-test"]}"#;
        let out = Scratch::new("out");
        put(&out.0, "elsewhere.json", good);
        let root = Scratch::new("root");
        std::fs::create_dir_all(root.0.join("gates")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(out.0.join("elsewhere.json"), root.0.join("gates/path-map.json")).unwrap();
            assert!(PathMap::load(&root.0).is_err(), "symlink out of the root is refused");
            std::fs::remove_file(root.0.join("gates/path-map.json")).unwrap();
        }
        assert!(PathMap::load(&root.0).is_err(), "missing file is refused, never an empty map");
        let nogates = Scratch::new("nogates");
        assert!(PathMap::load(&nogates.0).is_err(), "no gates folder is refused");
        std::fs::create_dir_all(root.0.join("gates/path-map.json")).unwrap();
        assert!(PathMap::load(&root.0).unwrap_err().contains("not a file"), "a directory in the file's place is refused");
        std::fs::remove_dir(root.0.join("gates/path-map.json")).unwrap();
        let big = format!("{}{}", good, " ".repeat(MAX_MAP_BYTES as usize));
        put_map(&root.0, &big);
        assert!(PathMap::load(&root.0).unwrap_err().contains("too large"), "over 1 MiB is refused");
        std::fs::write(root.0.join("gates/path-map.json"), b"{\"a\xff\":[\"c\"]}").unwrap();
        assert!(PathMap::load(&root.0).is_err(), "non-utf-8 is refused");
        put_map(&root.0, good);
        let m = PathMap::load(&root.0).expect("a good in-root map loads");
        assert_eq!(m.checks_for("docs/a.md"), ["cargo-test".to_string()]);
        assert!(m.checks_for("notes/a.md").is_empty());
    }

    #[test]
    fn changed_paths_git_cannot_emit_are_refused() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let m = map(r#"{"docs":["c"]}"#);
        for bad in ["/abs/x", "a/../b", ".git", ".Git/x", "a\\b", "", "docs/", "docs//a", "docs/a\nb"] {
            assert!(judge(&m, &s(&["docs/a.md", bad]), SHA).is_err(), "changed path refused: {:?}", bad);
        }
        assert!(judge(&m, &s(&["docs/a.md"]), SHA).is_ok(), "the control list is accepted");
    }

    #[test]
    fn a_real_diff_between_two_commits_is_judged() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let sc = Scratch::new("diff");
        let d = &sc.0;
        git(d, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(d, &["config", k, v]);
        }
        put(d, "core/rust/lib.rs", "fn main() {}\n");
        put(d, "a.md", "one\ntwo\nthree\nfour\nfive\n");
        git(d, &["add", "core", "a.md"]);
        git(d, &["commit", "-q", "-m", "one"]);
        let c1 = git(d, &["rev-parse", "HEAD"]);
        put(d, "docs/new.md", "new\n");
        git(d, &["add", "docs"]);
        git(d, &["commit", "-q", "-m", "two"]);
        let c2 = git(d, &["rev-parse", "HEAD"]);
        git(d, &["mv", "a.md", "b.md"]);
        git(d, &["commit", "-q", "-m", "three"]);
        let c3 = git(d, &["rev-parse", "HEAD"]);

        put_map(d, r#"{"core/rust":["c"]}"#);
        let r = judge_diff(d, &c1, &c2).unwrap();
        assert_eq!(r.status, Status::Fail, "arm 1");
        assert_eq!(r.file.as_deref(), Some("docs/new.md"));
        assert_eq!(r.head_sha, c2);

        put_map(d, r#"{"core/rust":["c"],"docs":["c"]}"#);
        let r = judge_diff(d, &c1, &c2).unwrap();
        assert_eq!(r.status, Status::Pass, "arm 2");
        assert_eq!(r.head_sha, c2, "names the head from rev-parse");
        assert!(r.validate().is_ok());

        put_map(d, r#"{"core/rust":["c"],"b.md":["c"]}"#);
        let r = judge_diff(d, &c2, &c3).unwrap();
        assert_eq!(r.status, Status::Fail, "arm 3: the old path of a rename is unmapped");
        assert_eq!(r.file.as_deref(), Some("a.md"));

        put_map(d, r#"{"core/rust":["c"]}"#);
        let r = judge_diff(d, &c2, &c3).unwrap();
        assert_eq!(r.status, Status::Fail, "arm 3b: neither path mapped");
        assert_eq!(r.file.as_deref(), Some("a.md"));

        let base_bad = "base is not a full lowercase hex sha";
        let head_bad = "head is not a full lowercase hex sha";
        assert_eq!(judge_diff(d, &c1, &c1).unwrap_err(), "base and head are the same commit");
        for n in [39usize, 41, 63, 65] {
            let bad = "a".repeat(n);
            assert_eq!(judge_diff(d, &c1, &bad).unwrap_err(), head_bad, "head length {}", n);
            assert_eq!(judge_diff(d, &bad, &c2).unwrap_err(), base_bad, "base length {}", n);
        }
        assert_eq!(judge_diff(d, &c1, &c2.to_uppercase()).unwrap_err(), head_bad, "uppercase head");
        assert_eq!(judge_diff(d, &c1.to_uppercase(), &c2).unwrap_err(), base_bad, "uppercase base");
        let absent = "b".repeat(40);
        assert_eq!(judge_diff(d, &c1, &absent).unwrap_err(), "git refused the request", "a sha absent from the repo");
        let sha64 = "c".repeat(64);
        let r = judge(&map(r#"{"docs":["c"]}"#), &s(&["docs/a.md"]), &sha64).unwrap();
        assert_eq!(r.head_sha, sha64, "a 64-character sha is accepted");
        assert!(r.validate().is_ok());

        let p = sc.0.clone();
        drop(sc);
        assert!(!p.exists(), "no scratch left behind");
    }

    #[test]
    #[ignore]
    fn child_judge_diff() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let (root, base, head) = (std::env::var("HPM_ROOT").unwrap(), std::env::var("HPM_BASE").unwrap(), std::env::var("HPM_HEAD").unwrap());
        println!("HPM-RESULT:{:?}", judge_diff(Path::new(&root), &base, &head));
    }

    #[test]
    fn git_env_and_config_cannot_steer_the_diff() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let sc = Scratch::new("env");
        let decoy = Scratch::new("decoy");
        let d = &sc.0;
        git(d, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(d, &["config", k, v]);
        }
        put(d, "core/rust/lib.rs", "fn main() {}\n");
        git(d, &["add", "core"]);
        git(d, &["commit", "-q", "-m", "one"]);
        let c1 = git(d, &["rev-parse", "HEAD"]);
        put(d, "docs/new.md", "new\n");
        git(d, &["add", "docs"]);
        git(d, &["commit", "-q", "-m", "two"]);
        let c2 = git(d, &["rev-parse", "HEAD"]);
        put_map(d, r#"{"core/rust":["c"]}"#);
        git(&decoy.0, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&decoy.0, &["config", k, v]);
        }
        let clean = format!("{:?}", judge_diff(d, &c1, &c2));
        assert!(clean.contains("unmapped-path"), "the clean run fails the unmapped path: {}", clean);

        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--ignored", "--exact", "path_map::tests::child_judge_diff", "--nocapture", "--test-threads=1"])
            .env("HPM_ROOT", d)
            .env("HPM_BASE", &c1)
            .env("HPM_HEAD", &c2)
            .env("GIT_DIR", decoy.0.join(".git"))
            .env("GIT_WORK_TREE", &decoy.0)
            .env("GIT_INDEX_FILE", decoy.0.join(".git/index"));
        let out = c.output().unwrap();
        assert!(out.status.success(), "child ran: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let line = text.lines().find_map(|l| l.split_once("HPM-RESULT:").map(|(_, r)| r)).expect("child printed a result");
        assert_eq!(line, clean, "a steered environment gives the same verdict as a clean one");

        // config steering: a repo that hides submodule changes must not hide a changed pointer
        let sub = Scratch::new("sub");
        let s = &sub.0;
        git(s, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(s, &["config", k, v]);
        }
        put(s, "core/rust/lib.rs", "fn main() {}\n");
        git(s, &["add", "core"]);
        git(s, &["update-index", "--add", "--cacheinfo", &format!("160000,{},vend", "1".repeat(40))]);
        git(s, &["commit", "-q", "-m", "one"]);
        let b = git(s, &["rev-parse", "HEAD"]);
        git(s, &["update-index", "--cacheinfo", &format!("160000,{},vend", "2".repeat(40))]);
        git(s, &["commit", "-q", "-m", "two"]);
        let h = git(s, &["rev-parse", "HEAD"]);
        git(s, &["config", "diff.ignoreSubmodules", "all"]);
        put_map(s, r#"{"core/rust":["c"]}"#);
        let r = judge_diff(s, &b, &h).unwrap();
        assert_eq!(r.status, Status::Fail, "a changed submodule pointer is still a changed path");
        assert_eq!(r.file.as_deref(), Some("vend"));
    }

    #[test]
    fn a_root_that_is_not_a_repository_top_level_is_refused() {
        let _spawn_gate = crate::caps::spawn_gate::read();
        let sc = Scratch::new("top");
        let d = &sc.0;
        git(d, &["init", "-q"]);
        // Stop git's detached upkeep from writing under .git mid-test (same fix as the intake fixture).
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(d, &["config", k, v]);
        }
        put(d, "core/rust/lib.rs", "fn main() {}\n");
        git(d, &["add", "core"]);
        git(d, &["commit", "-q", "-m", "one"]);
        let c1 = git(d, &["rev-parse", "HEAD"]);
        put(d, "docs/new.md", "new\n");
        git(d, &["add", "docs"]);
        git(d, &["commit", "-q", "-m", "two"]);
        let c2 = git(d, &["rev-parse", "HEAD"]);
        // a subfolder holds a map that would cover everything, while git reports paths from the top
        put(d, "sub/gates/path-map.json", r#"{"docs":["c"],"core":["c"]}"#);
        let e = judge_diff(&d.join("sub"), &c1, &c2).unwrap_err();
        assert!(e.contains("top level"), "a subfolder root is refused: {}", e);
        // an empty .git folder passes an exists() check but is not a repository: git must not walk up to the parent
        std::fs::create_dir_all(d.join("sub/.git")).unwrap();
        assert!(judge_diff(&d.join("sub"), &c1, &c2).is_err(), "an empty .git folder in a subfolder is refused");
        put_map(d, r#"{"docs":["c"],"core":["c"]}"#);
        assert_eq!(judge_diff(d, &c1, &c2).unwrap().status, Status::Pass, "the top level itself works");
    }
}
