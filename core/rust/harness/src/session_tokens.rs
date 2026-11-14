//! `harness session tokens --transcript <file>`: reads a JSON-lines transcript one line at a time (never the whole file)
//! and totals the assistant usage and the compactions. std only, so it carries its own small JSON reader.
use std::collections::HashSet;
use std::io::BufRead;

const MAX_DEPTH: usize = 128;

/// A parsed JSON value. A number keeps its source text so an integer is told from a non-integer exactly.
#[derive(Debug)]
enum Json {
    Null,
    Bool,
    Num(String),
    Str(String),
    Arr,
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> Result<(), ()> {
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(())
        }
    }
    fn lit(&mut self, word: &[u8]) -> Result<(), ()> {
        if self.b[self.i..].starts_with(word) {
            self.i += word.len();
            Ok(())
        } else {
            Err(())
        }
    }
    fn value(&mut self, depth: usize) -> Result<Json, ()> {
        if depth > MAX_DEPTH {
            return Err(());
        }
        self.ws();
        match self.b.get(self.i).copied().ok_or(())? {
            b'{' => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.eat(b'}').is_ok() {
                    return Ok(Json::Obj(m));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    let v = self.value(depth + 1)?;
                    m.push((k, v));
                    self.ws();
                    if self.eat(b',').is_ok() {
                        continue;
                    }
                    self.eat(b'}')?;
                    return Ok(Json::Obj(m));
                }
            }
            b'[' => {
                self.i += 1;
                self.ws();
                if self.eat(b']').is_ok() {
                    return Ok(Json::Arr);
                }
                loop {
                    self.value(depth + 1)?;
                    self.ws();
                    if self.eat(b',').is_ok() {
                        continue;
                    }
                    self.eat(b']')?;
                    return Ok(Json::Arr);
                }
            }
            b'"' => self.string().map(Json::Str),
            b't' => self.lit(b"true").map(|_| Json::Bool),
            b'f' => self.lit(b"false").map(|_| Json::Bool),
            b'n' => self.lit(b"null").map(|_| Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(()),
        }
    }
    fn digits(&mut self) -> Result<(), ()> {
        let s = self.i;
        while matches!(self.b.get(self.i), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        if self.i == s {
            Err(())
        } else {
            Ok(())
        }
    }
    fn number(&mut self) -> Result<Json, ()> {
        let s = self.i;
        let _ = self.eat(b'-');
        if self.b.get(self.i) == Some(&b'0') {
            self.i += 1;
        } else {
            self.digits()?;
        }
        if self.eat(b'.').is_ok() {
            self.digits()?;
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            self.digits()?;
        }
        std::str::from_utf8(&self.b[s..self.i]).map(|t| Json::Num(t.to_string())).map_err(|_| ())
    }
    fn hex4(&mut self) -> Result<u32, ()> {
        let h = self.b.get(self.i..self.i + 4).ok_or(())?;
        let t = std::str::from_utf8(h).map_err(|_| ())?;
        if !t.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(());
        }
        self.i += 4;
        u32::from_str_radix(t, 16).map_err(|_| ())
    }
    fn string(&mut self) -> Result<String, ()> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or(())?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| ()),
                0..=0x1f => return Err(()),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or(())?;
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
                            let mut u = self.hex4()?;
                            if (0xD800..0xDC00).contains(&u) {
                                self.lit(b"\\u")?;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(());
                                }
                                u = 0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00);
                            }
                            char::from_u32(u).ok_or(())?
                        }
                        _ => return Err(()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
}

fn parse_line(bytes: &[u8]) -> Result<Json, ()> {
    let mut p = P { b: bytes, i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i == bytes.len() {
        Ok(v)
    } else {
        Err(())
    }
}

/// The four totals printed on the one stdout line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub messages: u64,
    pub max_context: u64,
    pub output: u64,
    pub compactions: u64,
}

impl Totals {
    pub fn line(&self) -> String {
        format!("tokens messages={} max_context={} output={} compactions={}", self.messages, self.max_context, self.output, self.compactions)
    }
}

fn field(usage: &Json, name: &str, line_no: usize) -> Result<u64, String> {
    match usage.get(name) {
        Some(Json::Num(t)) if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) => {
            t.parse::<u64>().map_err(|_| format!("line {line_no}: usage.{name} is out of range"))
        }
        _ => Err(format!("line {line_no}: usage.{name} is missing or not an integer")),
    }
}

/// Read a transcript from any buffered reader, one line at a time. A repeated assistant `message.id` counts once (its first
/// line is the one read for usage). Errors carry the 1-based line number where one applies.
pub fn read_transcript<R: BufRead>(mut r: R) -> Result<Totals, String> {
    let mut t = Totals::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut line_no = 0usize;
    loop {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf).map_err(|e| format!("the transcript cannot be read: {}", e.kind()))?;
        if n == 0 {
            break;
        }
        line_no += 1;
        let text = buf.strip_suffix(b"\n").unwrap_or(&buf);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        let v = parse_line(text).map_err(|_| format!("line {line_no} is not valid JSON"))?;
        match v.get("type").and_then(Json::as_str) {
            Some("assistant") => {
                let msg = v.get("message").ok_or_else(|| format!("line {line_no}: assistant line has no message"))?;
                let id = msg.get("id").and_then(Json::as_str).ok_or_else(|| format!("line {line_no}: assistant message has no id"))?;
                let usage = msg.get("usage").ok_or_else(|| format!("line {line_no}: assistant message has no usage"))?;
                let input = field(usage, "input_tokens", line_no)?;
                let create = field(usage, "cache_creation_input_tokens", line_no)?;
                let read = field(usage, "cache_read_input_tokens", line_no)?;
                let out = field(usage, "output_tokens", line_no)?;
                if seen.insert(id.to_string()) {
                    let ctx = input
                        .checked_add(create)
                        .and_then(|x| x.checked_add(read))
                        .ok_or_else(|| format!("line {line_no}: usage is out of range"))?;
                    t.messages += 1;
                    t.max_context = t.max_context.max(ctx);
                    t.output = t.output.checked_add(out).ok_or_else(|| format!("line {line_no}: usage is out of range"))?;
                }
            }
            Some("system") if v.get("subtype").and_then(Json::as_str) == Some("compact_boundary") => t.compactions += 1,
            _ => {}
        }
    }
    if line_no == 0 {
        return Err("the transcript is empty".to_string());
    }
    Ok(t)
}

const TOKENS_USAGE: &str = "usage: harness session tokens --transcript <file>";

/// `harness session tokens --transcript <file>`. Exit 0 with one stdout line; 2 for usage, a missing or empty file, a line
/// that is not JSON (stderr names the line), or an assistant line whose usage is missing or not integers.
pub fn tokens_cmd(a: &[String]) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let path = match a {
        [flag, path] if flag == "--transcript" => path,
        _ => {
            eprintln!("{}", TOKENS_USAGE);
            return 2;
        }
    };
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return fail(&format!("the transcript cannot be opened: {}", e.kind())),
    };
    match read_transcript(std::io::BufReader::new(file)) {
        Ok(t) => {
            println!("{}", t.line());
            0
        }
        Err(e) => fail(&e),
    }
}
