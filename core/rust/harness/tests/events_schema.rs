//! `schemas/event.schema.json` mirrors the event loader's exact key set per kind; this pins the two together (same idea as the
//! gate and ticket schema tests). The crate never interprets the schema file.
use harness::events::{Kind, LINE_CAP, SCHEMA_VERSION};
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn required_in(section: &str) -> Result<Vec<String>, String> {
    let at = section.find("\"required\"").ok_or("no required array")?;
    let open = at + section[at..].find('[').ok_or("required is not an array")?;
    let close = open + section[open..].find(']').ok_or("required array is not closed")?;
    Ok(section[open + 1..close].split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect())
}

/// Err names the first way the schema text disagrees with the loader.
fn check(schema: &str) -> Result<(), String> {
    let marker = "\"title\": \"";
    let n = schema.matches(marker).count();
    if n != Kind::ALL.len() + 1 {
        return Err(format!("expected {} titles (the schema and one per kind), found {n}", Kind::ALL.len() + 1));
    }
    if !schema.contains(&format!("{LINE_CAP} bytes")) {
        return Err("the schema does not state the line cap".to_string());
    }
    for k in Kind::ALL {
        let name = k.as_str();
        let start = schema.find(&format!("{marker}{name}\"")).ok_or(format!("no branch for kind {name}"))?;
        let end = schema[start + 1..].find(marker).map(|i| start + 1 + i).unwrap_or(schema.len());
        let section = &schema[start..end];
        let mut got = required_in(section).map_err(|e| format!("{name}: {e}"))?;
        let mut want: Vec<String> = k.keys().iter().map(|s| s.to_string()).collect();
        got.sort();
        want.sort();
        if got != want {
            return Err(format!("{name}: required set {got:?} differs from the loader's {want:?}"));
        }
        for key in &want {
            if !section.contains(&format!("\"{key}\":")) {
                return Err(format!("{name}: required key {key} has no property"));
            }
        }
        if !section.contains(&format!("\"kind\": {{ \"const\": \"{name}\" }}")) {
            return Err(format!("{name}: the kind const does not match the branch"));
        }
        if !section.contains(&format!("\"v\": {{ \"const\": {SCHEMA_VERSION},")) {
            return Err(format!("{name}: the v const is not {SCHEMA_VERSION}"));
        }
        if !section.contains("\"additionalProperties\": false") {
            return Err(format!("{name}: extra keys are not refused"));
        }
    }
    Ok(())
}

fn schema_text() -> String {
    std::fs::read_to_string(repo().join("schemas/event.schema.json")).expect("the schema is committed and utf-8")
}

#[test]
fn the_committed_schema_matches_the_event_loader() {
    check(&schema_text()).unwrap();
}

#[test]
fn planted_a_schema_that_drifted_from_the_loader_is_caught() {
    let good = schema_text();
    let first_required = "\"required\": [\"v\", \"kind\", \"ts_ms\", \"ticket\", \"repo\", \"reason\"]";
    assert!(good.contains(first_required));
    let mutants = [
        ("a key removed from one kind", good.replacen(first_required, "\"required\": [\"v\", \"kind\", \"ts_ms\", \"ticket\", \"repo\"]", 1)),
        ("an extra required key", good.replacen(first_required, "\"required\": [\"v\", \"kind\", \"ts_ms\", \"ticket\", \"repo\", \"reason\", \"extra\"]", 1)),
        ("a wrong v const", good.replacen("\"v\": { \"const\": 1,", "\"v\": { \"const\": 2,", 1)),
        ("a kind const that no longer matches", good.replacen("\"kind\": { \"const\": \"escalation\" }", "\"kind\": { \"const\": \"escalate\" }", 1)),
        ("extra keys allowed", good.replacen("\"additionalProperties\": false", "\"additionalProperties\": true", 1)),
        ("a kind branch missing", good.replacen("\"title\": \"budget_exceeded\"", "\"title\": \"budget\"", 1)),
        ("a stale line cap", good.replace("4096 bytes", "8192 bytes")),
    ];
    for (name, m) in mutants {
        assert!(m != good, "{name}: the mutation changed nothing");
        assert!(check(&m).is_err(), "{name} must be caught");
    }
}

#[test]
fn the_doc_names_every_kind_and_the_cap() {
    let doc = std::fs::read_to_string(repo().join("docs/event-log.md")).expect("the doc is committed and utf-8");
    for k in Kind::ALL {
        assert!(doc.contains(&format!("`{}`", k.as_str())), "doc names {}", k.as_str());
    }
    assert!(doc.contains(&LINE_CAP.to_string()));
    assert!(doc.contains("torn tail") && doc.contains("EVENT NOT RECORDED"));
}
