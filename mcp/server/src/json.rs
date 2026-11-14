//! Small JSON reader and writer (RFC 8259). Std only.
//! Objects keep key order; numbers keep their source text so an id round-trips exactly.

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(items) => Some(items),
            _ => None,
        }
    }
}

pub fn parse(text: &str) -> Result<Value, String> {
    let mut p = Parser { b: text.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.b.len() {
        return Err("trailing characters".to_string());
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}'", c as char))
        }
    }

    fn lit(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err("bad literal".to_string())
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        match self.peek() {
            None => Err("unexpected end".to_string()),
            Some(b'n') => self.lit("null", Value::Null),
            Some(b't') => self.lit("true", Value::Bool(true)),
            Some(b'f') => self.lit("false", Value::Bool(false)),
            Some(b'"') => self.string().map(Value::Str),
            Some(b'[') => self.array(depth + 1),
            Some(b'{') => self.object(depth + 1),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err("unexpected character".to_string()),
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("nesting too deep".to_string());
        }
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Value::Arr(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth)?);
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Arr(items));
                }
                _ => return Err("bad array".to_string()),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err("nesting too deep".to_string());
        }
        self.eat(b'{')?;
        let mut pairs = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Value::Obj(pairs));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let v = self.value(depth)?;
            pairs.push((key, v));
            self.ws();
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Obj(pairs));
                }
                _ => return Err("bad object".to_string()),
            }
        }
    }

    fn digits(&mut self) -> Result<(), String> {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        if self.i == start {
            Err("digit expected".to_string())
        } else {
            Ok(())
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => self.digits()?,
            _ => return Err("bad number".to_string()),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            self.digits()?;
        }
        // Slice boundaries sit on ASCII bytes, so this is valid UTF-8.
        Ok(Value::Num(String::from_utf8_lossy(&self.b[start..self.i]).into_owned()))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return Err("short escape".to_string());
        }
        let mut n = 0u32;
        for k in 0..4 {
            let d = (self.b[self.i + k] as char)
                .to_digit(16)
                .ok_or_else(|| "bad hex escape".to_string())?;
            n = n * 16 + d;
        }
        self.i += 4;
        Ok(n)
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = self.peek().ok_or_else(|| "unterminated string".to_string())?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = self.peek().ok_or_else(|| "unterminated escape".to_string())?;
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
                                if !self.b[self.i..].starts_with(b"\\u") {
                                    return Err("lone surrogate".to_string());
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err("bad surrogate pair".to_string());
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return Err("lone surrogate".to_string());
                            } else {
                                hi
                            };
                            char::from_u32(cp).ok_or_else(|| "bad code point".to_string())?
                        }
                        _ => return Err("bad escape".to_string()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                0..=0x1f => return Err("control character in string".to_string()),
                _ => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid utf-8".to_string())
    }
}

pub fn write(v: &Value) -> String {
    let mut out = String::new();
    emit(v, &mut out);
    out
}

fn emit(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Num(n) => out.push_str(n),
        Value::Str(s) => emit_str(s, out),
        Value::Arr(items) => {
            out.push('[');
            for (k, item) in items.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                emit(item, out);
            }
            out.push(']');
        }
        Value::Obj(pairs) => {
            out.push('{');
            for (k, (key, item)) in pairs.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                emit_str(key, out);
                out.push(':');
                emit(item, out);
            }
            out.push('}');
        }
    }
}

fn emit_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_round_trips() {
        let src = include_str!("../../tools.json");
        let first = parse(src).expect("parse list");
        let again = parse(&write(&first)).expect("reparse");
        assert_eq!(first, again);
    }

    #[test]
    fn escapes_round_trip() {
        let v = parse(r#""a\"b\\c\n""#).unwrap();
        assert_eq!(v, Value::Str("a\"b\\c\n".to_string()));
        assert_eq!(write(&v), r#""a\"b\\c\n""#);
        assert_eq!(write(&Value::Str("\u{1}".to_string())), "\"\\u0001\"");
    }

    #[test]
    fn surrogate_pair_decodes() {
        let v = parse(r#""\ud83d\ude00""#).unwrap();
        assert_eq!(v, Value::Str("\u{1F600}".to_string()));
        assert!(parse(r#""\ud83d""#).is_err());
    }

    #[test]
    fn trailing_garbage_refused() {
        assert!(parse("{} x").is_err());
        assert!(parse("[,]").is_err());
        assert!(parse("{not json").is_err());
    }

    #[test]
    fn depth_cap() {
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(parse(&ok).is_ok());
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn ids_round_trip_exact() {
        for src in ["7", "\"x\""] {
            assert_eq!(write(&parse(src).unwrap()), src);
        }
    }
}
