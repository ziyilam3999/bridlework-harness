//! The two Claude plugin packages (std only). `plugins/claude/hooks/hooks.json` and `plugins/claude/.claude-plugin/plugin.json` are
//! generator output for the seat plugin, and `plugins/claude/orchestrator/hooks/hooks.json` and
//! `plugins/claude/orchestrator/.claude-plugin/plugin.json` for the orchestrator plugin: this module prints them from the control
//! registry (a hook row's profile picks the folder), and `check` refuses a committed file that differs from the generator.
//! Plugin files are read as data only; nothing under `plugins/` is a crate dependency.

use crate::control::{self, Registry};
use std::io::Read;
use std::path::Path;

pub const PLUGIN_NAME: &str = "bridlework-harness";
pub const ORCH_PLUGIN_NAME: &str = "bridlework-harness-orchestrator";
pub const DESCRIPTION: &str = "Fail-closed Claude Code hooks for the bridlework harness; every hook calls the harness CLI.";
pub const ORCH_DESCRIPTION: &str = "Fail-closed Claude Code hooks for the bridlework Orchestrator session; every hook calls the harness CLI.";
/// The host timeout the generator writes into each orchestrator hook entry (seconds). A host timeout lets the tool call through, so
/// this is only a far backstop: an orchestrator hook script is to stop and exit 2 before it is up.
pub const ORCH_HOOK_TIMEOUT_S: u64 = 120;
pub const LICENSE: &str = "Apache-2.0";
const PLUGIN_ROOT: &str = "plugins/claude";
const ORCH_ROOT: &str = "plugins/claude/orchestrator";
const HOOKS_REL: &str = "plugins/claude/hooks/hooks.json";
const PLUGIN_REL: &str = "plugins/claude/.claude-plugin/plugin.json";
const ORCH_HOOKS_REL: &str = "plugins/claude/orchestrator/hooks/hooks.json";
const ORCH_PLUGIN_REL: &str = "plugins/claude/orchestrator/.claude-plugin/plugin.json";
const MAX_BYTES: u64 = 1024 * 1024;

/// The hook-event to CLI-verb table: (control id, harness verb words, literal argv prefix the script must hold).
pub const CLI_VERBS: [(&str, &str, &str); 4] = [
    ("path-restriction-hook", "harness ticket intake", "[bin_path, \"ticket\", \"intake\""),
    ("gh-text-hook", "harness privacy scan --text", "[bin_path, \"privacy\", \"scan\", \"--text\""),
    ("pre-completion-hook", "harness status", "[bin_path, \"status\""),
    ("session-start-hook", "harness session record-start", "[bin_path, \"session\", \"record-start\""),
];

type Table<'a> = &'a [(&'a str, &'a str, &'a str)];

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn read_small(path: &Path, what: &str) -> Result<String, String> {
    let f = std::fs::File::open(path).map_err(|_| format!("{}: missing or unreadable", what))?;
    let mut bytes = Vec::new();
    f.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|_| format!("{}: unreadable", what))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(format!("{}: too large", what));
    }
    String::from_utf8(bytes).map_err(|_| format!("{}: not utf-8", what))
}

pub fn hooks_json(repo_root: &Path, reg: &Registry) -> Result<String, String> {
    hooks_json_with(repo_root, reg, &CLI_VERBS)
}

pub fn hooks_json_with(repo_root: &Path, reg: &Registry, table: Table) -> Result<String, String> {
    gen_hooks(repo_root, reg, table, control::SEAT)
}

fn orch_hooks_json_with(repo_root: &Path, reg: &Registry, table: Table) -> Result<String, String> {
    gen_hooks(repo_root, reg, table, control::ORCHESTRATOR)
}

