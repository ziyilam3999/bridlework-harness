//! Per-line class detection. Patterns are logic or split literals, so this file never matches itself.
use crate::decode;
use crate::overlay::List;

/// Detector needles are stored XOR-ed with one key and decoded once per process, so the engine binary holds no
/// whole marker in its read-only data (the release artifact scan reads the binary). The tables are produced from
/// the fragment form by the round-trip test below; never edit them by hand.
const KEY: u8 = 0x5a;

fn nd(enc: &[u8]) -> String {
    let k = std::hint::black_box(KEY);
    enc.iter().map(|b| (b ^ k) as char).collect()
}

const E_COAUTH: &[u8] = &[0x39, 0x35, 0x77, 0x3b, 0x2f, 0x2e, 0x32, 0x35, 0x28, 0x3f, 0x3e, 0x77, 0x38, 0x23, 0x60];
const E_SYNTH: &[u8] = &[0x29, 0x23, 0x34, 0x2e, 0x32, 0x3f, 0x2e, 0x33, 0x39, 0x77, 0x2e, 0x3f, 0x29, 0x2e, 0x77, 0x37, 0x3b, 0x28, 0x31, 0x3f, 0x28];
const E_USERS: &[u8] = &[0x0f, 0x29, 0x3f, 0x28, 0x29];
const E_HOME: &[u8] = &[0x32, 0x35, 0x37, 0x3f];
const E_JWT: &[u8] = &[0x3f, 0x23, 0x10];
const E_AKIA: &[u8] = &[0x1b, 0x11, 0x13, 0x1b];
const E_PAT: &[u8] = &[0x3d, 0x33, 0x2e, 0x32, 0x2f, 0x38, 0x05, 0x2a, 0x3b, 0x2e, 0x05];
const E_SK: &[u8] = &[0x29, 0x31, 0x77];
const E_AIZA: &[u8] = &[0x1b, 0x13, 0x20, 0x3b];
const E_GLPAT: &[u8] = &[0x3d, 0x36, 0x2a, 0x3b, 0x2e, 0x77];
const E_PEM_BEGIN: &[u8] = &[0x77, 0x77, 0x77, 0x77, 0x77, 0x38, 0x3f, 0x3d, 0x33, 0x34, 0x7a];
const E_PEM_KEY: &[u8] = &[0x2a, 0x28, 0x33, 0x2c, 0x3b, 0x2e, 0x3f, 0x7a, 0x31, 0x3f, 0x23];
const E_HOST: &[u8] = &[0x39, 0x36, 0x3b, 0x2f, 0x3e, 0x3f, 0x74, 0x3b, 0x33, 0x75];
const E_SESS: &[u8] = &[0x39, 0x36, 0x3b, 0x2f, 0x3e, 0x3f, 0x77, 0x29, 0x3f, 0x29, 0x29, 0x33, 0x35, 0x34, 0x60];
const E_BASE: &[u8] = &[0x38, 0x3b, 0x29, 0x3f, 0x36, 0x33, 0x34, 0x3f, 0x74];
const E_GH_0: &[u8] = &[0x3d, 0x32, 0x2a, 0x05];
const E_GH_1: &[u8] = &[0x3d, 0x32, 0x35, 0x05];
const E_GH_2: &[u8] = &[0x3d, 0x32, 0x2f, 0x05];
const E_GH_3: &[u8] = &[0x3d, 0x32, 0x29, 0x05];
const E_GH_4: &[u8] = &[0x3d, 0x32, 0x28, 0x05];
const E_XOX_0: &[u8] = &[0x22, 0x35, 0x22, 0x38, 0x77];
const E_XOX_1: &[u8] = &[0x22, 0x35, 0x22, 0x2a, 0x77];
const E_XOX_2: &[u8] = &[0x22, 0x35, 0x22, 0x3b, 0x77];
const E_XOX_3: &[u8] = &[0x22, 0x35, 0x22, 0x28, 0x77];
const E_XOX_4: &[u8] = &[0x22, 0x35, 0x22, 0x29, 0x77];

struct Needles {
    coauth: String,
    synth: String,
    users: String,
    home: String,
    jwt: String,
    akia: String,
    pat: String,
    sk: String,
    aiza: String,
    glpat: String,
    pem_begin: String,
    pem_key: String,
    host: String,
    sess: String,
    base: String,
    gh: [String; 5],
    xox: [String; 5],
}

