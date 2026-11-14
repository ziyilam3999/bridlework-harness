//! Input-to-argv binding for the MCP tools: one table entry per tools.json row, plus the refusal checks.
//! Every refusal happens here, before any child is spawned.

use crate::json::Value;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bind {
    Pos,
    Flag(&'static str),
    Repeat(&'static str),
    Stdin,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Plain,
    Path,
    RepoMap,
}

type Binding = (&'static str, Bind, Kind);

/// One entry per tools.json row, in list order. Each entry's binding order is also the argv order.
pub const BINDINGS: &[(&str, &[Binding])] = &[
    ("harness_version", &[]),
    (
        "harness_status",
        &[
            ("ticket_file", Bind::Pos, Kind::Path),
            ("repo_map", Bind::Repeat("--repo-map"), Kind::RepoMap),
            ("result_dir", Bind::Flag("--result-dir"), Kind::Path),
            ("caps_dir", Bind::Flag("--caps-dir"), Kind::Path),
        ],
    ),
    (
        "harness_ticket_intake",
        &[
            ("ticket_file", Bind::Pos, Kind::Path),
            ("repo_map", Bind::Repeat("--repo-map"), Kind::RepoMap),
        ],
    ),
    ("harness_events_list", &[("log", Bind::Flag("--log"), Kind::Path)]),
    (
        "harness_evidence_show",
        &[
            ("ticket", Bind::Pos, Kind::Plain),
            ("manifest_dir", Bind::Flag("--manifest-dir"), Kind::Path),
        ],
    ),
    (
        "harness_privacy_scan_text",
        &[
            ("text", Bind::Stdin, Kind::Plain),
            ("surface", Bind::Flag("--surface"), Kind::Plain),
        ],
    ),
    (
        "harness_config_check",
        &[
            ("config", Bind::Flag("--config"), Kind::Path),
            ("repo", Bind::Flag("--repo"), Kind::Plain),
            ("budget", Bind::Flag("--budget"), Kind::Plain),
        ],
    ),
];

pub const TEXT_MAX: usize = 1_048_576;

const KNOWN_PATTERN: &str = "^[a-z_.]+$";

/// Only the evidence and privacy tools read a privacy list.
pub fn needs_privacy_list(name: &str) -> bool {
    name == "harness_evidence_show" || name == "harness_privacy_scan_text"
}

fn bad(key: &str) -> String {
    format!("{key} has a bad value")
}

/// Walk every string a value holds (a string, or the items of an array of strings).
fn strings(v: &Value) -> Vec<&str> {
    match v {
        Value::Str(t) => vec![t.as_str()],
        Value::Arr(items) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn check_value(key: &str, schema: &Value, v: &Value) -> Result<(), String> {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => {
            if !matches!(v, Value::Str(_)) {
                return Err(bad(key));
            }
        }
        Some("array") => {
            let items = v.as_array().ok_or_else(|| bad(key))?;
            if items.iter().any(|i| i.as_str().is_none()) {
                return Err(bad(key));
            }
            let min = match schema.get("minItems") {
                Some(Value::Num(n)) => n.parse::<usize>().map_err(|_| bad(key))?,
                _ => 0,
            };
            if items.len() < min {
                return Err(bad(key));
            }
        }
        _ => return Err(bad(key)),
    }
    if let Some(Value::Arr(allowed)) = schema.get("enum") {
        if !allowed.iter().any(|a| a == v) {
            return Err(bad(key));
        }
    }
    if let Some(p) = schema.get("pattern").and_then(Value::as_str) {
        let t = v.as_str().ok_or_else(|| bad(key))?;
        if p != KNOWN_PATTERN
            || t.is_empty()
            || !t.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.')
        {
            return Err(bad(key));
        }
    }
    Ok(())
}

/// Build the child argv and the optional stdin text from a tool row and the call's `arguments`.
pub fn build(row: &Value, args: Option<&Value>) -> Result<(Vec<String>, Option<String>), String> {
    let empty = Value::Obj(Vec::new());
    let args = args.unwrap_or(&empty);
    let pairs = match args {
        Value::Obj(p) => p,
        _ => return Err("arguments must be an object".to_string()),
    };
    let name = row.get("name").and_then(Value::as_str).unwrap_or("");
    let table = BINDINGS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, b)| *b)
        .ok_or_else(|| format!("no binding for {name}"))?;
    let schema = row.get("inputSchema").ok_or_else(|| "no input schema".to_string())?;
    let props = schema.get("properties").ok_or_else(|| "no input schema".to_string())?;
    for (k, _) in pairs {
        if props.get(k).is_none() {
            return Err(format!("unknown argument {k}"));
        }
    }
    for req in schema.get("required").and_then(Value::as_array).unwrap_or(&[]) {
        if let Some(k) = req.as_str() {
            if args.get(k).is_none() {
                return Err(format!("missing {k}"));
            }
        }
    }
    for (k, v) in pairs {
        let sub = props.get(k).ok_or_else(|| bad(k))?;
        check_value(k, sub, v)?;
    }
    for (k, v) in pairs {
        if strings(v).iter().any(|t| t.contains('\0')) {
            return Err(format!("{k} holds a NUL byte"));
        }
    }
    for (prop, bind, _) in table {
        if *bind == Bind::Stdin {
            continue;
        }
        if let Some(v) = args.get(prop) {
            if strings(v).iter().any(|t| t.starts_with('-')) {
                return Err(format!("{prop} starts with '-'"));
            }
        }
    }
    for (prop, _, kind) in table {
        let Some(v) = args.get(prop) else { continue };
        for t in strings(v) {
            let ok = match kind {
                Kind::Plain => true,
                Kind::Path => t.starts_with('/'),
                Kind::RepoMap => t.split_once('=').is_some_and(|(_, dir)| dir.starts_with('/')),
            };
            if !ok {
                return Err(format!("{prop} must be an absolute path"));
            }
        }
    }
    let mut argv: Vec<String> = row
        .get("argv")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let mut stdin = None;
    for (prop, bind, _) in table {
        let Some(v) = args.get(prop) else { continue };
        match bind {
            Bind::Stdin => {
                let t = v.as_str().unwrap_or("");
                if t.len() > TEXT_MAX {
                    return Err(format!("{prop} is over {TEXT_MAX} bytes"));
                }
                stdin = Some(t.to_string());
            }
            Bind::Pos => argv.extend(strings(v).iter().map(|t| t.to_string())),
            Bind::Flag(f) => {
                argv.push((*f).to_string());
                argv.extend(strings(v).iter().map(|t| t.to_string()));
            }
            Bind::Repeat(f) => {
                for t in strings(v) {
                    argv.push((*f).to_string());
                    argv.push(t.to_string());
                }
            }
        }
    }
    Ok((argv, stdin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn rows() -> Vec<Value> {
        let v = json::parse(include_str!("../../tools.json")).unwrap();
        v.get("tools").and_then(Value::as_array).unwrap().to_vec()
    }

    fn row(name: &str) -> Value {
        rows().into_iter().find(|r| r.get("name").and_then(Value::as_str) == Some(name)).unwrap()
    }

    fn run(name: &str, args: &str) -> Result<(Vec<String>, Option<String>), String> {
        let a = json::parse(args).unwrap();
        build(&row(name), Some(&a))
    }

    fn refusal(name: &str, args: &str) -> String {
        run(name, args).unwrap_err()
    }

    #[test]
    fn bindings_match_tools_json() {
        let rows = rows();
        let names: Vec<&str> = BINDINGS.iter().map(|(n, _)| *n).collect();
        let want: Vec<&str> = rows.iter().filter_map(|r| r.get("name").and_then(Value::as_str)).collect();
        assert_eq!(names, want);
        for ((_, table), r) in BINDINGS.iter().zip(rows.iter()) {
            let mut have: Vec<&str> = table.iter().map(|(p, _, _)| *p).collect();
            have.sort_unstable();
            let mut props: Vec<&str> = match r.get("inputSchema").and_then(|s| s.get("properties")) {
                Some(Value::Obj(p)) => p.iter().map(|(k, _)| k.as_str()).collect(),
                _ => Vec::new(),
            };
            props.sort_unstable();
            assert_eq!(have, props);
        }
        let stdin: Vec<&str> = BINDINGS
            .iter()
            .flat_map(|(n, t)| t.iter().filter(|(_, b, _)| *b == Bind::Stdin).map(move |(p, _, _)| (*n, *p)))
            .map(|(n, p)| if n == "harness_privacy_scan_text" && p == "text" { "ok" } else { "other" })
            .collect();
        assert_eq!(stdin, vec!["ok"]);
    }

    #[test]
    fn argv_order() {
        let (argv, stdin) = run(
            "harness_status",
            r#"{"caps_dir":"/c","result_dir":"/r","repo_map":["a=/x","b=/y"],"ticket_file":"/t"}"#,
        )
        .unwrap();
        let want = "status /t --repo-map a=/x --repo-map b=/y --result-dir /r --caps-dir /c";
        assert_eq!(argv.join(" "), want);
        assert_eq!(stdin, None);
    }

    #[test]
    fn only_known_pattern() {
        for r in rows() {
            if let Some(Value::Obj(props)) = r.get("inputSchema").and_then(|s| s.get("properties")) {
                for (_, p) in props {
                    if let Some(pat) = p.get("pattern").and_then(Value::as_str) {
                        assert_eq!(pat, KNOWN_PATTERN);
                    }
                }
            }
        }
    }

    #[test]
    fn refuses_non_object() {
        let a = json::parse("[]").unwrap();
        assert_eq!(build(&row("harness_version"), Some(&a)).unwrap_err(), "arguments must be an object");
        assert!(build(&row("harness_version"), None).is_ok());
    }

    #[test]
    fn refuses_unknown_argument() {
        assert_eq!(refusal("harness_config_check", r#"{"zz":"a"}"#), "unknown argument zz");
    }

    #[test]
    fn refuses_missing_required() {
        assert_eq!(refusal("harness_events_list", "{}"), "missing log");
    }

    #[test]
    fn refuses_bad_values() {
        assert_eq!(refusal("harness_config_check", r#"{"budget":"X"}"#), "budget has a bad value");
        assert_eq!(refusal("harness_config_check", r#"{"budget":1}"#), "budget has a bad value");
        assert_eq!(
            refusal("harness_privacy_scan_text", r#"{"text":"x","surface":"A"}"#),
            "surface has a bad value"
        );
        assert_eq!(
            refusal("harness_privacy_scan_text", r#"{"text":"x","surface":""}"#),
            "surface has a bad value"
        );
        assert_eq!(
            refusal("harness_ticket_intake", r#"{"ticket_file":"/t","repo_map":[]}"#),
            "repo_map has a bad value"
        );
        assert_eq!(
            refusal("harness_ticket_intake", r#"{"ticket_file":"/t","repo_map":[1]}"#),
            "repo_map has a bad value"
        );
    }

    #[test]
    fn refuses_nul_byte() {
        assert_eq!(refusal("harness_events_list", r#"{"log":"/a\u0000b"}"#), "log holds a NUL byte");
    }

    #[test]
    fn refuses_dash() {
        assert_eq!(refusal("harness_events_list", r#"{"log":"--log"}"#), "log starts with '-'");
        assert_eq!(
            refusal("harness_ticket_intake", r#"{"ticket_file":"--testmode","repo_map":["a=/x"]}"#),
            "ticket_file starts with '-'"
        );
        assert_eq!(
            refusal("harness_ticket_intake", r#"{"ticket_file":"/t","repo_map":["-x=/a"]}"#),
            "repo_map starts with '-'"
        );
        assert_eq!(
            refusal("harness_evidence_show", r#"{"ticket":"-x","manifest_dir":"/m"}"#),
            "ticket starts with '-'"
        );
    }

    #[test]
    fn refuses_relative_paths() {
        assert_eq!(refusal("harness_events_list", r#"{"log":"e.jsonl"}"#), "log must be an absolute path");
        assert_eq!(
            refusal("harness_ticket_intake", r#"{"ticket_file":"/t","repo_map":["a=rel"]}"#),
            "repo_map must be an absolute path"
        );
        assert_eq!(
            refusal("harness_ticket_intake", r#"{"ticket_file":"/t","repo_map":["norel"]}"#),
            "repo_map must be an absolute path"
        );
    }

    #[test]
    fn refuses_long_text() {
        let big = "a".repeat(TEXT_MAX + 1);
        let a = format!(r#"{{"text":"{big}"}}"#);
        assert_eq!(refusal("harness_privacy_scan_text", &a), "text is over 1048576 bytes");
        let ok = "a".repeat(TEXT_MAX);
        assert!(run("harness_privacy_scan_text", &format!(r#"{{"text":"{ok}"}}"#)).is_ok());
    }

    #[test]
    fn dash_text_allowed() {
        let (argv, stdin) = run("harness_privacy_scan_text", r#"{"text":"-x"}"#).unwrap();
        assert_eq!(argv.join(" "), "privacy scan --text");
        assert_eq!(stdin.as_deref(), Some("-x"));
    }

    #[test]
    fn privacy_list_scope() {
        assert!(needs_privacy_list("harness_evidence_show"));
        assert!(needs_privacy_list("harness_privacy_scan_text"));
        assert!(!needs_privacy_list("harness_config_check"));
        assert!(!needs_privacy_list("harness_status"));
    }
}