/// The hooks.json of one profile. The table cross-check runs over all enabled rows of both profiles; the entries then hold
/// only the rows of this profile, in EVENTS order then registry order. An orchestrator entry carries the host timeout; an empty
/// orchestrator output is the four-line form with an empty hooks object (the seat formatter keeps its own shape).
fn gen_hooks(repo_root: &Path, reg: &Registry, table: Table, profile: &str) -> Result<String, String> {
    let enabled: Vec<_> = reg.rows.iter().filter(|r| r.enabled && r.kind == "hook" && r.hook.is_some()).collect();
    for r in &enabled {
        if !table.iter().any(|t| t.0 == r.id) {
            return Err(format!("{}: enabled hook row has no CLI verb entry", r.id));
        }
    }
    for t in table {
        if !enabled.iter().any(|r| r.id == t.0) {
            return Err(format!("{}: CLI verb entry has no enabled hook row", t.0));
        }
    }
    let orch = profile == control::ORCHESTRATOR;
    let root = if orch { ORCH_ROOT } else { PLUGIN_ROOT };
    let mut events: Vec<(&str, Vec<String>)> = Vec::new();
    for ev in control::EVENTS {
        let mut entries: Vec<String> = Vec::new();
        for r in enabled.iter().filter(|r| r.hook.as_ref().map(|h| h.event == ev && h.profile == profile).unwrap_or(false)) {
            let h = r.hook.as_ref().unwrap();
            let name = h.script.strip_prefix(&format!("{}/", root)).filter(|n| !n.is_empty() && !n.contains('/') && *n != "." && *n != "..");
            let name = name.ok_or_else(|| format!("{}: script `{}` is not directly under {}/", r.id, h.script, root))?;
            let (_, verb, needle) = table.iter().find(|t| t.0 == r.id).unwrap();
            let text = read_small(&repo_root.join(&h.script), &h.script)?;
            if !text.contains("HARNESS_BIN") || !text.contains(needle) {
                return Err(format!("{}: does not call the harness CLI ({})", h.script, verb));
            }
            let command = format!("bash \"${{CLAUDE_PLUGIN_ROOT}}/{}\"", name);
            let inner = if orch {
                format!("\"command\": {},\n            \"timeout\": {}", esc(&command), ORCH_HOOK_TIMEOUT_S)
            } else {
                format!("\"command\": {}", esc(&command))
            };
            entries.push(format!(
                "      {{\n        \"matcher\": {},\n        \"hooks\": [\n          {{\n            \"type\": \"command\",\n            {}\n          }}\n        ]\n      }}",
                esc(&h.tools.join("|")),
                inner
            ));
        }
        if !entries.is_empty() {
            events.push((ev, entries));
        }
    }
    if orch && events.is_empty() {
        return Ok(format!("{{\n  \"description\": {},\n  \"hooks\": {{}}\n}}\n", esc(ORCH_DESCRIPTION)));
    }
    let body: Vec<String> = events.iter().map(|(ev, es)| format!("    {}: [\n{}\n    ]", esc(ev), es.join(",\n"))).collect();
    let desc = if orch { ORCH_DESCRIPTION } else { DESCRIPTION };
    Ok(format!("{{\n  \"description\": {},\n  \"hooks\": {{\n{}\n  }}\n}}\n", esc(desc), body.join(",\n")))
}

pub fn plugin_json() -> String {
    format!(
        "{{\n  \"name\": {},\n  \"version\": {},\n  \"description\": {},\n  \"license\": {}\n}}\n",
        esc(PLUGIN_NAME),
        esc(env!("CARGO_PKG_VERSION")),
        esc(DESCRIPTION),
        esc(LICENSE)
    )
}

/// The orchestrator plugin.json: its own name, the same version and license.
pub fn orchestrator_plugin_json() -> String {
    format!(
        "{{\n  \"name\": {},\n  \"version\": {},\n  \"description\": {},\n  \"license\": {}\n}}\n",
        esc(ORCH_PLUGIN_NAME),
        esc(env!("CARGO_PKG_VERSION")),
        esc(ORCH_DESCRIPTION),
        esc(LICENSE)
    )
}

fn first_diff(want: &str, got: &str) -> usize {
    let (w, g): (Vec<&str>, Vec<&str>) = (want.lines().collect(), got.lines().collect());
    (0..w.len().max(g.len())).find(|i| w.get(*i) != g.get(*i)).map(|i| i + 1).unwrap_or(w.len() + 1)
}

