//! `harness session limit-check --transcript <file> --provider <claude|zai> [--utc-offset <+HH:MM|-HH:MM>] [--now-ms <n>]`:
//! reads one seat transcript, one line at a time, and says whether the last error row is a usage limit. Read-only: one file is
//! read, nothing is written, no provider is called. std only, so it carries its own small JSON reader.
use std::io::BufRead;

const MAX_DEPTH: usize = 128;
/// Eight days in milliseconds: a reset further ahead than this is unknown.
const MAX_AHEAD_MS: i128 = 691_200_000;
/// The provider zone of z.ai texts, +08:00, in seconds.
const ZAI_OFFSET_S: i128 = 8 * 3600;

const USAGE: &str = "usage: harness session limit-check --transcript <file> --provider <claude|zai> [--utc-offset <+HH:MM|-HH:MM>] [--now-ms <n>]";

/// A parsed JSON value. A number keeps its source text; an array keeps its items.
enum Json {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Json>),
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
                let mut items = Vec::new();
                self.ws();
                if self.eat(b']').is_ok() {
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b',').is_ok() {
                        continue;
                    }
                    self.eat(b']')?;
                    return Ok(Json::Arr(items));
                }
            }
            b'"' => self.string().map(Json::Str),
            b't' => self.lit(b"true").map(|_| Json::Bool(true)),
            b'f' => self.lit(b"false").map(|_| Json::Bool(false)),
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

/// The fields of one error row that the verdict reads.
struct ErrorRow {
    status: Option<String>,
    error: Option<String>,
    transient: bool,
    text: Option<String>,
}

fn error_row(v: &Json) -> ErrorRow {
    let status = match v.get("apiErrorStatus") {
        Some(Json::Num(t)) => Some(t.clone()),
        _ => None,
    };
    let error = v.get("error").and_then(Json::as_str).map(str::to_string);
    let transient = matches!(v.get("apiErrorIsTransient"), Some(Json::Bool(true)));
    let text = match v.get("message").and_then(|m| m.get("content")) {
        Some(Json::Arr(items)) => items
            .iter()
            .find(|i| i.get("type").and_then(Json::as_str) == Some("text"))
            .and_then(|i| i.get("text"))
            .and_then(Json::as_str)
            .map(str::to_string),
        _ => None,
    };
    ErrorRow { status, error, transient, text }
}

/// What reading the whole file found.
enum Found {
    NoError,
    Recovered,
    Last(ErrorRow),
}

/// Read every line. Errors are the text after `reason=unreadable`.
fn read_rows<R: BufRead>(mut r: R) -> Result<Found, String> {
    let mut buf: Vec<u8> = Vec::new();
    let mut line_no = 0usize;
    let mut last: Option<ErrorRow> = None;
    let mut recovered = false;
    loop {
        buf.clear();
        let n = r.read_until(b'\n', &mut buf).map_err(|e| format!("the transcript cannot be read: {}", e.kind()))?;
        if n == 0 {
            break;
        }
        line_no += 1;
        let text = buf.strip_suffix(b"\n").unwrap_or(&buf);
        if std::str::from_utf8(text).is_err() {
            return Err(format!("line {line_no} is not valid UTF-8"));
        }
        let v = parse_line(text).map_err(|_| format!("line {line_no} is not valid JSON"))?;
        if !matches!(v, Json::Obj(_)) {
            return Err(format!("line {line_no} is not a JSON object"));
        }
        match v.get("isApiErrorMessage") {
            Some(Json::Bool(true)) => {
                last = Some(error_row(&v));
                recovered = false;
            }
            None | Some(Json::Bool(false)) if last.is_some() && v.get("type").and_then(Json::as_str) == Some("assistant") => {
                recovered = true;
            }
            _ => {}
        }
    }
    if line_no == 0 {
        return Err("the transcript is empty".to_string());
    }
    Ok(match last {
        None => Found::NoError,
        Some(_) if recovered => Found::Recovered,
        Some(row) => Found::Last(row),
    })
}

