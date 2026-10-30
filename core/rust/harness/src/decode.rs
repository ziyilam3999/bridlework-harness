//! Alternate readings of one line of text: raw, percent-decoded, escape-decoded, and both.

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn percent(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn escapes(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] != '\\' || i + 1 >= c.len() {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let n = c[i + 1];
        match n {
            '/' | '\\' => {
                out.push(n);
                i += 2;
            }
            'x' if i + 3 < c.len() + 0 && c[i + 2].is_ascii_hexdigit() && c[i + 3].is_ascii_hexdigit() => {
                let v = c[i + 2].to_digit(16).unwrap() * 16 + c[i + 3].to_digit(16).unwrap();
                out.push(char::from_u32(v).unwrap_or('?'));
                i += 4;
            }
            'u' if i + 5 < c.len() + 0 && c[i + 2..i + 6].iter().all(|d| d.is_ascii_hexdigit()) => {
                let mut v = 0;
                for d in &c[i + 2..i + 6] {
                    v = v * 16 + d.to_digit(16).unwrap();
                }
                out.push(char::from_u32(v).unwrap_or('?'));
                i += 6;
            }
            '0'..='3' if i + 3 < c.len() + 0 && c[i + 2].is_digit(8) && c[i + 3].is_digit(8) => {
                let v = n.to_digit(8).unwrap() * 64 + c[i + 2].to_digit(8).unwrap() * 8 + c[i + 3].to_digit(8).unwrap();
                out.push(char::from_u32(v).unwrap_or('?'));
                i += 4;
            }
            _ => {
                out.push('\\');
                i += 1;
            }
        }
    }
    out
}

pub fn readings(s: &str) -> Vec<String> {
    let p = percent(s);
    let e = escapes(s);
    let pe = escapes(&p);
    let ep = percent(&e);
    let mut out: Vec<String> = vec![s.to_string()];
    for r in [p, e, pe, ep] {
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}