fn same(root: &Path, rel: &str, want: &str) -> Result<(), String> {
    let got = read_small(&root.join(rel), rel)?;
    if got != want {
        return Err(format!("{}: differs from the generator at line {}", rel, first_diff(want, &got)));
    }
    Ok(())
}

/// Like `same`, but the file must also be a regular file: a symbolic link is refused even when its bytes match.
fn same_regular(root: &Path, rel: &str, want: &str) -> Result<(), String> {
    match std::fs::symlink_metadata(root.join(rel)) {
        Ok(m) if !m.is_file() => return Err(format!("{}: not a regular file", rel)),
        _ => {}
    }
    same(root, rel, want)
}

pub fn check(repo_root: &Path) -> Result<(), String> {
    let reg = Registry::load(repo_root)?;
    control::check_hooks(repo_root, &reg)?;
    same(repo_root, HOOKS_REL, &hooks_json(repo_root, &reg)?)?;
    same(repo_root, PLUGIN_REL, &plugin_json())?;
    same_regular(repo_root, ORCH_HOOKS_REL, &orch_hooks_json_with(repo_root, &reg, &CLI_VERBS)?)?;
    same_regular(repo_root, ORCH_PLUGIN_REL, &orchestrator_plugin_json())
}

const PLUGIN_USAGE: &str =
    "usage: harness plugin hooks-json --repo <root> [--profile seat|orchestrator] | plugin-json [--profile seat|orchestrator] | check --repo <root>";

/// The flags after a plugin subcommand: `--repo` and `--profile`, each at most once, each with a value. A flag the subcommand does
/// not take, a missing value, a repeat, or a profile outside the loader's list is a usage error.
fn take_plugin_flags(rest: &[String], repo_ok: bool, profile_ok: bool) -> Result<(Option<String>, Option<String>), String> {
    let mut repo: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut it = rest.iter();
    while let Some(f) = it.next() {
        if f == "--repo" && repo_ok {
            let v = it.next().ok_or_else(|| PLUGIN_USAGE.to_string())?;
            if repo.replace(v.clone()).is_some() {
                return Err(PLUGIN_USAGE.to_string());
            }
        } else if f == "--profile" && profile_ok {
            let v = it.next().ok_or_else(|| PLUGIN_USAGE.to_string())?;
            if profile.replace(v.clone()).is_some() || !control::PROFILES.contains(&v.as_str()) {
                return Err(PLUGIN_USAGE.to_string());
            }
        } else {
            return Err(PLUGIN_USAGE.to_string());
        }
    }
    Ok((repo, profile))
}