// ---- time ----

fn days_from_civil(y: i128, m: i128, d: i128) -> i128 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i128) -> (i128, i128, i128) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_leap(y: i128) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i128, m: i128) -> i128 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap(y) => 29,
        _ => 28,
    }
}

fn fmt_utc(sec: i128) -> String {
    let (y, m, d) = civil_from_days(sec.div_euclid(86400));
    let s = sec.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, (s % 3600) / 60, s % 60)
}

/// A reset moment (UTC seconds) is kept when it is after now and at most 8 days ahead.
fn bounded(utc_s: i128, now_ms: i128) -> Option<i128> {
    let ms = utc_s * 1000;
    (ms > now_ms && ms - now_ms <= MAX_AHEAD_MS).then_some(utc_s)
}

fn digits_to(t: &str) -> Option<i128> {
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// `H am`, `H pm`, `H:MM am`, `H:MM pm` written without the space: seconds since midnight.
fn parse_clock(t: &str) -> Option<i128> {
    let (body, pm) = match (t.strip_suffix("am"), t.strip_suffix("pm")) {
        (Some(b), _) => (b, false),
        (_, Some(b)) => (b, true),
        _ => return None,
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h, digits_to(m)?),
        Some(_) => return None,
        None => (body, 0),
    };
    if h.len() > 2 {
        return None;
    }
    let h = digits_to(h)?;
    if !(1..=12).contains(&h) || m > 59 {
        return None;
    }
    Some((h % 12 + if pm { 12 } else { 0 }) * 3600 + m * 60)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// The reset words of a Claude text: what follows `resets ` up to the opening bracket of the zone name.
fn claude_reset_words(text: &str) -> Option<&str> {
    let rest = text.split_once("resets ")?.1;
    Some(rest.split('(').next().unwrap_or(rest).trim())
}

/// The Claude reset in UTC seconds, from the words, the offset (seconds) and now. None when it does not parse or is out of bound.
fn claude_reset(words: &str, offset_s: Option<i128>, now_ms: i128) -> Option<i128> {
    let off = offset_s?;
    let parts: Vec<&str> = words.split(' ').collect();
    let local_now = now_ms.div_euclid(1000) + off;
    let today = local_now.div_euclid(86400);
    let utc = match parts.as_slice() {
        [clock] => {
            let secs = parse_clock(clock)?;
            let cand = today * 86400 + secs - off;
            if cand * 1000 <= now_ms {
                cand + 86400
            } else {
                cand
            }
        }
        [mon, day, "at", clock] => {
            let m = MONTHS.iter().position(|x| x == mon)? as i128 + 1;
            if day.len() > 2 {
                return None;
            }
            let d = digits_to(day)?;
            let secs = parse_clock(clock)?;
            let at = |y: i128| -> Option<i128> {
                if d < 1 || d > days_in_month(y, m) {
                    return None;
                }
                Some(days_from_civil(y, m, d) * 86400 + secs - off)
            };
            let (y, _, _) = civil_from_days(today);
            let cand = at(y)?;
            if cand * 1000 <= now_ms {
                at(y + 1)?
            } else {
                cand
            }
        }
        _ => return None,
    };
    bounded(utc, now_ms)
}

/// The first `YYYY-MM-DD HH:MM:SS` in the text, read at +08:00, in UTC seconds (bounded).
fn zai_reset(text: &str, now_ms: i128) -> Option<i128> {
    let b = text.as_bytes();
    let shape = b"dddd-dd-dd dd:dd:dd";
    for i in 0..b.len().saturating_sub(shape.len() - 1) {
        let w = &b[i..i + shape.len()];
        let ok = w.iter().zip(shape).all(|(c, s)| if *s == b'd' { c.is_ascii_digit() } else { c == s });
        if !ok {
            continue;
        }
        let n = |a: usize, z: usize| -> i128 { std::str::from_utf8(&w[a..z]).ok().and_then(|t| t.parse().ok()).unwrap_or(-1) };
        let (y, mo, d, h, mi, s) = (n(0, 4), n(5, 7), n(8, 10), n(11, 13), n(14, 16), n(17, 19));
        if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) || h > 23 || mi > 59 || s > 59 {
            return None;
        }
        let utc = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s - ZAI_OFFSET_S;
        return bounded(utc, now_ms);
    }
    None
}

