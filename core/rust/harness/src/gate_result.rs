//! Normalized gate result (std only). One fixed JSON shape every gate speaks; a result without a head sha or with an unknown status is refused.
//! Canonical form: compact, fixed key order, every key present (absent optionals are null), so a read-back re-serializes byte for byte.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateResult {
    pub status: Status,
    pub gate_id: String,
    pub head_sha: String,
    /// Required on fail, absent on pass.
    pub failure_class: Option<String>,
    pub message: String,
    pub file: Option<String>,
    /// Needs a file.
    pub line: Option<u32>,
}

fn token_ok(s: &str, extra: &[char]) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
}

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl GateResult {
    pub fn validate(&self) -> Result<(), String> {
        if !token_ok(&self.gate_id, &['-', '_', '.']) {
            return Err("gate_id missing or malformed".to_string());
        }
        if !sha_ok(&self.head_sha) {
            return Err("head_sha missing or not a full lowercase hex sha".to_string());
        }
        match (&self.status, &self.failure_class) {
            (Status::Fail, None) => return Err("fail needs a failure_class".to_string()),
            (Status::Pass, Some(_)) => return Err("pass must not carry a failure_class".to_string()),
            (_, Some(c)) if !token_ok(c, &['-', '_']) => return Err("failure_class malformed".to_string()),
            _ => {}
        }
        if self.line.is_some() && self.file.is_none() {
            return Err("line needs a file".to_string());
        }
        if self.file.as_deref() == Some("") {
            return Err("file is empty".to_string());
        }
        Ok(())
    }

    /// Canonical JSON. Refuses an invalid result so a bad one is never written.
    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        let mut o = String::from("{\"status\":");
        put_str(&mut o, self.status.as_str());
        o.push_str(",\"gate_id\":");
        put_str(&mut o, &self.gate_id);
        o.push_str(",\"head_sha\":");
        put_str(&mut o, &self.head_sha);
        o.push_str(",\"failure_class\":");
        put_opt(&mut o, self.failure_class.as_deref());
        o.push_str(",\"message\":");
        put_str(&mut o, &self.message);
        o.push_str(",\"file\":");
        put_opt(&mut o, self.file.as_deref());
        o.push_str(",\"line\":");
        match self.line {
            Some(n) => o.push_str(&n.to_string()),
            None => o.push_str("null"),
        }
        o.push('}');
        Ok(o)
    }

    /// Strict reader for the one fixed shape: every key exactly once, no unknown key, then `validate`.
    pub fn from_json(s: &str) -> Result<GateResult, String> {
        let mut p = P { b: s.as_bytes(), i: 0 };
        let r = p.object()?;
        p.ws();
        if p.i != p.b.len() {
            return Err("trailing bytes".to_string());
        }
        r.validate()?;
        Ok(r)
    }

    /// Process exit code for a parse or validate outcome: 0 accepted, 2 refused.
    pub fn exit_code(r: &Result<GateResult, String>) -> i32 {
        if r.is_ok() {
            0
        } else {
            2
        }
    }
}

fn put_str(o: &mut String, s: &str) {
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
}