pub fn plugin_cmd(args: &[String]) -> i32 {
    let sub = args.first().map(|s| s.as_str());
    let out: Result<Option<String>, String> = match sub {
        Some("plugin-json") => match take_plugin_flags(&args[1..], false, true) {
            Ok((_, profile)) => Ok(Some(if profile.as_deref() == Some(control::ORCHESTRATOR) { orchestrator_plugin_json() } else { plugin_json() })),
            Err(e) => Err(e),
        },
        Some("hooks-json") => match take_plugin_flags(&args[1..], true, true) {
            Ok((Some(r), profile)) => {
                let r = std::path::PathBuf::from(r);
                Registry::load(&r)
                    .and_then(|reg| {
                        if profile.as_deref() == Some(control::ORCHESTRATOR) {
                            orch_hooks_json_with(&r, &reg, &CLI_VERBS)
                        } else {
                            hooks_json(&r, &reg)
                        }
                    })
                    .map(Some)
            }
            Ok((None, _)) => Err(PLUGIN_USAGE.to_string()),
            Err(e) => Err(e),
        },
        Some("check") => match take_plugin_flags(&args[1..], true, false) {
            Ok((Some(r), _)) => {
                let r = std::path::PathBuf::from(r);
                match check(&r) {
                    Ok(()) => Ok(None),
                    Err(e) => {
                        eprintln!("harness: plugin: {}", e);
                        return 1;
                    }
                }
            }
            _ => Err(PLUGIN_USAGE.to_string()),
        },
        _ => Err(PLUGIN_USAGE.to_string()),
    };
    match out {
        Ok(Some(s)) => {
            print!("{}", s);
            0
        }
        Ok(None) => 0,
        Err(e) => {
            eprintln!("harness: plugin: {}", e.trim_start_matches('E'));
            2
        }
    }
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
            let p = std::env::temp_dir().join(format!("hg-plg-{}-{}-{}", name, std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
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

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.path().is_dir() {
                copy_dir(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }

    fn copy_repo(name: &str) -> Scratch {
        let s = Scratch::new(name);
        for d in ["controls", "plugins/claude"] {
            copy_dir(&repo().join(d), &s.0.join(d));
        }
        s
    }

    fn edit(root: &Path, rel: &str, from: &str, to: &str) {
        let p = root.join(rel);
        let t = std::fs::read_to_string(&p).unwrap();
        assert!(t.contains(from), "{} holds {}", rel, from);
        std::fs::write(&p, t.replacen(from, to, 1)).unwrap();
    }

    fn load(root: &Path) -> Registry {
        Registry::load(root).unwrap()
    }

    #[test]
    fn the_committed_plugin_matches_the_registry() {
        let root = repo();
        check(&root).unwrap();
        let text = std::fs::read_to_string(root.join(HOOKS_REL)).unwrap();
        assert_eq!(text.matches("\"command\":").count(), CLI_VERBS.len());
        for r in load(&root).rows.iter().filter(|r| r.enabled) {
            let m = format!("\"matcher\": \"{}\"", r.hook.as_ref().unwrap().tools.join("|"));
            assert!(text.contains(&m), "hooks.json holds {}", m);
        }
    }

    #[test]
    fn generated_hooks_equal_enabled_registry_rows() {
        let s = copy_repo("rows");
        let root = &s.0;
        let reg = load(root);
        let base = hooks_json(root, &reg).unwrap();
        assert_eq!(base.matches("\"command\":").count(), CLI_VERBS.len());
        // one more enabled row gives one more entry
        std::fs::write(root.join("plugins/claude/extra-hook.sh"), "HARNESS_BIN\n[bin_path, \"extra\"\n").unwrap();
        let reg_text = std::fs::read_to_string(root.join("controls/registry.json")).unwrap();
        let row = r#"{"id":"extra-hook","kind":"hook","class":"policy","enabled":true,"failure":"f","test":"plugins/claude/t.sh","retirement_test":"r","owner":"execution_verification","hook":{"script":"plugins/claude/extra-hook.sh","manifest":"plugins/claude/manifest.json","event":"PreToolUse","tools":["Read"]}}"#;
        let reg4 = Registry::parse(&reg_text.replacen("\"controls\": [", &format!("\"controls\": [{},", row), 1)).unwrap();
        let mut table: Vec<(&str, &str, &str)> = CLI_VERBS.to_vec();
        table.push(("extra-hook", "harness extra", "[bin_path, \"extra\""));
        let four = hooks_json_with(root, &reg4, &table).unwrap();
        assert_eq!(four.matches("\"command\":").count(), CLI_VERBS.len() + 1);
        std::fs::remove_file(root.join("plugins/claude/extra-hook.sh")).unwrap();
        // one row disabled drops exactly that entry
        let off = Registry::parse(&reg_text.replacen("\"enabled\": true", "\"enabled\": false", 1)).unwrap();
        let t2: Vec<_> = CLI_VERBS[1..].to_vec();
        let two = hooks_json_with(root, &off, &t2).unwrap();
        assert_eq!(two.matches("\"command\":").count(), CLI_VERBS.len() - 1);
        assert!(!two.contains("path-restriction-hook.sh"));
        // a committed file with one entry deleted, or one extra, is an Err naming the file and a line
        let hp = root.join(HOOKS_REL);
        let lines: Vec<&str> = base.lines().collect();
        let cut = lines.iter().position(|l| l.contains("\"Bash\"")).unwrap();
        let mut v = lines.clone();
        v.drain(cut - 1..cut + 8);
        std::fs::write(&hp, v.join("\n") + "\n").unwrap();
        let e = check(root).unwrap_err();
        assert!(e.contains("hooks.json") && e.contains("line"), "{}", e);
        std::fs::write(&hp, base.replacen("\"Bash\"", "\"Bash|Read\"", 1)).unwrap();
        assert!(check(root).unwrap_err().contains("hooks.json"));
        std::fs::write(&hp, &base).unwrap();
        check(root).unwrap();
        let mut extra = base.clone();
        extra = extra.replacen("\"PreToolUse\": [", "\"PreToolUse\": [\n      {\"matcher\": \"Read\", \"hooks\": []},", 1);
        std::fs::write(&hp, extra).unwrap();
        assert!(check(root).unwrap_err().contains("hooks.json"));
    }

    #[test]
    fn a_hook_that_does_not_call_the_cli_fails() {
        // (i) needle removed
        let s = copy_repo("noneedle");
        edit(&s.0, "plugins/claude/gh-text-hook.sh", "[bin_path, \"privacy\", \"scan\", \"--text\"", "[other, \"privacy\"");
        let e = hooks_json(&s.0, &load(&s.0)).unwrap_err();
        assert!(e.contains("gh-text-hook.sh") && e.contains("does not call the harness CLI"), "{}", e);
        // (ii) a command replaced by `true`
        let s = copy_repo("true");
        edit(&s.0, HOOKS_REL, "bash \\\"${CLAUDE_PLUGIN_ROOT}/gh-text-hook.sh\\\"", "true");
        assert!(check(&s.0).unwrap_err().contains("hooks.json"));
        // (iii) script outside the plugin root, or in a subfolder
        for bad in ["plugins/other/path-restriction-hook.sh", "plugins/claude/sub/path-restriction-hook.sh"] {
            let s = copy_repo("outside");
            let mut reg = load(&s.0);
            reg.rows[0].hook.as_mut().unwrap().script = bad.to_string();
            std::fs::create_dir_all(s.0.join(bad).parent().unwrap()).unwrap();
            std::fs::copy(s.0.join("plugins/claude/path-restriction-hook.sh"), s.0.join(bad)).unwrap();
            let e = hooks_json(&s.0, &reg).unwrap_err();
            assert!(e.contains("path-restriction-hook"), "{}", e);
        }
        // (iv) a row with no table entry, and an entry with no row
        let s = copy_repo("table");
        let reg = load(&s.0);
        let e = hooks_json_with(&s.0, &reg, &CLI_VERBS[1..]).unwrap_err();
        assert!(e.contains("path-restriction-hook"), "{}", e);
        let mut t = CLI_VERBS.to_vec();
        t.push(("ghost-hook", "harness ghost", "x"));
        let e = hooks_json_with(&s.0, &reg, &t).unwrap_err();
        assert!(e.contains("ghost-hook"), "{}", e);
    }

    #[test]
    fn the_cli_verb_table_matches_the_committed_registry() {
        let root = repo();
        let reg = load(&root);
        let mut ids: Vec<&str> = reg.rows.iter().filter(|r| r.enabled && r.hook.is_some()).map(|r| r.id.as_str()).collect();
        let mut tids: Vec<&str> = CLI_VERBS.iter().map(|t| t.0).collect();
        ids.sort();
        tids.sort();
        assert_eq!(ids, tids);
        for r in &reg.rows {
            let t = CLI_VERBS.iter().find(|t| t.0 == r.id).unwrap();
            let text = std::fs::read_to_string(root.join(&r.hook.as_ref().unwrap().script)).unwrap();
            assert!(text.contains(t.2) && text.contains("HARNESS_BIN"), "{} holds its needle", r.id);
        }
    }

    #[test]
    fn plugin_json_carries_name_version_license() {
        let t = plugin_json();
        let want = format!("\"name\": \"{}\"", PLUGIN_NAME);
        assert!(t.contains(&want));
        assert!(t.contains(&format!("\"version\": \"{}\"", env!("CARGO_PKG_VERSION"))));
        assert!(t.contains("\"license\": \"Apache-2.0\""));
        let keys: Vec<&str> = t.lines().filter_map(|l| l.trim().strip_prefix('"')).filter_map(|l| l.split('"').next()).collect();
        assert_eq!(keys, ["name", "version", "description", "license"]);
        assert_eq!(esc("a\"b\\c\n\u{1}"), "\"a\\\"b\\\\c\\n\\u0001\"");
    }

    // ---- the orchestrator plugin ----

    /// The value under `key` in a parsed object.
    fn get<'a>(v: &'a control::V, k: &str) -> &'a control::V {
        match v {
            control::V::O(kv) => &kv.iter().find(|(x, _)| x.as_str() == k).unwrap().1,
            _ => panic!("`{}` is not inside an object", k),
        }
    }

    fn first_of(v: &control::V) -> &control::V {
        match v {
            control::V::A(a) => &a[0],
            _ => panic!("expected an array"),
        }
    }

    /// The four-line orchestrator hooks.json with no entry.
    fn empty_orch_hooks() -> String {
        format!("{{\n  \"description\": {},\n  \"hooks\": {{}}\n}}\n", esc(ORCH_DESCRIPTION))
    }

    /// Write an orchestrator row's script and manifest into the scratch tree and return the registry with the row plus its table entry.
    fn with_orch_row(root: &Path, tools: &str, enabled: bool) -> (Registry, Vec<(&'static str, &'static str, &'static str)>) {
        std::fs::write(root.join("plugins/claude/orchestrator/orch-hook.sh"), "HARNESS_BIN\n[bin_path, \"orch\"\n").unwrap();
        std::fs::write(root.join("plugins/claude/orchestrator/orch-manifest.json"), format!("{{\"event\":\"PreToolUse\",\"tools\":{}}}", tools)).unwrap();
        let text = std::fs::read_to_string(root.join("controls/registry.json")).unwrap();
        let row = format!(
            r#"{{"id":"orch-hook","kind":"hook","class":"policy","enabled":{},"failure":"f","test":"plugins/claude/text-rule-smoke.sh","retirement_test":"r","owner":"execution_verification","hook":{{"script":"plugins/claude/orchestrator/orch-hook.sh","manifest":"plugins/claude/orchestrator/orch-manifest.json","event":"PreToolUse","tools":{},"profile":"orchestrator"}}}}"#,
            enabled, tools
        );
        let reg = Registry::parse(&text.replacen("\"controls\": [", &format!("\"controls\": [{},", row), 1)).unwrap();
        let mut table: Vec<(&str, &str, &str)> = CLI_VERBS.to_vec();
        if enabled {
            table.push(("orch-hook", "harness orch", "[bin_path, \"orch\""));
        }
        (reg, table)
    }

    #[test]
    fn seat_hooks_are_unchanged_by_an_orchestrator_row() {
        let s = copy_repo("orchseat");
        let root = &s.0;
        let committed = std::fs::read_to_string(root.join(HOOKS_REL)).unwrap();
        let (reg, table) = with_orch_row(root, r#"["AskUserQuestion"]"#, true);
        let seat = hooks_json_with(root, &reg, &table).unwrap();
        assert_eq!(seat, committed, "the seat output is byte-identical to the committed seat file");
        assert!(!seat.contains("orch-hook"), "{}", seat);
        let orch = orch_hooks_json_with(root, &reg, &table).unwrap();
        assert_eq!(orch.matches("\"command\":").count(), 1, "{}", orch);
        assert!(orch.contains("bash \\\"${CLAUDE_PLUGIN_ROOT}/orch-hook.sh\\\""), "{}", orch);
    }

    #[test]
    fn orchestrator_entries_carry_the_host_timeout() {
        let s = copy_repo("orchtimeout");
        let root = &s.0;
        let (reg, table) = with_orch_row(root, r#"["AskUserQuestion"]"#, true);
        let orch = orch_hooks_json_with(root, &reg, &table).unwrap();
        assert_eq!(ORCH_HOOK_TIMEOUT_S, 120);
        assert_eq!(orch.matches("\"timeout\": 120").count(), 1, "{}", orch);
        let doc = control::parse_doc(&orch).unwrap();
        let hook = first_of(get(first_of(get(get(&doc, "hooks"), "PreToolUse")), "hooks"));
        assert!(matches!(get(hook, "timeout"), control::V::N(n) if *n == 120), "{}", orch);
        let seat = hooks_json_with(root, &reg, &table).unwrap();
        assert!(!seat.contains("timeout"), "{}", seat);
    }

    #[test]
    fn orchestrator_matcher_is_the_row_tools() {
        let s = copy_repo("orchmatcher");
        let root = &s.0;
        let (reg, table) = with_orch_row(root, r#"["AskUserQuestion","Bash"]"#, true);
        let orch = orch_hooks_json_with(root, &reg, &table).unwrap();
        assert!(orch.contains("\"matcher\": \"AskUserQuestion|Bash\""), "{}", orch);
    }

    #[test]
    fn a_disabled_orchestrator_row_writes_no_entry() {
        let s = copy_repo("orchoff");
        let root = &s.0;
        let (reg, table) = with_orch_row(root, r#"["AskUserQuestion"]"#, false);
        let orch = orch_hooks_json_with(root, &reg, &table).unwrap();
        assert_eq!(orch.matches("\"command\":").count(), 0, "{}", orch);
        assert_eq!(orch, empty_orch_hooks());
    }

    #[test]
    fn an_orchestrator_hook_that_does_not_call_the_cli_fails() {
        // the needle removed
        let s = copy_repo("orchnocli");
        let root = &s.0;
        let (reg, table) = with_orch_row(root, r#"["AskUserQuestion"]"#, true);
        std::fs::write(root.join("plugins/claude/orchestrator/orch-hook.sh"), "HARNESS_BIN\n[other, \"x\"\n").unwrap();
        let e = orch_hooks_json_with(root, &reg, &table).unwrap_err();
        assert!(e.contains("orch-hook.sh") && e.contains("does not call the harness CLI"), "{}", e);
        // an enabled orchestrator row with no table entry
        let e = orch_hooks_json_with(root, &reg, &CLI_VERBS).unwrap_err();
        assert!(e.contains("orch-hook"), "{}", e);
        // a table entry with no enabled row
        let mut t = CLI_VERBS.to_vec();
        t.push(("ghost-hook", "harness ghost", "x"));
        let e = orch_hooks_json_with(root, &load(root), &t).unwrap_err();
        assert!(e.contains("ghost-hook"), "{}", e);
    }

    #[test]
    fn orchestrator_hooks_with_no_row_have_an_empty_hooks_object() {
        let root = repo();
        let orch = orch_hooks_json_with(&root, &load(&root), &CLI_VERBS).unwrap();
        let literal = "{\n  \"description\": \"Fail-closed Claude Code hooks for the bridlework Orchestrator session; every hook calls the harness CLI.\",\n  \"hooks\": {}\n}\n";
        assert_eq!(orch, literal);
        assert_eq!(orch.lines().count(), 4);
        assert_eq!(orch, empty_orch_hooks());
        let doc = control::parse_doc(&orch).unwrap();
        assert!(matches!(get(&doc, "hooks"), control::V::O(kv) if kv.is_empty()), "{}", orch);
    }

    #[test]
    fn orchestrator_plugin_json_has_its_own_name() {
        let t = orchestrator_plugin_json();
        assert_ne!(PLUGIN_NAME, ORCH_PLUGIN_NAME);
        assert!(!t.contains(&format!("\"name\": \"{}\"", PLUGIN_NAME)), "{}", t);
        assert_eq!(
            t,
            format!(
                "{{\n  \"name\": \"{}\",\n  \"version\": \"{}\",\n  \"description\": {},\n  \"license\": \"{}\"\n}}\n",
                ORCH_PLUGIN_NAME,
                env!("CARGO_PKG_VERSION"),
                esc(ORCH_DESCRIPTION),
                LICENSE
            )
        );
    }

    #[test]
    fn a_stale_orchestrator_file_fails_the_check() {
        // hooks.json deleted
        let s = copy_repo("orchstale");
        std::fs::remove_file(s.0.join(ORCH_HOOKS_REL)).unwrap();
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_HOOKS_REL) && e.contains("missing or unreadable"), "{}", e);
        // one line edited
        let s = copy_repo("orchstale");
        edit(&s.0, ORCH_HOOKS_REL, "Orchestrator session", "other session");
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_HOOKS_REL) && e.contains("differs"), "{}", e);
        // one extra entry
        let s = copy_repo("orchstale");
        let entry = "\"hooks\": {\n    \"PreToolUse\": [\n      {\n        \"matcher\": \"Bash\",\n        \"hooks\": [\n          {\n            \"type\": \"command\",\n            \"command\": \"bash \\\"${CLAUDE_PLUGIN_ROOT}/x-hook.sh\\\"\",\n            \"timeout\": 120\n          }\n        ]\n      }\n    ]\n  }";
        edit(&s.0, ORCH_HOOKS_REL, "\"hooks\": {}", entry);
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_HOOKS_REL) && e.contains("differs"), "{}", e);
        // emptied
        let s = copy_repo("orchstale");
        std::fs::write(s.0.join(ORCH_HOOKS_REL), "").unwrap();
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_HOOKS_REL) && e.contains("differs"), "{}", e);
        // bytes that are not UTF-8
        let s = copy_repo("orchstale");
        std::fs::write(s.0.join(ORCH_HOOKS_REL), b"\xff\xfe").unwrap();
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_HOOKS_REL) && e.contains("not utf-8"), "{}", e);
        // grown over 1 MiB
        let s = copy_repo("orchstale");
        std::fs::write(s.0.join(ORCH_HOOKS_REL), vec![b'x'; MAX_BYTES as usize + 8]).unwrap();
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_HOOKS_REL) && e.contains("too large"), "{}", e);
        // a symbolic link to a byte-identical copy
        let s = copy_repo("orchstale");
        let p = s.0.join(ORCH_HOOKS_REL);
        let copy = s.0.join("hooks-copy.json");
        std::fs::copy(&p, &copy).unwrap();
        std::fs::remove_file(&p).unwrap();
        std::os::unix::fs::symlink(&copy, &p).unwrap();
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_HOOKS_REL) && e.contains("not a regular file"), "{}", e);
        // plugin.json deleted
        let s = copy_repo("orchstale");
        std::fs::remove_file(s.0.join(ORCH_PLUGIN_REL)).unwrap();
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_PLUGIN_REL) && e.contains("missing or unreadable"), "{}", e);
        // the version edited
        let s = copy_repo("orchstale");
        edit(&s.0, ORCH_PLUGIN_REL, &format!("\"version\": \"{}\"", env!("CARGO_PKG_VERSION")), "\"version\": \"9.9.9\"");
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_PLUGIN_REL) && e.contains("differs"), "{}", e);
        // a symbolic link to a byte-identical copy
        let s = copy_repo("orchstale");
        let p = s.0.join(ORCH_PLUGIN_REL);
        let copy = s.0.join("plugin-copy.json");
        std::fs::copy(&p, &copy).unwrap();
        std::fs::remove_file(&p).unwrap();
        std::os::unix::fs::symlink(&copy, &p).unwrap();
        let e = check(&s.0).unwrap_err();
        assert!(e.contains(ORCH_PLUGIN_REL) && e.contains("not a regular file"), "{}", e);
    }

    #[test]
    fn the_committed_orchestrator_plugin_matches_the_registry() {
        let root = repo();
        check(&root).unwrap();
        let reg = load(&root);
        assert_eq!(std::fs::read_to_string(root.join(ORCH_HOOKS_REL)).unwrap(), orch_hooks_json_with(&root, &reg, &CLI_VERBS).unwrap());
        assert_eq!(std::fs::read_to_string(root.join(ORCH_PLUGIN_REL)).unwrap(), orchestrator_plugin_json());
    }
}
