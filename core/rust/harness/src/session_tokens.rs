//! `harness session tokens --transcript <file>` and `harness session cost --transcript <file> --model <id> ...`: both read a
//! JSON-lines transcript one line at a time (never the whole file) through the same reader and total the assistant usage and
//! the compactions; `cost` also prices the sums from a price file. std only, so it carries its own small JSON reader.
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

/// The totals of one transcript. `session tokens` prints the first four; `session cost` also uses the three input sums,
/// which are kept in u128 here so that reading never fails on them (the cost verb checks that they fit in u64).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub messages: u64,
    pub max_context: u64,
    pub output: u64,
    pub compactions: u64,
    pub input: u128,
    pub cache_write: u128,
    pub cache_read: u128,
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
                    t.input += u128::from(input);
                    t.cache_write += u128::from(create);
                    t.cache_read += u128::from(read);
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

/// Open `path` and read it as a transcript. The error text is the one `session tokens` and `session cost` both print.
fn read_transcript_file(path: &str) -> Result<Totals, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("the transcript cannot be opened: {}", e.kind()))?;
    read_transcript(std::io::BufReader::new(file))
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
    match read_transcript_file(path) {
        Ok(t) => {
            println!("{}", t.line());
            0
        }
        Err(e) => fail(&e),
    }
}

const COST_USAGE: &str = "usage: harness session cost --transcript <file> --model <id> [--prices <file>] [--label <text>] [--out <file>]";
/// A price file larger than this many bytes is refused.
const MAX_PRICE_FILE: u64 = 64 * 1024;
/// A price has at most this many digits before the point (so the whole-number arithmetic cannot overflow u128).
const MAX_PRICE_INT_DIGITS: usize = 9;
const MICRO: u128 = 1_000_000;

/// `--model` and `--label` values: 1 to 128 characters out of ASCII letters, digits, `.`, `_`, `-` and `:`.
fn safe_value(v: &str) -> bool {
    !v.is_empty() && v.len() <= 128 && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
}

/// The values of the `--model` and `--label` flags: `safe_value`, and not starting with `-` (so `--label --out` cannot swallow
/// the next flag). The model ids in a price file follow `safe_value` only.
fn flag_value_ok(v: &str) -> bool {
    safe_value(v) && !v.starts_with('-')
}

/// One price in millionths of a US dollar per million tokens, from a plain decimal with at most 6 digits after the point.
fn parse_price(t: &str) -> Option<u128> {
    let (int, frac) = match t.split_once('.') {
        Some((i, f)) if !f.is_empty() => (i, f),
        Some(_) => return None,
        None => (t, ""),
    };
    let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || int.len() > MAX_PRICE_INT_DIGITS || !digits(int) || frac.len() > 6 || !digits(frac) {
        return None;
    }
    let i: u128 = int.parse().ok()?;
    let f: u128 = if frac.is_empty() { 0 } else { format!("{frac:0<6}").parse().ok()? };
    Some(i * MICRO + f)
}

/// The four prices of one model: input, cache write, cache read, output (millionths of a dollar per million tokens).
type Prices = [u128; 4];

/// Parse the text of a price file. Errors name the 1-based line number.
fn parse_prices(text: &str) -> Result<Vec<(String, Prices)>, String> {
    let mut out: Vec<(String, Prices)> = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let n = idx + 1;
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 5 || !safe_value(f[0]) {
            return Err(format!("the price file line {n} is malformed: want <model> <input> <cache_write> <cache_read> <output>"));
        }
        let mut p: Prices = [0; 4];
        for (slot, text) in p.iter_mut().zip(&f[1..]) {
            *slot = parse_price(text).ok_or_else(|| format!("the price file line {n} is malformed: bad price {text:?}"))?;
        }
        if out.iter().any(|(m, _)| m == f[0]) {
            return Err(format!("the price file line {n} repeats the model {}", f[0]));
        }
        out.push((f[0].to_string(), p));
    }
    Ok(out)
}