fn put_opt(o: &mut String, s: Option<&str>) {
    match s {
        Some(v) => put_str(o, v),
        None => o.push_str("null"),
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

enum V {
    Str(String),
    Num(u32),
    Null,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}'", c as char))
        }
    }

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
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => self.unicode()?,
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

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.b.get(self.i..self.i + 4).ok_or("short unicode escape")?;
        let s = std::str::from_utf8(h).map_err(|_| "bad unicode escape".to_string())?;
        let n = u32::from_str_radix(s, 16).map_err(|_| "bad unicode escape".to_string())?;
        self.i += 4;
        Ok(n)
    }

    fn unicode(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        let cp = if (0xD800..0xDC00).contains(&hi) {
            if self.b.get(self.i) != Some(&b'\\') || self.b.get(self.i + 1) != Some(&b'u') {
                return Err("lone surrogate".to_string());
            }
            self.i += 2;
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err("lone surrogate".to_string());
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            hi
        };
        char::from_u32(cp).ok_or_else(|| "bad code point".to_string())
    }

    fn value(&mut self) -> Result<V, String> {
        self.ws();
        match self.b.get(self.i) {
            Some(b'"') => Ok(V::Str(self.string()?)),
            Some(b'n') if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(V::Null)
            }
            Some(c) if c.is_ascii_digit() => {
                let st = self.i;
                while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.b[st..self.i]).unwrap();
                if t.len() > 1 && t.starts_with('0') {
                    return Err("leading zero".to_string());
                }
                Ok(V::Num(t.parse::<u32>().map_err(|_| "number out of range".to_string())?))
            }
            _ => Err("unexpected value".to_string()),
        }
    }

    fn object(&mut self) -> Result<GateResult, String> {
        self.eat(b'{')?;
        let (mut status, mut gate_id, mut head_sha, mut failure_class) = (None, None, None, None);
        let (mut message, mut file, mut line) = (None, None, None);
        loop {
            let k = self.string()?;
            self.eat(b':')?;
            let v = self.value()?;
            let dup = || "duplicate key".to_string();
            match (k.as_str(), v) {
                ("status", V::Str(s)) => {
                    let st = match s.as_str() {
                        "pass" => Status::Pass,
                        "fail" => Status::Fail,
                        _ => return Err("unknown status".to_string()),
                    };
                    if status.replace(st).is_some() {
                        return Err(dup());
                    }
                }
                ("gate_id", V::Str(s)) => {
                    if gate_id.replace(s).is_some() {
                        return Err(dup());
                    }
                }
                ("head_sha", V::Str(s)) => {
                    if head_sha.replace(s).is_some() {
                        return Err(dup());
                    }
                }
                ("message", V::Str(s)) => {
                    if message.replace(s).is_some() {
                        return Err(dup());
                    }
                }
                ("failure_class", v @ (V::Str(_) | V::Null)) => {
                    let o = if let V::Str(s) = v { Some(s) } else { None };
                    if failure_class.replace(o).is_some() {
                        return Err(dup());
                    }
                }
                ("file", v @ (V::Str(_) | V::Null)) => {
                    let o = if let V::Str(s) = v { Some(s) } else { None };
                    if file.replace(o).is_some() {
                        return Err(dup());
                    }
                }
                ("line", v @ (V::Num(_) | V::Null)) => {
                    let o = if let V::Num(n) = v { Some(n) } else { None };
                    if line.replace(o).is_some() {
                        return Err(dup());
                    }
                }
                _ => return Err("unknown key or wrong value type".to_string()),
            }
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                _ => return Err("expected ',' or '}'".to_string()),
            }
        }
        Ok(GateResult {
            status: status.ok_or("status missing")?,
            gate_id: gate_id.ok_or("gate_id missing")?,
            head_sha: head_sha.ok_or("head_sha missing")?,
            failure_class: failure_class.ok_or("failure_class missing")?,
            message: message.ok_or("message missing")?,
            file: file.ok_or("file missing")?,
            line: line.ok_or("line missing")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn fail() -> GateResult {
        GateResult {
            status: Status::Fail,
            gate_id: "unit-tests".to_string(),
            head_sha: SHA.to_string(),
            failure_class: Some("test-failure".to_string()),
            message: "quote \" slash \\ tab \t nl \n bell \u{7} caf\u{e9} \u{1f600}".to_string(),
            file: Some("src/a.rs".to_string()),
            line: Some(42),
        }
    }

    fn pass() -> GateResult {
        GateResult {
            status: Status::Pass,
            gate_id: "unit-tests".to_string(),
            head_sha: SHA.to_string(),
            failure_class: None,
            message: "ok".to_string(),
            file: None,
            line: None,
        }
    }

    #[test]
    fn complete_results_round_trip_byte_identical() {
        for r in [fail(), pass()] {
            let j = r.to_json().unwrap();
            let back = GateResult::from_json(&j).unwrap();
            assert_eq!(back, r);
            assert_eq!(back.to_json().unwrap(), j);
        }
        assert_eq!(
            pass().to_json().unwrap(),
            format!("{{\"status\":\"pass\",\"gate_id\":\"unit-tests\",\"head_sha\":\"{}\",\"failure_class\":null,\"message\":\"ok\",\"file\":null,\"line\":null}}", SHA)
        );
    }

    #[test]
    fn missing_or_bad_head_sha_is_refused() {
        let j = pass().to_json().unwrap();
        let no_sha = j.replace(&format!("\"head_sha\":\"{}\",", SHA), "");
        let r = GateResult::from_json(&no_sha);
        assert!(r.is_err());
        assert_eq!(GateResult::exit_code(&r), 2);
        for bad in ["", "abc", "0123456789ABCDEF0123456789ABCDEF01234567", "zz23456789abcdef0123456789abcdef01234567"] {
            let mut g = pass();
            g.head_sha = bad.to_string();
            assert!(g.validate().is_err());
            assert!(g.to_json().is_err());
        }
    }

    #[test]
    fn unknown_status_is_refused() {
        let j = pass().to_json().unwrap().replace("\"pass\"", "\"maybe\"");
        let r = GateResult::from_json(&j);
        assert_eq!(r.clone().unwrap_err(), "unknown status");
        assert_eq!(GateResult::exit_code(&r), 2);
        assert_eq!(GateResult::exit_code(&Ok(pass())), 0);
    }

    #[test]
    fn consistency_rules() {
        let mut g = fail();
        g.failure_class = None;
        assert!(g.validate().is_err());
        let mut g = pass();
        g.failure_class = Some("x".to_string());
        assert!(g.validate().is_err());
        let mut g = pass();
        g.line = Some(3);
        assert!(g.validate().is_err());
        let mut g = pass();
        g.gate_id = String::new();
        assert!(g.validate().is_err());
    }

    #[test]
    fn strict_reader_refuses_malformed_input() {
        let good = fail().to_json().unwrap();
        assert!(GateResult::from_json(&format!("{} x", good)).is_err());
        assert!(GateResult::from_json(&good.replace("\"line\":42", "\"line\":42,\"extra\":1")).is_err());
        assert!(GateResult::from_json(&good.replace("\"line\":42", "\"line\":042")).is_err());
        assert!(GateResult::from_json(&good.replace("\"line\":42", "\"line\":-1")).is_err());
        assert!(GateResult::from_json(&good.replace("\"gate_id\":\"unit-tests\",", "\"gate_id\":\"a\",\"gate_id\":\"b\",")).is_err());
        assert!(GateResult::from_json(&good.replace("\"line\":42", "\"line\":null").replace("\"file\":\"src/a.rs\"", "\"file\":null")).is_ok());
        assert!(GateResult::from_json("").is_err());
        assert!(GateResult::from_json("{").is_err());
        assert!(GateResult::from_json("[]").is_err());
    }

    #[test]
    fn unicode_escapes_read_back() {
        let j = pass().to_json().unwrap().replace("\"ok\"", "\"a\\u00e9\\ud83d\\ude00\\u0007\"");
        let r = GateResult::from_json(&j).unwrap();
        assert_eq!(r.message, "a\u{e9}\u{1f600}\u{7}");
        assert!(GateResult::from_json(&j.replace("\\ude00", "")).is_err());
    }
}
