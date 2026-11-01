//! The config loader (std only). One settings file, read-only, parsed by a strict hand-written subset of YAML: `docs/config-contract.md` is the contract.
//! Anything outside the contract is a refusal. A refusal names the section, the key and the line number, and never carries a value (a value may be a private folder).
//! The loader does not touch the filesystem for the repo map; whether a folder exists is the ticket intake's job.
//!
//! Repo ids that a YAML reader would read as a non-string (`true`, `false`, `null`, `yes`, `no`, `on`, `off`) are refused as map keys.

use crate::repo_id::RepoId;
use std::io::Read;
use std::path::Path;

const MAX_BYTES: u64 = 1024 * 1024;

/// Every required `section.key`, in contract order. The loader accepts exactly these (plus repo ids under `repos`).
pub const KEYS: [&str; 15] = [
    "budgets.s_wall_seconds",
    "budgets.s_tokens",
    "budgets.m_wall_seconds",
    "budgets.m_tokens",
    "budgets.l_wall_seconds",
    "budgets.l_tokens",
    "shadow.p1a_cap",
    "shadow.p4a_cap",
    "shadow.d2a_cap",
    "shadow.p7_cap",
    "shadow.p1a_token_budget",
    "shadow.p4a_token_budget",
    "shadow.d2a_token_budget",
    "shadow.p7_token_budget",
    "lanes.count",
];

/// The one string-valued key (config v3): the friction inbox path. Required, exactly like every numeric key; only its
/// value shape differs. A refusal over it never carries the value (the path names a private folder).
pub const INBOX_KEY: &str = "friction.inbox";

const SECTIONS: [&str; 5] = ["repos", "budgets", "shadow", "lanes", "friction"];
const YAML_WORDS: [&str; 11] = ["true", "false", "null", "yes", "no", "on", "off", "y", "n", ".inf", ".nan"];

/// The wall-time and token limit of one ticket size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub wall_seconds: u64,
    pub tokens: u64,
}

/// The shadow-trial caps and token budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shadow {
    pub p1a_cap: u64,
    pub p4a_cap: u64,
    pub d2a_cap: u64,
    pub p7_cap: u64,
    pub p1a_token_budget: u64,
    pub p4a_token_budget: u64,
    pub d2a_token_budget: u64,
    pub p7_token_budget: u64,
}

#[derive(Debug)]
pub struct Config {
    /// Validated, in file order: (repo id, folder).
    repos: Vec<(RepoId, String)>,
    /// Values in `KEYS` order.
    nums: [u64; 15],
    /// The `friction.inbox` path (v3). Never printed.
    inbox: String,
}

/// A refusal that names the line, and the section and key when known. Never carries a value.
fn refuse(line: usize, at: &str, why: &str) -> String {
    format!("config: line {line}: {at}: {why}")
}

/// A key or section name is echoed only when it is short and made of plain name characters; anything else is called "key".
fn shown(name: &str) -> &str {
    if !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')) {
        name
    } else {
        "key"
    }
}

/// Split a raw value into its text under the closed plain-value grammar. Returns (text, was_quoted) or the reason it is refused.
fn plain_value(v: &str) -> Result<(&str, bool), &'static str> {
    if v.starts_with('#') || v.starts_with("- ") || v.starts_with("? ") || v.starts_with(['{', '[', '&', '*', '|', '>', '!', '%', '@', '`', '\'']) {
        return Err("flow, anchor, tag, list or comment form is not allowed");
    }
    let (inner, quoted) = if let Some(rest) = v.strip_prefix('"') {
        let inner = rest.strip_suffix('"').ok_or("value is outside the plain-value grammar")?;
        if inner.contains('"') {
            return Err("value is outside the plain-value grammar");
        }
        if inner.contains('\\') {
            return Err("value is outside the plain-value grammar");
        }
        (inner, true)
    } else {
        if v.contains('"') {
            return Err("value is outside the plain-value grammar");
        }
        (v, false)
    };
    if inner.ends_with(' ') || inner.contains(" #") || inner.contains(": ") || inner.ends_with(':') {
        return Err("value is outside the plain-value grammar");
    }
    Ok((inner, quoted))
}

/// A positive whole number: digits only, no leading zero, at most 15 digits.
fn number(s: &str) -> Option<u64> {
    let ok = !s.is_empty() && s.len() <= 15 && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0');
    if ok {
        s.parse().ok()
    } else {
        None
    }
}

