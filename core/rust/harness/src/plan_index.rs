//! The plan index reader (std only, no cfg(unix); it does not use launcher.rs). The index is one file of JSON lines, one object per
//! row with eight required keys and one optional key, `left_estimate` (`docs/plan-index.md`). It is private data of the operator, passed by path; none of it is kept in this
//! repo. The reader runs nothing and writes nothing. A refusal of a line names the line, and the field when it has one; no refusal prints a value or a path.
//! It does not check whether a row is built; `harness ticket intake --plan-index` checks that each `main_plan_ref` element is an
//! id of the index, and refuses an index with a repeated id. The seven states are not-built, in-build, built, deviation, later,
//! dropped (the operator decided not to build the row) and unknown; the reader does not judge the deviation object of a deviation or dropped row.

use std::io::Read;
use std::path::Path;

/// Largest index file read, in bytes.
pub const MAX_INDEX_BYTES: u64 = 1024 * 1024;
/// Deepest nesting of arrays and objects in one row.
const MAX_DEPTH: usize = 32;
const KEYS: [&str; 8] = ["id", "title", "plan_line", "state", "tickets", "deviation", "trigger", "code"];
/// Largest left_estimate.
const MAX_ESTIMATE: u64 = 1_000_000;
const DEVIATION_KEYS: [&str; 4] = ["reason", "decided_by", "date", "source"];

/// A row id or a ticket link element: 1 to 64 bytes of ASCII letters, digits, dot, underscore or hyphen.
pub fn id_ok(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NotBuilt,
    InBuild,
    Built,
    Deviation,
    Later,
    /// The operator decided not to build the row; it needs a deviation object like state deviation.
    Dropped,
    /// No source shows the state yet.
    Unknown,
}

impl State {
    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "not-built" => State::NotBuilt,
            "in-build" => State::InBuild,
            "built" => State::Built,
            "deviation" => State::Deviation,
            "later" => State::Later,
            "dropped" => State::Dropped,
            "unknown" => State::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deviation {
    pub reason: Option<String>,
    pub decided_by: Option<String>,
    pub date: Option<String>,
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub plan_line: u64,
    pub state: State,
    pub tickets: Vec<String>,
    pub deviation: Option<Deviation>,
    pub trigger: Option<String>,
    pub code: Vec<String>,
    /// Points of work on the row that has no ticket file yet (optional key).
    pub left_estimate: Option<u64>,
}

#[derive(Debug)]
enum J {
    Null,
    Bool,
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

enum E {
    Syntax,
    Deep,
    Dup(String),
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &[u8]) -> Result<(), E> {
        if self.b.get(self.i..self.i + word.len()) == Some(word) {
            self.i += word.len();
            Ok(())
        } else {
            Err(E::Syntax)
        }
    }

    fn hex4(&mut self) -> Result<u32, E> {
        let h = self.b.get(self.i..self.i + 4).ok_or(E::Syntax)?;
        if !h.iter().all(|c| c.is_ascii_hexdigit()) {
            return Err(E::Syntax);
        }
        self.i += 4;
        u32::from_str_radix(std::str::from_utf8(h).map_err(|_| E::Syntax)?, 16).map_err(|_| E::Syntax)
    }

    fn string(&mut self) -> Result<String, E> {
        if self.b.get(self.i) != Some(&b'"') {
            return Err(E::Syntax);
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or(E::Syntax)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or(E::Syntax)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                // A high surrogate must be followed by a low one.
                                self.lit(b"\\u")?;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(E::Syntax);
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            char::from_u32(cp).ok_or(E::Syntax)?
                        }
                        _ => return Err(E::Syntax),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err(E::Syntax),
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| E::Syntax)
    }

    /// A JSON number, kept as its text (the grammar is checked, the value is read by the caller).
    fn number(&mut self) -> Result<String, E> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let s = p.i;
            while matches!(p.b.get(p.i), Some(b'0'..=b'9')) {
                p.i += 1;
            }
            p.i > s
        };
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        match self.b.get(self.i) {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(E::Syntax),
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(E::Syntax);
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(E::Syntax);
            }
        }
        Ok(String::from_utf8_lossy(&self.b[start..self.i]).into_owned())
    }

    fn value(&mut self, depth: usize) -> Result<J, E> {
        self.ws();
        match self.b.get(self.i).copied() {
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(b'n') => self.lit(b"null").map(|_| J::Null),
            Some(b't') => self.lit(b"true").map(|_| J::Bool),
            Some(b'f') => self.lit(b"false").map(|_| J::Bool),
            Some(b'-' | b'0'..=b'9') => Ok(J::Num(self.number()?)),
            Some(b'[') => {
                if depth >= MAX_DEPTH {
                    return Err(E::Deep);
                }
                self.i += 1;
                let mut v = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(J::Arr(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(J::Arr(v));
                        }
                        _ => return Err(E::Syntax),
                    }
                }
            }
            Some(b'{') => {
                if depth >= MAX_DEPTH {
                    return Err(E::Deep);
                }
                self.i += 1;
                let mut v: Vec<(String, J)> = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(J::Obj(v));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(E::Syntax);
                    }
                    self.i += 1;
                    let val = self.value(depth + 1)?;
                    if v.iter().any(|(e, _)| *e == k) {
                        return Err(E::Dup(k));
                    }
                    v.push((k, val));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(J::Obj(v));
                        }
                        _ => return Err(E::Syntax),
                    }
                }
            }
            _ => Err(E::Syntax),
        }
    }
}