// ---- shapes ----

struct Limit {
    scope: &'static str,
    code: &'static str,
    resets: Option<i128>,
}

/// The model name of a `You've hit your <name> limit` or `You've reached your <name> limit` text.
fn model_name(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("You've hit your ").or_else(|| text.strip_prefix("You've reached your "))?;
    let at = rest.find(" limit")?;
    let name = rest[..at].trim_matches(' ');
    let after = rest[at + 6..].chars().next();
    let name_ok = !name.is_empty()
        && rest[..at].len() == name.len()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b' ' || b == b'.');
    let end_ok = !after.is_some_and(|c| c.is_alphanumeric());
    (name_ok && end_ok && !name.eq_ignore_ascii_case("weekly") && !name.eq_ignore_ascii_case("session")).then_some(name)
}

fn claude_shape(text: &str, offset_s: Option<i128>, now_ms: i128) -> Option<Limit> {
    let reset = |t: &str| claude_reset_words(t).and_then(|w| claude_reset(w, offset_s, now_ms));
    if text.starts_with("You've hit your weekly limit · resets ") {
        return Some(Limit { scope: "provider", code: "weekly", resets: reset(text) });
    }
    if text.starts_with("You've hit your session limit · resets ") {
        return Some(Limit { scope: "provider", code: "session", resets: reset(text) });
    }
    let name = model_name(text)?;
    let scope = if name.split(' ').next() == Some("Fable") { "fable" } else { "provider" };
    Some(Limit { scope, code: "model", resets: reset(text) })
}

fn zai_shape(text: &str, now_ms: i128) -> Option<Limit> {
    if text.contains("[1310]") || text.contains("Weekly/Monthly Limit Exhausted") {
        return Some(Limit { scope: "provider", code: "1310", resets: zai_reset(text, now_ms) });
    }
    if text.contains("[1113]") {
        return Some(Limit { scope: "provider", code: "1113", resets: None });
    }
    None
}

/// A limit marker: the start of a Claude shape, a z.ai code, or `Limit Exhausted`.
fn has_marker(text: &str) -> bool {
    text.starts_with("You've hit your ")
        || text.starts_with("You've reached your ")
        || ["[1310]", "[1113]", "[1308]", "Limit Exhausted"].iter().any(|m| text.contains(m))
}

enum Verdict {
    Limit(Limit),
    Other,
    Unknown,
}

fn classify(row: &ErrorRow, provider: &str, offset_s: Option<i128>, now_ms: i128) -> Verdict {
    if row.transient {
        return Verdict::Other;
    }    let Some(text) = row.text.as_deref() else { return Verdict::Unknown };
    let is429 = row.status.as_deref() == Some("429");
    let rate = row.error.as_deref() == Some("rate_limit");
    if is429 && rate {
        let shape = if provider == "claude" { claude_shape(text, offset_s, now_ms) } else { zai_shape(text, now_ms) };
        return shape.map_or(Verdict::Unknown, Verdict::Limit);
    }
    if has_marker(text) || is429 || rate {
        return Verdict::Unknown;
    }
    Verdict::Other
}

/// The first 80 characters of the text, each control character replaced by one space.
fn short_text(text: &str) -> String {
    text.chars().take(80).map(|c| if (c as u32) < 0x20 || c as u32 == 0x7f { ' ' } else { c }).collect()
}