/// An absolute folder spelling: starts with `/`, no `..` component, no control character, no trailing `/`.
fn folder_ok(f: &str) -> bool {
    f.starts_with('/') && !f.ends_with('/') && !f.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) && !f.split('/').any(|c| c == "..")
}

impl Config {
    /// Parse the whole text. Fail closed: the first problem is the answer.
    pub fn parse(text: &str) -> Result<Config, String> {
        if text.is_empty() {
            return Err("config: file is empty".to_string());
        }
        let mut seen = [false; 5];
        let mut section: Option<usize> = None;
        let mut repos: Vec<(RepoId, String)> = Vec::new();
        let mut nums: [Option<u64>; 15] = [None; 15];
        let mut inbox: Option<String> = None;
        for (i, line) in text.split('\n').enumerate() {
            let n = i + 1;
            let here = match section {
                Some(s) => format!("section `{}`", SECTIONS[s]),
                None => "line".to_string(),
            };
            if line.contains('\t') {
                return Err(refuse(n, &here, "a tab is not allowed"));
            }
            if line.contains('\r') {
                return Err(refuse(n, &here, "a carriage return is not allowed"));
            }
            if line.is_empty() {
                continue;
            }
            let bare = line.trim_start_matches(' ');
            if bare.starts_with('#') {
                continue;
            }
            if bare.is_empty() {
                return Err(refuse(n, &here, "a line of only spaces is not allowed"));
            }
            if !line.starts_with(' ') {
                let name = line.strip_suffix(':').filter(|nm| shown(nm) != "key");
                let name = name.ok_or_else(|| refuse(n, "section", "expected a section name followed by a colon and nothing else"))?;
                let idx = SECTIONS.iter().position(|s| *s == name).ok_or_else(|| refuse(n, &format!("section `{name}`"), "unknown section"))?;
                if seen[idx] {
                    return Err(refuse(n, &format!("section `{name}`"), "duplicate section"));
                }
                seen[idx] = true;
                section = Some(idx);
                continue;
            }
            let sec = match section {
                Some(s) => SECTIONS[s],
                None => return Err(refuse(n, &format!("key `{}`", shown(bare.split(':').next().unwrap_or(""))), "an entry before any section")),
            };
            let rest = line.strip_prefix("  ").filter(|r| !r.starts_with(' ')).ok_or_else(|| refuse(n, &format!("section `{sec}`"), "an entry is indented by exactly two spaces"))?;
            let colon = rest.find(':').ok_or_else(|| refuse(n, &format!("section `{sec}`"), "expected `key: value`"))?;
            let key = &rest[..colon];
            let after = &rest[colon + 1..];
            let raw = if after.is_empty() {
                ""
            } else {
                after.strip_prefix(' ').filter(|v| !v.starts_with(' ')).ok_or_else(|| refuse(n, &format!("section `{sec}` key `{}`", shown(key)), "expected one space after the colon"))?
            };
            let at = format!("section `{sec}` key `{}`", shown(key));
            let (val, quoted) = plain_value(raw).map_err(|why| refuse(n, &at, why))?;
            if sec == "repos" {
                let id = RepoId::parse(key).map_err(|_| refuse(n, &format!("section `repos` key `repo_id`"), "must be a plain repo id"))?;
                let numeric = id.as_str().parse::<f64>().is_ok() && id.as_str().starts_with(|c: char| c.is_ascii_digit() || c == '.');
                if YAML_WORDS.contains(&id.as_str()) || numeric {
                    return Err(refuse(n, &format!("section `repos` key `repo_id`"), "a YAML special word or number cannot be a repo id"));
                }
                let at = format!("section `repos` repo `{}`", id.as_str());
                if !folder_ok(val) {
                    return Err(refuse(n, &at, "folder must be absolute, with no `..` component, no control character and no trailing `/`"));
                }
                if repos.iter().any(|(r, _)| *r == id) {
                    return Err(refuse(n, &at, "duplicate repo id"));
                }
                if repos.iter().any(|(_, f)| f == val) {
                    return Err(refuse(n, &at, "two repo ids on one folder"));
                }
                repos.push((id, val.to_string()));
            } else if sec == "friction" {
                let at = format!("section `{sec}` key `{}`", shown(key));
                if key != "inbox" {
                    return Err(refuse(n, &at, "unknown key"));
                }
                if inbox.is_some() {
                    return Err(refuse(n, &at, "duplicate key"));
                }
                if !folder_ok(val) {
                    return Err(refuse(n, &at, "the inbox path must be absolute, with no `..` component, no control character and no trailing `/`"));
                }
                inbox = Some(val.to_string());
            } else {
                let full = format!("{sec}.{key}");
                let slot = KEYS.iter().position(|k| *k == full).ok_or_else(|| refuse(n, &at, "unknown key"))?;
                if nums[slot].is_some() {
                    return Err(refuse(n, &at, "duplicate key"));
                }
                if quoted {
                    return Err(refuse(n, &at, "a number must not be quoted"));
                }
                nums[slot] = Some(number(val).ok_or_else(|| refuse(n, &at, "must be a positive whole number (digits only, no leading zero, at most 15 digits)"))?);
            }
        }
        // v3: a missing `friction` section is named by its key (a v2 file is refused naming `friction.inbox`), but an
        // earlier missing section is named first, in file order, as before.
        if let Some(i) = seen.iter().position(|s| !s) {
            if i == 4 {
                return Err(format!("config: missing key `{INBOX_KEY}`"));
            }
            return Err(format!("config: missing section `{}`", SECTIONS[i]));
        }
        let mut out = [0u64; 15];
        for (i, slot) in nums.iter().enumerate() {
            out[i] = slot.ok_or_else(|| format!("config: missing key `{}`", KEYS[i]))?;
        }
        Ok(Config { repos, nums: out, inbox: inbox.ok_or_else(|| format!("config: missing key `{INBOX_KEY}`"))? })
    }