fn take(obj: &mut Vec<(String, J)>, key: &str, n: usize) -> Result<J, String> {
    match obj.iter().position(|(k, _)| k == key) {
        Some(i) => Ok(obj.remove(i).1),
        None => Err(format!("line {n}: field `{key}`: missing")),
    }
}

fn wrong(n: usize, key: &str) -> String {
    format!("line {n}: field `{key}`: wrong type or value")
}

fn strings(j: J, n: usize, key: &str, ids: bool) -> Result<Vec<String>, String> {
    let J::Arr(items) = j else { return Err(wrong(n, key)) };
    items
        .into_iter()
        .map(|e| match e {
            J::Str(s) if !ids || id_ok(&s) => Ok(s),
            _ => Err(wrong(n, key)),
        })
        .collect()
}

fn opt_string(j: J, n: usize, key: &str) -> Result<Option<String>, String> {
    match j {
        J::Null => Ok(None),
        J::Str(s) => Ok(Some(s)),
        _ => Err(wrong(n, key)),
    }
}

fn row(line: &[u8], n: usize) -> Result<Row, String> {
    let mut p = P { b: line, i: 0 };
    let parsed = p.value(0).and_then(|v| {
        p.ws();
        if p.i == p.b.len() {
            Ok(v)
        } else {
            Err(E::Syntax)
        }
    });
    let J::Obj(mut obj) = (match parsed {
        Ok(v) => v,
        Err(E::Deep) => return Err(format!("line {n}: nesting is too deep")),
        Err(E::Dup(k)) if KEYS.contains(&k.as_str()) || k == "left_estimate" || DEVIATION_KEYS.contains(&k.as_str()) => {
            return Err(format!("line {n}: field `{k}`: given twice"))
        }
        Err(E::Dup(_)) => return Err(format!("line {n}: a key is given twice")),
        Err(E::Syntax) => return Err(format!("line {n}: does not parse as one JSON object")),
    }) else {
        return Err(format!("line {n}: does not parse as one JSON object"));
    };
    let id = match take(&mut obj, "id", n)? {
        J::Str(s) if id_ok(&s) => s,
        _ => return Err(wrong(n, "id")),
    };
    let title = match take(&mut obj, "title", n)? {
        J::Str(s) if !s.is_empty() => s,
        _ => return Err(wrong(n, "title")),
    };
    let plan_line = match take(&mut obj, "plan_line", n)? {
        J::Num(s) if s.bytes().all(|b| b.is_ascii_digit()) => s.parse::<u64>().map_err(|_| wrong(n, "plan_line"))?,
        _ => return Err(wrong(n, "plan_line")),
    };
    let state = match take(&mut obj, "state", n)? {
        J::Str(s) => State::parse(&s).ok_or_else(|| wrong(n, "state"))?,
        _ => return Err(wrong(n, "state")),
    };
    let tickets = strings(take(&mut obj, "tickets", n)?, n, "tickets", true)?;
    let deviation = match take(&mut obj, "deviation", n)? {
        J::Null => None,
        J::Obj(fields) => {
            let mut d = Deviation { reason: None, decided_by: None, date: None, source: None };
            for (k, v) in fields {
                let J::Str(s) = v else { return Err(wrong(n, "deviation")) };
                match k.as_str() {
                    "reason" => d.reason = Some(s),
                    "decided_by" => d.decided_by = Some(s),
                    "date" => d.date = Some(s),
                    "source" => d.source = Some(s),
                    _ => return Err(wrong(n, "deviation")),
                }
            }
            Some(d)
        }
        _ => return Err(wrong(n, "deviation")),
    };
    let trigger = opt_string(take(&mut obj, "trigger", n)?, n, "trigger")?;
    let code = strings(take(&mut obj, "code", n)?, n, "code", false)?;
    // Optional ninth key: digits only, 0 to MAX_ESTIMATE.
    let left_estimate = match obj.iter().position(|(k, _)| k == "left_estimate") {
        None => None,
        Some(i) => match obj.remove(i).1 {
            J::Num(s) if s.bytes().all(|b| b.is_ascii_digit()) => match s.parse::<u64>() {
                Ok(v) if v <= MAX_ESTIMATE => Some(v),
                _ => return Err(wrong(n, "left_estimate")),
            },
            _ => return Err(wrong(n, "left_estimate")),
        },
    };
    if !obj.is_empty() {
        return Err(format!("line {n}: an unknown key"));
    }
    Ok(Row { id, title, plan_line, state, tickets, deviation, trigger, code, left_estimate })
}