/// Read the price file: a regular file (not a symbolic link) of at most MAX_PRICE_FILE bytes.
fn read_prices(path: &str) -> Result<Vec<(String, Prices)>, String> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("the price file cannot be read: {}", e.kind()))?;
    if !meta.file_type().is_file() {
        return Err("the price file is not a regular file".to_string());
    }
    if meta.len() > MAX_PRICE_FILE {
        return Err(format!("the price file is larger than {MAX_PRICE_FILE} bytes"));
    }
    let f = std::fs::File::open(path).map_err(|e| format!("the price file cannot be read: {}", e.kind()))?;
    let mut bytes = Vec::new();
    f.take(MAX_PRICE_FILE + 1).read_to_end(&mut bytes).map_err(|e| format!("the price file cannot be read: {}", e.kind()))?;
    if bytes.len() as u64 > MAX_PRICE_FILE {
        return Err(format!("the price file is larger than {MAX_PRICE_FILE} bytes"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "the price file is not valid UTF-8".to_string())?;
    parse_prices(&text)
}

/// The amount in whole millionths of a dollar, rounded half up: (sum of tokens * price) / 1_000_000, where each price is in
/// millionths of a dollar per million tokens. Printed as a decimal with exactly 6 digits after the point.
fn usd(tokens: [u64; 4], p: &Prices) -> String {
    let sum: u128 = tokens.iter().zip(p.iter()).map(|(t, p)| u128::from(*t) * p).sum();
    let micro = (sum + MICRO / 2) / MICRO;
    format!("{}.{:06}", micro / MICRO, micro % MICRO)
}

/// Where `--out` may write: an absolute path that is absent or a regular file, not a symbolic link.
fn check_out(path: &str) -> Result<(), String> {
    if !std::path::Path::new(path).is_absolute() {
        return Err("--out must be an absolute path".to_string());
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => Ok(()),
        Ok(_) => Err("--out is not a regular file".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("--out cannot be checked: {}", e.kind())),
    }
}

/// `harness session cost --transcript <file> --model <id> [--prices <file>] [--label <text>] [--out <file>]`. Exit 0 with one
/// stdout line; 2 for usage, an unsafe `--model` or `--label`, any refusal `session tokens` makes, a sum that does not fit in
/// u64, a refused price file or `--out` path (all before anything is printed), or a failed append to `--out` (after the line
/// was printed).
pub fn cost_cmd(a: &[String]) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {m}");
        2
    };
    let mut transcript: Option<&str> = None;
    let mut model: Option<&str> = None;
    let mut prices: Option<&str> = None;
    let mut label: Option<&str> = None;
    let mut out_path: Option<&str> = None;
    let mut it = a.iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--transcript" => &mut transcript,
            "--model" => &mut model,
            "--prices" => &mut prices,
            "--label" => &mut label,
            "--out" => &mut out_path,
            _ => {
                eprintln!("{COST_USAGE}");
                return 2;
            }
        };
        match it.next() {
            Some(v) if slot.is_none() => *slot = Some(v.as_str()),
            _ => {
                eprintln!("{COST_USAGE}");
                return 2;
            }
        }
    }
    let (Some(transcript), Some(model)) = (transcript, model) else {
        eprintln!("{COST_USAGE}");
        return 2;
    };
    if !flag_value_ok(model) {
        return fail("--model must be 1 to 128 characters of ASCII letters, digits, '.', '_', '-' and ':', and must not start with '-'");
    }
    if label.is_some_and(|l| !flag_value_ok(l)) {
        return fail("--label must be 1 to 128 characters of ASCII letters, digits, '.', '_', '-' and ':', and must not start with '-'");
    }
    let t = match read_transcript_file(transcript) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    let (Ok(input), Ok(cache_write), Ok(cache_read)) = (u64::try_from(t.input), u64::try_from(t.cache_write), u64::try_from(t.cache_read)) else {
        return fail("usage is out of range");
    };
    let amount = match prices {
        None => "unpriced".to_string(),
        Some(p) => match read_prices(p) {
            Ok(list) => match list.iter().find(|(m, _)| m == model) {
                Some((_, pr)) => usd([input, cache_write, cache_read, t.output], pr),
                None => "unpriced".to_string(),
            },
            Err(e) => return fail(&e),
        },
    };
    if let Some(Err(e)) = out_path.map(check_out) {
        return fail(&e);
    }
    let line = format!(
        "cost label={} model={} messages={} input={} cache_write={} cache_read={} output={} max_context={} compactions={} usd={}",
        label.unwrap_or("-"),
        model,
        t.messages,
        input,
        cache_write,
        cache_read,
        t.output,
        t.max_context,
        t.compactions,
        amount
    );
    println!("{line}");
    if let Some(o) = out_path {
        use std::io::Write;
        let r = std::fs::OpenOptions::new().append(true).create(true).open(o).and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
        if let Err(e) = r {
            return fail(&format!("--out could not be written: {}", e.kind()));
        }
    }
    0
}
