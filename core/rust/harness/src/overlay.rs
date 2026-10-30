//! The deny list ("overlay"): parsing and the one resolution rule for both surfaces.
use std::env;
use std::fs;

pub struct Entry {
    pub tok: String,
    pub regulated: bool,
}

pub struct List {
    pub entries: Vec<Entry>,
}

const TAG: &str = "regulated:";

/// Parse list text. Blank lines and `#` lines are skipped; an optional tag prefix marks a regulated line.
pub fn parse(text: &str) -> Result<List, String> {
    let mut entries = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (body, regulated) = match line.strip_prefix(TAG) {
            Some(rest) => (rest.trim(), true),
            None => (line, false),
        };
        if body.is_empty() {
            continue;
        }
        entries.push(Entry { tok: body.to_lowercase(), regulated });
    }
    if entries.is_empty() {
        return Err("list has no entries".to_string());
    }
    Ok(List { entries })
}

/// Resolve the list: the secret variable wins when set (even empty); otherwise the path variable.
pub fn load() -> Result<List, String> {
    let (bytes, text) = if let Some(v) = env::var_os("HARNESS_PRIVACY_LIST_SECRET") {
        let s = v.into_string().map_err(|_| "list is not valid text".to_string())?;
        (s.clone().into_bytes(), s)
    } else if let Some(p) = env::var_os("HARNESS_PRIVACY_LIST") {
        let b = fs::read(&p).map_err(|_| "list file unreadable".to_string())?;
        let s = String::from_utf8(b.clone()).map_err(|_| "list is not valid text".to_string())?;
        (b, s)
    } else {
        return Err("no list configured".to_string());
    };
    let list = parse(&text)?;
    if let Some(old) = env::var_os("HARNESS_PRIVACY_OLD_LIST") {
        let ob = fs::read(&old).map_err(|_| "old list unreadable".to_string())?;
        if ob != bytes {
            return Err("list drift".to_string());
        }
    }
    Ok(list)
}