    /// Read at most the cap plus one byte, then parse. No search, no default location: the caller names the file. Errors never carry the path.
    pub fn load(path: &Path) -> Result<Config, String> {
        let meta = std::fs::metadata(path).map_err(|_| "config: file missing or unreadable".to_string())?;
        if !meta.is_file() {
            return Err("config: not a file".to_string());
        }
        let f = std::fs::File::open(path).map_err(|_| "config: file unreadable".to_string())?;
        let mut bytes = Vec::new();
        f.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| "config: file unreadable".to_string())?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err("config: file too large".to_string());
        }
        let text = String::from_utf8(bytes).map_err(|_| "config: file is not utf-8".to_string())?;
        Config::parse(&text)
    }

    /// The folder this repo id maps to, from the config only; `None` when the id is not mapped.
    pub fn repo_dir(&self, id: &RepoId) -> Option<&str> {
        self.repos.iter().find(|(r, _)| r == id).map(|(_, f)| f.as_str())
    }

    /// How many repos are mapped.
    pub fn repo_count(&self) -> usize {
        self.repos.len()
    }

    /// The limits of one ticket size; `None` unless `size` is exactly `S`, `M` or `L`.
    pub fn budget(&self, size: &str) -> Option<Budget> {
        let i = match size {
            "S" => 0,
            "M" => 2,
            "L" => 4,
            _ => return None,
        };
        Some(Budget { wall_seconds: self.nums[i], tokens: self.nums[i + 1] })
    }

    /// The configured lane count (`lanes.count`), the upper bound of a legal lane number.
    pub fn lane_count(&self) -> u64 {
        self.nums[14]
    }

    /// The configured friction inbox path (`friction.inbox`, v3). Read by `harness friction`; never printed.
    pub fn friction_inbox(&self) -> &str {
        &self.inbox
    }

    pub fn shadow(&self) -> Shadow {
        let n = &self.nums;
        Shadow {
            p1a_cap: n[6],
            p4a_cap: n[7],
            d2a_cap: n[8],
            p7_cap: n[9],
            p1a_token_budget: n[10],
            p4a_token_budget: n[11],
            d2a_token_budget: n[12],
            p7_token_budget: n[13],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const SECRET: &str = "/synthetic-folder-0xA1";

    fn text(repos: &str) -> String {
        format!(
            "repos:\n{repos}\nbudgets:\n  s_wall_seconds: 111\n  s_tokens: 222\n  m_wall_seconds: 333\n  m_tokens: 444\n  l_wall_seconds: 555\n  l_tokens: 666\n\nshadow:\n  p1a_cap: 5\n  p4a_cap: 10\n  d2a_cap: 20\n  p7_cap: 10\n  p1a_token_budget: 7\n  p4a_token_budget: 8\n  d2a_token_budget: 9\n  p7_token_budget: 11\n\nlanes:\n  count: 4\n\nfriction:\n  inbox: /synthetic-inbox.jsonl\n"
        )
    }

    fn valid() -> String {
        text("  a: /x\n  b: /y")
    }

    fn refused(t: &str) -> String {
        Config::parse(t).expect_err(&format!("must be refused: {t:?}"))
    }

    /// True when the message carries the secret.
    fn leaks(msg: &str, secret: &str) -> bool {
        msg.contains(secret)
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("harness-config-test-{}-{tag}", std::process::id()));
        assert!(d.starts_with(std::env::temp_dir()), "scratch lives under the system temp folder");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_valid_config_loads_and_the_valid_control_exits_clean() {
        let c = Config::parse(&valid()).unwrap();
        assert_eq!(c.repo_count(), 2);
        let s = c.shadow();
        assert_eq!((s.p1a_cap, s.p4a_cap, s.d2a_cap, s.p7_cap), (5, 10, 20, 10));
        assert_eq!((s.p1a_token_budget, s.p4a_token_budget, s.d2a_token_budget, s.p7_token_budget), (7, 8, 9, 11));
    }

    #[test]
    fn keys_are_distinct_and_every_one_is_required() {
        for (i, k) in KEYS.iter().enumerate() {
            assert!(!KEYS[..i].contains(k), "{k} listed twice");
            let (sec, key) = k.split_once('.').unwrap();
            assert!(sec == "budgets" || sec == "shadow" || sec == "lanes", "{k}");
            let line = format!("  {key}: ");
            let mut dropped = valid();
            let start = dropped.find(&line).unwrap();
            let end = start + dropped[start..].find('\n').unwrap() + 1;
            dropped.replace_range(start..end, "");
            let e = refused(&dropped);
            assert!(e.contains(&format!("missing key `{k}`")), "{k}: {e}");
        }
    }

    #[test]
    fn an_empty_repos_section_is_an_empty_map() {
        let c = Config::parse(&text("")).unwrap();
        assert_eq!(c.repo_count(), 0);
        assert_eq!(c.repo_dir(&RepoId::parse("a").unwrap()), None);
    }

    #[test]
    fn a_missing_section_is_refused_by_name() {
        for sec in ["repos", "budgets", "shadow"] {
            let t = valid();
            let cut = t.replacen(&format!("{sec}:\n"), "zzz:\n", 1);
            assert!(refused(&cut).contains("unknown section"), "{sec}");
        }
        let no_repos = valid().replacen("repos:\n  a: /x\n  b: /y\n", "", 1);
        assert!(refused(&no_repos).contains("missing section `repos`"));
    }

    #[test]
    fn duplicate_sections_and_keys_are_refused() {
        let e = refused(&format!("{}\nrepos:\n", valid()));
        assert!(e.contains("duplicate section"), "{e}");
        let e = refused(&valid().replacen("  s_tokens: 222\n", "  s_tokens: 222\n  s_tokens: 222\n", 1));
        assert!(e.contains("duplicate key") && e.contains("s_tokens"), "{e}");
    }

    #[test]
    fn an_unknown_key_is_refused_and_a_known_key_in_the_wrong_section_is_unknown() {
        let e = refused(&valid().replacen("  s_tokens: 222\n", "  s_tokens: 222\n  extra: 1\n", 1));
        assert!(e.contains("unknown key") && e.contains("`extra`") && e.contains("`budgets`"), "{e}");
        let e = refused(&valid().replacen("shadow:\n", "shadow:\n  s_tokens: 3\n", 1));
        assert!(e.contains("unknown key") && e.contains("`s_tokens`") && e.contains("`shadow`"), "{e}");
        // a folder-shaped key is not echoed
        let e = refused(&valid().replacen("  s_tokens: 222\n", &format!("  s_tokens: 222\n  {SECRET}: 1\n"), 1));
        assert!(e.contains("unknown key") && !leaks(&e, SECRET), "{e}");
    }

    #[test]
    fn an_unknown_section_names_the_section() {
        let e = refused(&valid().replacen("budgets:\n", "zzz:\n", 1));
        assert!(e.contains("unknown section") && e.contains("`zzz`") && e.contains("line"), "{e}");
        let e = refused(&valid().replacen("budgets:\n", &format!("{SECRET}:\n"), 1));
        assert!(!leaks(&e, SECRET), "{e}");
    }

    #[test]
    fn a_tab_and_an_entry_before_any_section_name_where_they_are() {
        let e = refused(&valid().replacen("  s_tokens", "\ts_tokens", 1));
        assert!(e.contains("tab") && e.contains("section `budgets`"), "{e}");
        let e = refused(&valid().replacen("  p7_cap: 10", "  p7_cap:\t10", 1));
        assert!(e.contains("section `shadow`"), "{e}");
        let e = refused(&format!("  extra: 1\n{}", valid()));
        assert!(e.contains("before any section") && e.contains("`extra`") && e.contains("line 1"), "{e}");
        let e = refused(&format!("  {SECRET}: 1\n{}", valid()));
        assert!(e.contains("before any section") && !leaks(&e, SECRET), "{e}");
    }

    #[test]
    fn number_like_repo_ids_and_a_unicode_line_break_in_a_folder_are_refused() {
        for id in ["123", "1.5", "1e3", ".5", ".inf", ".nan", "y", "n"] {
            let e = refused(&text(&format!("  {id}: /x")));
            assert!(e.contains("`repo_id`"), "{id}: {e}");
        }
        Config::parse(&text("  v1: /x\n  a.b: /y\n  e3: /z")).unwrap();
        for c in ['\u{2028}', '\u{2029}'] {
            let e = refused(&text(&format!("  a: /x{c}y")));
            assert!(e.contains("repo `a`"), "{e}");
        }
    }

    #[test]
    fn tab_carriage_return_and_space_only_lines_are_refused() {
        assert!(refused(&valid().replacen("  s_tokens", "\ts_tokens", 1)).contains("tab"));
        assert!(refused(&valid().replacen("budgets:\n", "budgets:\r\n", 1)).contains("carriage return"));
        assert!(refused(&valid().replacen("\n\nshadow", "\n   \nshadow", 1)).contains("only spaces"));
        assert!(refused(&valid().replacen("  s_tokens: 222", "   s_tokens: 222", 1)).contains("exactly two spaces"));
        assert!(refused(&valid().replacen("  s_tokens: 222", "s_tokens: 222", 1)).contains("section"));
    }

    #[test]
    fn flow_anchor_and_document_forms_are_refused() {
        let bad = ["{a: 1}", "[1]", "&x 1", "*x", "---", "| x", "> x", "!tag 1", "- 1", "'1'", "#c"];
        for v in bad {
            let e = refused(&valid().replacen("  s_tokens: 222", &format!("  s_tokens: {v}"), 1));
            assert!(e.contains("s_tokens"), "{v}: {e}");
        }
        for v in ["{a: /x}", "[/x]", "&x /y", "*x", "| /y", "> /y", "'/y'"] {
            refused(&text(&format!("  a: {v}")));
        }
        refused(&format!("---\n{}", valid()));
        refused(&valid().replacen("repos:", "repos: {}", 1));
        refused(&valid().replacen("repos:", "repos: ", 1));
    }

    #[test]
    fn plain_value_grammar_rows_are_each_refused_and_the_twin_loads() {
        for v in ["/x #y", "/x ", "/x: y", "/x\"y", "/x:", "\"/x\\y\"", "\"/x #y\"", "\"/x \"", "\"/x", "/x\"", "\"", " /x"] {
            let e = refused(&text(&format!("  a: {v}")));
            assert!(e.contains("line") && !e.contains("/x"), "{v}: {e}");
        }
        Config::parse(&text("  a: /x")).unwrap();
        Config::parse(&text("  a: \"/x\"")).unwrap();
        // a `#` or `:` without the space is plain text
        assert_eq!(Config::parse(&text("  a: /x#y")).unwrap().repo_dir(&RepoId::parse("a").unwrap()), Some("/x#y"));
        assert_eq!(Config::parse(&text("  a: /x:y")).unwrap().repo_dir(&RepoId::parse("a").unwrap()), Some("/x:y"));
        // the quotes are not part of the folder
        assert_eq!(Config::parse(&text("  a: \"/x\"")).unwrap().repo_dir(&RepoId::parse("a").unwrap()), Some("/x"));
        // no colon-space glue and no extra spaces after the colon
        refused(&text("  a:/x"));
        refused(&text("  a:  /x"));
    }

    #[test]
    fn numbers_must_be_positive_whole_numbers() {
        for bad in ["0", "-1", "+1", "1.5", "007", "1234567890123456", "abc", "", "1e3", "0x1", "1_0"] {
            let e = refused(&valid().replacen("  s_tokens: 222", &format!("  s_tokens: {bad}"), 1));
            assert!(e.contains("s_tokens"), "{bad:?}: {e}");
            let e = refused(&valid().replacen("  p7_cap: 10", &format!("  p7_cap: {bad}"), 1));
            assert!(e.contains("p7_cap"), "{bad:?}: {e}");
        }
        for good in ["1", "9", "10", "123456789012345"] {
            Config::parse(&valid().replacen("  s_tokens: 222", &format!("  s_tokens: {good}"), 1)).unwrap();
        }
    }

    #[test]
    fn lanes_count_is_a_required_positive_whole_number() {
        // the section is required: dropping it names the section
        let e = refused(&valid().replace("\nlanes:\n  count: 4\n", "\n"));
        assert!(e.contains("missing section `lanes`"), "{e}");
        // the key is required: an empty lanes section names the full key
        let e = refused(&valid().replace("lanes:\n  count: 4", "lanes:"));
        assert!(e.contains("missing key `lanes.count`"), "{e}");
        // 0, a leading zero, a non-number, a negative, a fraction and an over-cap value are refused naming the section and the key
        for bad in ["0", "04", "four", "-1", "1.5", "1234567890123456"] {
            let e = refused(&valid().replace("count: 4", &format!("count: {bad}")));
            assert!(e.contains("`lanes`") && e.contains("`count`") && e.contains("positive whole number"), "{bad}: {e}");
        }
        // a quoted count is refused as a type error, naming the section and the key
        let e = refused(&valid().replace("count: 4", "count: \"4\""));
        assert!(e.contains("`lanes`") && e.contains("`count`") && e.contains("quoted"), "{e}");
        // a second count line under lanes is refused as a duplicate key, naming the section and the key
        let e = refused(&valid().replace("  count: 4", "  count: 4\n  count: 4"));
        assert!(e.contains("`lanes`") && e.contains("`count`") && e.contains("duplicate key"), "{e}");
        // the twin loads and the count is read from the text, not compiled in
        let c = Config::parse(&valid().replace("count: 4", "count: 9")).unwrap();
        assert_eq!(c.lane_count(), 9);
        assert_eq!(Config::parse(&valid()).unwrap().lane_count(), 4);
        // lanes holds exactly one key
        let e = refused(&valid().replace("  count: 4", "  count: 4\n  extra: 1"));
        assert!(e.contains("unknown key"), "{e}");
    }

    #[test]
    fn a_double_quoted_number_is_refused_in_budgets_and_shadow() {
        let e = refused(&valid().replacen("  s_tokens: 222", "  s_tokens: \"5\"", 1));
        assert!(e.contains("s_tokens") && e.contains("quoted"), "{e}");
        let e = refused(&valid().replacen("  p7_cap: 10", "  p7_cap: \"5\"", 1));
        assert!(e.contains("p7_cap") && e.contains("quoted"), "{e}");
        Config::parse(&valid().replacen("  s_tokens: 222", "  s_tokens: 5", 1)).unwrap();
    }

    #[test]
    fn v3_friction_inbox_is_required_shape_checked_and_read() {
        // the value is read back, not compiled in
        assert_eq!(Config::parse(&valid()).unwrap().friction_inbox(), "/synthetic-inbox.jsonl");
        assert_eq!(
            Config::parse(&valid().replace("/synthetic-inbox.jsonl", "/other-inbox.jsonl")).unwrap().friction_inbox(),
            "/other-inbox.jsonl"
        );
        // a v2 file (no friction section) is refused naming the full key, and so is an empty section
        for dropped in ["\nfriction:\n  inbox: /synthetic-inbox.jsonl\n", "friction:\n  inbox: /synthetic-inbox.jsonl"] {
            let e = refused(&valid().replace(dropped, dropped.lines().next().unwrap()));
            assert!(e.contains("missing key `friction.inbox`"), "{e}");
        }
        // the same shape rules as a repo folder; a refusal never carries the value
        for bad in ["relative/inbox.jsonl", "/x/../y", "/x/"] {
            let e = refused(&valid().replace("/synthetic-inbox.jsonl", bad));
            assert!(e.contains("`friction`") && e.contains("`inbox`") && !e.contains("synthetic-inbox"), "{bad}: {e}");
        }
        // only `inbox` lives under friction
        let e = refused(&valid().replace("  inbox: /synthetic-inbox.jsonl", "  inbox: /synthetic-inbox.jsonl\n  extra: 1"));
        assert!(e.contains("unknown key"), "{e}");
    }


    #[test]
    fn a_bad_repo_id_is_refused_naming_repo_id() {
        for bad in ["Repo", "../x", "a/b", "", "a b", "a\\b"] {
            let e = refused(&text(&format!("  {bad}: /x")));
            assert!(e.contains("`repo_id`") && e.contains("line 2"), "{bad:?}: {e}");
        }
        Config::parse(&text("  scratch.1: /x")).unwrap();
    }

    #[test]
    fn repo_ids_that_are_yaml_special_words_are_refused() {
        for w in ["true", "false", "null", "yes", "no", "on", "off"] {
            let e = refused(&text(&format!("  {w}: /x")));
            assert!(e.contains("`repo_id`") && e.contains("special word"), "{w}: {e}");
        }
        Config::parse(&text("  nope: /x\n  on-call: /y")).unwrap();
    }

    #[test]
    fn repo_folder_rules_each_refuse_and_the_twin_loads() {
        for (what, v) in [("relative", "x/y"), ("tilde", "~/x"), ("dotdot", "/x/../y"), ("dotdot-end", "/x/.."), ("trailing slash", "/x/"), ("root", "/"), ("control", "/x\u{1}y"), ("del", "/x\u{7f}y"), ("empty", "")] {
            let e = refused(&text(&format!("  a: {v}")));
            assert!(e.contains("repo `a`") || what == "empty", "{what}: {e}");
            assert!(e.contains("line 2") && !e.contains(v) || v.is_empty() || v == "/", "{what}: {e}");
        }
        Config::parse(&text("  a: /x\n  b: /y")).unwrap();
        // `..` inside a name is not a component
        Config::parse(&text("  a: /x..y")).unwrap();
    }

    #[test]
    fn a_duplicate_id_and_a_duplicate_folder_are_refused_naming_the_id() {
        let e = refused(&text("  a: /x\n  a: /y"));
        assert!(e.contains("duplicate repo id") && e.contains("`a`") && e.contains("line 3"), "{e}");
        let e = refused(&text("  a: /x\n  b: /x"));
        assert!(e.contains("one folder") && e.contains("`b`") && !e.contains("/x"), "{e}");
        refused(&text("  a: /x\n  b: \"/x\""));
    }

    #[test]
    fn no_refusal_carries_a_value() {
        // each bad text plants SECRET where a value sits
        let bads = [
            text(&format!("  a: {SECRET}/..")),
            text(&format!("  a: {SECRET} #y")),
            text(&format!("  a: {SECRET} ")),
            text(&format!("  a: {SECRET}: y")),
            text(&format!("  a: {SECRET}\"y")),
            text(&format!("  a: {SECRET}/")),
            text(&format!("  a: {SECRET}\n  a: {SECRET}2")),
            text(&format!("  a: {SECRET}\n  b: {SECRET}")),
            text(&format!("  a: {}", SECRET.trim_start_matches('/'))),
            text(&format!("  a: ~{SECRET}")),
            text(&format!("  a: {SECRET}\u{1}")),
            text(&format!("  {SECRET}: /x")),
            text(&format!("  true: {SECRET}")),
            valid().replacen("  s_tokens: 222", &format!("  s_tokens: {SECRET}"), 1),
            valid().replacen("  s_tokens: 222", &format!("  s_tokens: \"{SECRET}\""), 1),
            valid().replacen("  s_tokens: 222", &format!("  {SECRET}: 1"), 1),
            valid().replacen("  s_tokens: 222", &format!("  s_tokens: 222\n  {SECRET}: 1"), 1),
            valid().replacen("budgets:", &format!("{SECRET}:"), 1),
            valid().replacen("budgets:", &format!("budgets: {SECRET}"), 1),
            valid().replacen("  s_tokens: 222", &format!("\t{SECRET}"), 1),
            valid().replacen("  s_tokens: 222", &format!("  s_tokens:{SECRET}"), 1),
            valid().replacen("  s_tokens: 222", &format!("  s_tokens: {{{SECRET}}}"), 1),
        ];
        for t in &bads {
            let e = refused(t);
            assert!(!leaks(&e, SECRET) && !leaks(&e, SECRET.trim_start_matches('/')), "a refusal carried the value: {e}");
        }
        // the assertion helper catches a leaking message (so the loop above is not vacuous)
        assert!(leaks(&format!("line 3: {SECRET}"), SECRET));
        assert!(!leaks("line 3: section `repos` repo `a`: duplicate repo id", SECRET));
        // the valid control loads
        Config::parse(&valid()).unwrap();
    }

    #[test]
    fn repo_dir_gives_each_id_its_own_folder_from_the_text_only() {
        let c = Config::parse(&text("  a: /alpha\n  b: /beta")).unwrap();
        assert_eq!(c.repo_dir(&RepoId::parse("a").unwrap()), Some("/alpha"));
        assert_eq!(c.repo_dir(&RepoId::parse("b").unwrap()), Some("/beta"));
        assert_eq!(c.repo_dir(&RepoId::parse("c").unwrap()), None, "an id not in the map is not mapped");
        let swapped = Config::parse(&text("  b: /alpha\n  a: /beta")).unwrap();
        assert_eq!(swapped.repo_dir(&RepoId::parse("a").unwrap()), Some("/beta"), "not the first entry, not a constant");
    }

    #[test]
    fn budget_reads_the_text_and_a_re_parse_of_edited_text_gives_the_new_pair() {
        let t = valid();
        let c = Config::parse(&t).unwrap();
        assert_eq!(c.budget("S"), Some(Budget { wall_seconds: 111, tokens: 222 }));
        assert_eq!(c.budget("M"), Some(Budget { wall_seconds: 333, tokens: 444 }));
        assert_eq!(c.budget("L"), Some(Budget { wall_seconds: 555, tokens: 666 }));
        let edited = t.replacen("s_wall_seconds: 111", "s_wall_seconds: 777", 1).replacen("s_tokens: 222", "s_tokens: 888", 1);
        let c2 = Config::parse(&edited).unwrap();
        assert_eq!(c2.budget("S"), Some(Budget { wall_seconds: 777, tokens: 888 }), "not compiled in");
        assert_eq!(c2.budget("M"), c.budget("M"));
        for bad in ["XL", "s", "", "SM", " S", "S "] {
            assert_eq!(c.budget(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn load_refuses_bad_files_and_accepts_the_exact_cap() {
        let d = scratch("load");
        let ok = d.join("ok.yaml");
        std::fs::write(&ok, valid()).unwrap();
        assert_eq!(Config::load(&ok).unwrap().repo_count(), 2);
        // missing, a directory
        assert!(Config::load(&d.join("nope.yaml")).is_err());
        assert!(Config::load(&d).is_err());
        // zero bytes
        let z = d.join("zero.yaml");
        std::fs::write(&z, b"").unwrap();
        assert!(Config::load(&z).unwrap_err().contains("empty"));
        // not utf-8
        let bad = d.join("bin.yaml");
        std::fs::write(&bad, [0xff, 0xfe, 0x00]).unwrap();
        assert!(Config::load(&bad).unwrap_err().contains("utf-8"));
        // exactly the cap loads; one byte more is refused
        let base = valid();
        let pad = MAX_BYTES as usize - base.len();
        let exact = format!("{base}#{}\n", "x".repeat(pad - 2));
        assert_eq!(exact.len() as u64, MAX_BYTES);
        let f = d.join("exact.yaml");
        std::fs::write(&f, &exact).unwrap();
        Config::load(&f).unwrap();
        let over = d.join("over.yaml");
        std::fs::write(&over, format!("{base}#{}\n", "x".repeat(pad - 1))).unwrap();
        assert!(Config::load(&over).unwrap_err().contains("too large"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn load_errors_never_carry_the_path() {
        let d = scratch("path-leak");
        let secret = ["synthetic", "config", "path"].join("-") + "-marker";
        let secret = secret.as_str();
        let p = d.join(secret);
        for e in [Config::load(&p).unwrap_err(), Config::load(&d).unwrap_err()] {
            assert!(!e.contains(secret) && !e.contains(d.to_str().unwrap()), "{e}");
        }
        std::fs::write(&p, "repos:\n  a: /x/\n").unwrap();
        assert!(!Config::load(&p).unwrap_err().contains(secret));
        let _ = std::fs::remove_dir_all(&d);
    }
}