fn needles() -> &'static Needles {
    static N: std::sync::OnceLock<Needles> = std::sync::OnceLock::new();
    N.get_or_init(|| Needles {
        coauth: nd(E_COAUTH),
        synth: nd(E_SYNTH),
        users: nd(E_USERS),
        home: nd(E_HOME),
        jwt: nd(E_JWT),
        akia: nd(E_AKIA),
        pat: nd(E_PAT),
        sk: nd(E_SK),
        aiza: nd(E_AIZA),
        glpat: nd(E_GLPAT),
        pem_begin: nd(E_PEM_BEGIN),
        pem_key: nd(E_PEM_KEY),
        host: nd(E_HOST),
        sess: nd(E_SESS),
        base: nd(E_BASE),
        gh: [nd(E_GH_0), nd(E_GH_1), nd(E_GH_2), nd(E_GH_3), nd(E_GH_4)],
        xox: [nd(E_XOX_0), nd(E_XOX_1), nd(E_XOX_2), nd(E_XOX_3), nd(E_XOX_4)],
    })
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
    if low.trim_start().starts_with(needles().coauth.as_str()) && has_email(r) {
        push(out, "coauthor-personal");
    }
    if baseline(&low) {
        push(out, "baseline-number");
    }
    if low.contains(needles().synth.as_str()) {
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
        let dom = r[i + 1..e].trim_end_matches(['.', '-']);
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
    for root in [needles().users.as_str(), needles().home.as_str()] {
        let rb = root.as_bytes();
        let mut from = 0;
        while let Some(off) = r[from..].find(root) {
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
    let head = needles().jwt.as_str();
    while let Some(off) = r[from..].find(head) {
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
    if prefix_hits(r, &needles().akia, 16, &up_dig) {
        return true;
    }
    for pre in &needles().gh {
        if prefix_hits(r, pre, 30, &alnum) {
            return true;
        }
    }
    if prefix_hits(r, &needles().pat, 20, &us) {
        return true;
    }
    if prefix_hits(r, &needles().sk, 20, &dash) {
        return true;
    }
    for pre in &needles().xox {
        if prefix_hits(r, pre, 10, &dash) {
            return true;
        }
    }
    if prefix_hits(r, &needles().aiza, 30, &dash) {
        return true;
    }
    if prefix_hits(r, &needles().glpat, 16, &dash) {
        return true;
    }
    if low.contains(needles().pem_begin.as_str()) && low.contains(needles().pem_key.as_str()) {
        return true;
    }
    jwt(r) || assigned_secret(r)
}

fn session_link(low: &str) -> bool {
    let host = needles().host.as_str();
    let mut from = 0;
    while let Some(off) = low[from..].find(host) {
        let p = from + off + host.len();
        from = p;
        let tail = &low[p..];
        if tail.starts_with("code/") || tail.starts_with("chat") || tail.starts_with("share/") {
            return true;
        }
    }
    low.trim_start().starts_with(needles().sess.as_str())
}

fn baseline(low: &str) -> bool {
    let b = low.as_bytes();
    let needle = needles().base.as_str();
    let mut from = 0;
    while let Some(off) = low[from..].find(needle) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn j(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn encoded_needles_round_trip_to_their_fragment_form() {
        let all: Vec<(&[u8], String)> = vec![
        (E_COAUTH, j(&["co-auth", "ored-by:"])),
        (E_SYNTH, j(&["synthetic", "-test", "-marker"])),
        (E_USERS, j(&["Us", "ers"])),
        (E_HOME, j(&["ho", "me"])),
        (E_JWT, j(&["ey", "J"])),
        (E_AKIA, j(&["AK", "IA"])),
        (E_PAT, j(&["github", "_pat_"])),
        (E_SK, j(&["s", "k-"])),
        (E_AIZA, j(&["AI", "za"])),
        (E_GLPAT, j(&["gl", "pat-"])),
        (E_PEM_BEGIN, j(&["-----be", "gin "])),
        (E_PEM_KEY, j(&["private", " key"])),
        (E_HOST, j(&["clau", "de.ai/"])),
        (E_SESS, j(&["claude-", "session:"])),
        (E_BASE, j(&["base", "line."])),
        (E_GH_0, j(&["gh", "p", "_"])),
        (E_GH_1, j(&["gh", "o", "_"])),
        (E_GH_2, j(&["gh", "u", "_"])),
        (E_GH_3, j(&["gh", "s", "_"])),
        (E_GH_4, j(&["gh", "r", "_"])),
        (E_XOX_0, j(&["xo", "x", "b", "-"])),
        (E_XOX_1, j(&["xo", "x", "p", "-"])),
        (E_XOX_2, j(&["xo", "x", "a", "-"])),
        (E_XOX_3, j(&["xo", "x", "r", "-"])),
        (E_XOX_4, j(&["xo", "x", "s", "-"])),
        ];
        assert_eq!(all.len(), 25);
        for (enc, want) in all {
            assert_eq!(nd(enc), want);
        }
    }

    #[test]
    fn needles_are_decoded_once_and_stable() {
        let a = needles() as *const Needles;
        let b = needles() as *const Needles;
        assert_eq!(a, b);
        assert_eq!(needles().host, j(&["clau", "de.ai/"]));
    }
}