fn status_code(row: &ErrorRow) -> String {
    match row.status.as_deref() {
        Some(s) if !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit()) => s.to_string(),
        _ => "-".to_string(),
    }
}

/// `+HH:MM` or `-HH:MM`: HH 00 to 14, MM 00 to 59. The offset in seconds.
fn parse_offset(t: &str) -> Option<i128> {
    let b = t.as_bytes();
    if !t.is_ascii() || b.len() != 6 || b[3] != b':' || !matches!(b[0], b'+' | b'-') {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i128> {
        let s = &t[a..z];
        if s.bytes().all(|c| c.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let (h, m) = (num(1, 3)?, num(4, 6)?);
    if h > 14 || m > 59 {
        return None;
    }
    let s = h * 3600 + m * 60;
    Some(if b[0] == b'-' { -s } else { s })
}

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

/// `harness session limit-check ...`. Exit 0 limit, 2 usage, 3 unreadable transcript, 4 other, 5 unknown, 6 no error row or
/// the seat recovered. One stdout line on 0, 4 and 5; stdout is empty otherwise.
pub fn limit_check_cmd(a: &[String]) -> i32 {
    let mut transcript: Option<&str> = None;
    let mut provider: Option<&str> = None;
    let mut offset: Option<&str> = None;
    let mut now: Option<&str> = None;
    let mut it = a.iter();
    while let Some(flag) = it.next() {
        let slot = match flag.as_str() {
            "--transcript" => &mut transcript,
            "--provider" => &mut provider,
            "--utc-offset" => &mut offset,
            "--now-ms" => &mut now,
            _ => return usage(),
        };
        match it.next() {
            Some(v) if slot.is_none() => *slot = Some(v.as_str()),
            _ => return usage(),
        }
    }
    let (Some(transcript), Some(provider)) = (transcript, provider) else { return usage() };
    if transcript.is_empty() || !matches!(provider, "claude" | "zai") {
        return usage();
    }
    let offset_s = match offset.map(parse_offset) {
        Some(None) => return usage(),
        Some(Some(s)) => Some(s),
        None => None,
    };
    let now_ms: i128 = match now {
        Some(t) => match digits_to(t) {
            Some(n) => n,
            None => return usage(),
        },
        None => match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i128,
            Err(_) => {
                eprintln!("harness: error: clock is before the epoch");
                return 2;
            }
        },
    };
    let refuse = |why: &str| {
        eprintln!("harness: limit-check refused: reason=unreadable {why}");
        3
    };
    match std::fs::metadata(transcript) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return refuse("the transcript is not a regular file"),
        Err(e) => return refuse(&format!("the transcript cannot be opened: {}", e.kind())),
    }
    let file = match std::fs::File::open(transcript) {
        Ok(f) => f,
        Err(e) => return refuse(&format!("the transcript cannot be opened: {}", e.kind())),
    };
    let row = match read_rows(std::io::BufReader::new(file)) {
        Err(e) => return refuse(&e),
        Ok(Found::NoError) => {
            eprintln!("harness: limit-check: reason=no-error-row");
            return 6;
        }
        Ok(Found::Recovered) => {
            eprintln!("harness: limit-check: reason=recovered");
            return 6;
        }
        Ok(Found::Last(r)) => r,
    };
    let text = short_text(row.text.as_deref().unwrap_or(""));
    let code = status_code(&row);
    match classify(&row, provider, offset_s, now_ms) {
        Verdict::Limit(l) => {
            let resets = l.resets.map_or("unknown".to_string(), fmt_utc);
            println!("class=limit provider={provider} scope={} code={} resets={resets} text={text}", l.scope, l.code);
            0
        }
        Verdict::Other => {
            println!("class=other provider=- scope=- code={code} resets=unknown text={text}");
            4
        }
        Verdict::Unknown => {
            println!("class=unknown provider=- scope=- code={code} resets=unknown text={text}");
            5
        }
    }
}
