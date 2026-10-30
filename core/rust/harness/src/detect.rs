//! Per-line class detection. Patterns are logic or split literals, so this file never matches itself.
use crate::decode;
use crate::overlay::List;

fn j(parts: &[&str]) -> String {
    parts.concat()
}

fn push(out: &mut Vec<&'static str>, c: &'static str) {
    if !out.contains(&c) {
        out.push(c);
    }
}

/// Every class id found in one line, over all decoded readings.
pub fn classes_for_line(line: &str, list: &List) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for r in decode::readings(line) {
        check(&r, list, &mut out);
    }
    out
}

fn check(r: &str, list: &List, out: &mut Vec<&'static str>) {
    let low = r.to_lowercase();
    for e in &list.entries {
        if low.contains(e.tok.as_str()) {
            push(out, if e.regulated { "regulated-id" } else { "deny-token" });
        }
    }
    if has_email(r) {
        push(out, "email");
    }
    if home_path(r) {
        push(out, "home-path");
    }
    if credential(r, &low) {
        push(out, "credential");
    }
    if session_link(&low) {
        push(out, "session-link");
    }
    if low.trim_start().starts_with(j(&["co-auth", "ored-by:"]).as_str()) && has_email(r) {
        push(out, "coauthor-personal");
    }
    if baseline(&low) {
        push(out, "baseline-number");
    }
    if low.contains(j(&["synthetic", "-test", "-marker"]).as_str()) {
        push(out, "synthetic-test");
    }
}

fn run_len(b: &[u8], start: usize, f: &dyn Fn(u8) -> bool) -> usize {
    let mut n = 0;
    while start + n < b.len() && f(b[start + n]) {
        n += 1;
    }
    n
}

fn local_ch(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn valid_domain(d: &str) -> bool {
    if !d.contains('.') {
        return false;
    }
    let labels: Vec<&str> = d.split('.').collect();
    if labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    let tld = labels[labels.len() - 1];
    tld.len() >= 2 && tld.bytes().all(|c| c.is_ascii_alphabetic())
}

/// Any address except a noreply one.
fn has_email(r: &str) -> bool {
    let b = r.as_bytes();
    for i in 0..b.len() {
        if b[i] != b'@' {
            continue;
        }
        let mut s = i;
        while s > 0 && local_ch(b[s - 1]) {
            s -= 1;
        }
        if s == i {
            continue;
        }
        let mut e = i + 1;
        while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'.' || b[e] == b'-') {
            e += 1;
        }
        let dom = r[i + 1..e].trim_end_matches(|c| c == '.' || c == '-');
        if !valid_domain(dom) {
            continue;
        }
        if dom.to_lowercase().contains("noreply") {
            continue;
        }
        return true;
    }
    false
}

fn slash(c: u8) -> bool {
    c == b'/' || c == b'\\'
}

/// A home-directory path in raw, percent, escaped, dash-encoded or underscore-encoded form
/// (the percent and escaped forms arrive here already decoded).
fn home_path(r: &str) -> bool {
    let b = r.as_bytes();
    for root in [j(&["Us", "ers"]), j(&["ho", "me"])] {
        let rb = root.as_bytes();
        let mut from = 0;
        while let Some(off) = r[from..].find(root.as_str()) {
            let p = from + off;
            from = p + 1;
            let after = p + rb.len();
            if p == 0 || after >= b.len() {
                continue;
            }
            let sep_b = b[p - 1];
            let sep_a = b[after];
            if slash(sep_b) && slash(sep_a) {
                let pre_ok = p == 1
                    || !(b[p - 2].is_ascii_alphanumeric() || matches!(b[p - 2], b'.' | b'_' | b'-' | b'/' | b'\\'));
                if pre_ok && after + 1 < b.len() && b[after + 1].is_ascii_alphanumeric() {
                    return true;
                }
            } else if (sep_b == b'-' || sep_b == b'_') && sep_a == sep_b {
                if !(p == 1 || !b[p - 2].is_ascii_alphanumeric()) {
                    continue;
                }
                let mut q = after + 1;
                let mut segs = 0;
                loop {
                    let st = q;
                    while q < b.len() && b[q].is_ascii_alphanumeric() {
                        q += 1;
                    }
                    if q == st {
                        break;
                    }
                    segs += 1;
                    if q < b.len() && b[q] == sep_b {
                        q += 1;
                    } else {
                        break;
                    }
                }
                let need = if root.starts_with('U') { 1 } else { 2 };
                if segs >= need {
                    return true;
                }
            }
        }
    }
    false
}