/// Read rows from the bytes of an index file. Lines end at LF; a CR before the LF is JSON whitespace, so CR LF lines are read. The
/// last row may have no newline. An empty or blanks-only line is refused. Each Err is a reason with no value and no path.
pub fn parse(bytes: &[u8]) -> Result<Vec<Row>, String> {
    if bytes.len() as u64 > MAX_INDEX_BYTES {
        return Err("the file is over 1 MiB".to_string());
    }
    if std::str::from_utf8(bytes).is_err() {
        return Err("the file is not utf-8".to_string());
    }
    if bytes.is_empty() {
        return Err("the file has no rows".to_string());
    }
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut rows = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let n = i + 1;
        if line.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r')) {
            return Err(format!("line {n}: is empty or blank"));
        }
        rows.push(row(line, n)?);
    }
    Ok(rows)
}

/// Read the index file at `path`: at most 1 MiB + 1 bytes are read. A missing file, a folder and an unreadable file are one refusal.
pub fn read(path: &Path) -> Result<Vec<Row>, String> {
    let mut bytes = Vec::new();
    let got = std::fs::File::open(path).and_then(|f| f.take(MAX_INDEX_BYTES + 1).read_to_end(&mut bytes));
    if got.is_err() {
        return Err("the file is missing or cannot be read".to_string());
    }
    parse(&bytes)
}

/// The ids that appear on more than one row, each once, in order of the second sighting.
pub fn repeated_ids(rows: &[Row]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        if rows[..i].iter().any(|e| e.id == r.id) && !out.contains(&r.id) {
            out.push(r.id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROW: &str = r#"{"id":"R-1","title":"t","plan_line":3,"state":"unknown","tickets":[],"deviation":null,"trigger":null,"code":[]}"#;

    #[test]
    fn a_row_reads_and_a_repeat_is_found() {
        let rows = parse(format!("{ROW}\n{ROW}").as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(repeated_ids(&rows), vec!["R-1".to_string()]);
    }

    #[test]
    fn a_dropped_row_reads() {
        let line = ROW.replace("\"unknown\"", "\"dropped\"").replace(
            "\"deviation\":null",
            "\"deviation\":{\"reason\":\"why\",\"decided_by\":\"operator\",\"date\":\"2026-01-02\"}",
        );
        let rows = parse(line.as_bytes()).unwrap();
        assert_eq!(rows[0].state, State::Dropped);
        let d = rows[0].deviation.as_ref().unwrap();
        assert_eq!(d.reason.as_deref(), Some("why"));
        assert_eq!(d.decided_by.as_deref(), Some("operator"));
        assert_eq!(d.date.as_deref(), Some("2026-01-02"));
        let null_dev = ROW.replace("\"unknown\"", "\"dropped\"");
        assert!(parse(null_dev.as_bytes()).unwrap()[0].deviation.is_none());
    }

    fn with(extra: &str) -> String {
        ROW.replace("\"code\":[]", &format!("\"code\":[],{extra}"))
    }

    #[test]
    fn a_row_with_a_left_estimate_reads() {
        let rows = parse(format!("{}\n{}\n{ROW}", with("\"left_estimate\":0"), with("\"left_estimate\":1000000")).as_bytes()).unwrap();
        assert_eq!(rows[0].left_estimate, Some(0));
        assert_eq!(rows[1].left_estimate, Some(1_000_000));
        assert_eq!(rows[2].left_estimate, None);
    }

    #[test]
    fn a_bad_left_estimate_is_refused() {
        let bad = ["\"3\"", "1.5", "-1", "-0", "1e2", "null", "true", "[]", "{}", "1000001", "18446744073709551616"];
        for v in bad {
            let e = parse(with(&format!("\"left_estimate\":{v}")).as_bytes()).unwrap_err();
            assert_eq!(e, "line 1: field `left_estimate`: wrong type or value", "{v}");
        }
    }

    #[test]
    fn a_repeated_left_estimate_is_refused() {
        let e = parse(with("\"left_estimate\":1,\"left_estimate\":2").as_bytes()).unwrap_err();
        assert_eq!(e, "line 1: field `left_estimate`: given twice");
        let e = parse(with("\"zzz\":1").as_bytes()).unwrap_err();
        assert_eq!(e, "line 1: an unknown key");
    }

    #[test]
    fn crlf_lines_are_read() {
        assert_eq!(parse(format!("{ROW}\r\n{ROW}\r\n").as_bytes()).unwrap().len(), 2);
    }

    #[test]
    fn a_surrogate_pair_is_read_and_a_lone_half_is_not() {
        let ok = ROW.replace("\"t\"", "\"\\ud83d\\ude00\"");
        assert_eq!(parse(ok.as_bytes()).unwrap()[0].title, "\u{1f600}");
        assert!(parse(ROW.replace("\"t\"", "\"\\ud83d\"").as_bytes()).is_err());
        assert!(parse(ROW.replace("\"t\"", "\"\\ude00\"").as_bytes()).is_err());
    }
}