fn prefix_hits(r: &str, pre: &str, min: usize, f: &dyn Fn(u8) -> bool) -> bool {
    let b = r.as_bytes();
    let mut from = 0;
    while let Some(off) = r[from..].find(pre) {
        let p = from + off;
        from = p + 1;
        if p > 0 && b[p - 1].is_ascii_alphanumeric() {
            continue;
        }
        if run_len(b, p + pre.len(), f) >= min {
            return true;
        }
    }
    false
}

fn alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

fn jwt(r: &str) -> bool {
    let b = r.as_bytes();
    let seg = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
    let mut from = 0;
    let head = j(&["ey", "J"]);
    while let Some(off) = r[from..].find(head.as_str()) {
        let p = from + off;
        from = p + 1;
        if p > 0 && b[p - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut q = p;
        let mut ok = true;
        for k in 0..3 {
            let n = run_len(b, q, &seg);
            if n < 10 {
                ok = false;
                break;
            }
            q += n;
            if k < 2 {
                if q < b.len() && b[q] == b'.' {
                    q += 1;
                } else {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return true;
        }
    }
    false
}

fn assigned_secret(r: &str) -> bool {
    let b = r.as_bytes();
    for i in 0..b.len() {
        if b[i] != b'=' && b[i] != b':' {
            continue;
        }
        if b[i] == b':' && i + 1 < b.len() && b[i + 1] == b'/' {
            continue;
        }
        let mut k = i;
        while k > 0 && b[k - 1] == b' ' {
            k -= 1;
        }
        if k == 0 || !(b[k - 1].is_ascii_alphanumeric() || matches!(b[k - 1], b'_' | b'"' | b'\'')) {
            continue;
        }
        let mut s = i + 1;
        while s < b.len() && matches!(b[s], b' ' | b'"' | b'\'') {
            s += 1;
        }
        let n = run_len(b, s, &|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'+' | b'-'));
        if n < 24 {
            continue;
        }
        let v = &b[s..s + n];
        if v.iter().any(|c| c.is_ascii_uppercase()) && v.iter().any(|c| c.is_ascii_lowercase()) && v.iter().any(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    false
}

fn credential(r: &str, low: &str) -> bool {
    let up_dig = |c: u8| c.is_ascii_uppercase() || c.is_ascii_digit();
    let us = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let dash = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
    if prefix_hits(r, &j(&["AK", "IA"]), 16, &up_dig) {
        return true;
    }
    for c in ["p", "o", "u", "s", "r"] {
        if prefix_hits(r, &j(&["gh", c, "_"]), 30, &alnum) {
            return true;
        }
    }
    if prefix_hits(r, &j(&["github", "_pat_"]), 20, &us) {
        return true;
    }
    if prefix_hits(r, &j(&["s", "k-"]), 20, &dash) {
        return true;
    }
    for c in ["b", "p", "a", "r", "s"] {
        if prefix_hits(r, &j(&["xo", "x", c, "-"]), 10, &dash) {
            return true;
        }
    }
    if prefix_hits(r, &j(&["AI", "za"]), 30, &dash) {
        return true;
    }
    if prefix_hits(r, &j(&["gl", "pat-"]), 16, &dash) {
        return true;
    }
    if low.contains(j(&["-----be", "gin "]).as_str()) && low.contains(j(&["private", " key"]).as_str()) {
        return true;
    }
    jwt(r) || assigned_secret(r)
}

fn session_link(low: &str) -> bool {
    let host = j(&["clau", "de.ai/"]);
    let mut from = 0;
    while let Some(off) = low[from..].find(host.as_str()) {
        let p = from + off + host.len();
        from = p;
        let tail = &low[p..];
        if tail.starts_with("code/") || tail.starts_with("chat") || tail.starts_with("share/") {
            return true;
        }
    }
    low.trim_start().starts_with(j(&["claude-", "session:"]).as_str())
}

fn baseline(low: &str) -> bool {
    let b = low.as_bytes();
    let needle = j(&["base", "line."]);
    let mut from = 0;
    while let Some(off) = low[from..].find(needle.as_str()) {
        let p = from + off;
        from = p + 1;
        if p > 0 && (b[p - 1].is_ascii_alphanumeric() || b[p - 1] == b'_') {
            continue;
        }
        let mut q = p + needle.len();
        while q < b.len() && (b[q].is_ascii_alphanumeric() || matches!(b[q], b'_' | b'.' | b'-')) {
            q += 1;
        }
        while q < b.len() && b[q] == b' ' {
            q += 1;
        }
        if q >= b.len() || (b[q] != b'=' && b[q] != b':') {
            continue;
        }
        q += 1;
        while q < b.len() && matches!(b[q], b' ' | b'"' | b'\'') {
            q += 1;
        }
        if q < b.len() && b[q].is_ascii_digit() {
            return true;
        }
    }
    false
}
