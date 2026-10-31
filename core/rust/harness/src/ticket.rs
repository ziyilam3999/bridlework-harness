//! The ticket reader (std only). One ticket is one JSON file with exactly eleven keys (`docs/ticket-contract.md`); `Ticket::parse` turns
//! the text into data and runs nothing. Reading fails closed: anything the contract does not name is a refusal that names the field.
//! The repo comes from the ticket's `repo_id` through `RepoId`, never from the folder the reader runs in.

use crate::repo_id::RepoId;

/// The eleven required keys. `schemas/ticket.schema.json` mirrors this set; a test pins the two together.
pub const FIELDS: [&str; 11] =
    ["id", "repo_id", "title", "group", "size", "objective", "acceptance_criteria", "dependencies", "risk", "owner_role", "evidence_links"];
/// The closed lists from the contract.
const SIZES: [&str; 3] = ["S", "M", "L"];
const RISKS: [&str; 2] = ["normal", "high"];
const OWNER_ROLES: [&str; 6] =
    ["orchestration_portfolio", "execution_verification", "risk_review", "friction_learning", "knowledge_lessons", "harness_governance"];
const MAX_TICKET_BYTES: usize = 1024 * 1024;

/// One parsed ticket. Pure data: `parse` validates every value once, and only validated values go onward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub id: String,
    pub repo_id: RepoId,
    pub title: String,
    pub group: String,
    pub size: String,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub dependencies: Vec<String>,
    pub risk: String,
    pub owner_role: String,
    pub evidence_links: Vec<String>,
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.ws();
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

    /// A string. Escapes are the plain set (`\" \\ \/ \n \r \t \uXXXX`); a raw control byte and a lone surrogate are refused.
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
                        b'u' => {
                            let h = self.b.get(self.i..self.i + 4).ok_or("short unicode escape")?;
                            let s = std::str::from_utf8(h).map_err(|_| "bad unicode escape".to_string())?;
                            if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                                return Err("bad unicode escape".to_string());
                            }
                            self.i += 4;
                            char::from_u32(u32::from_str_radix(s, 16).map_err(|_| "bad unicode escape".to_string())?).ok_or("lone surrogate")?
                        }
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

    fn string_array(&mut self) -> Result<Vec<String>, String> {
        self.eat(b'[')?;
        let mut v = Vec::new();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(v);
        }
        loop {
            v.push(self.string()?);
            match self.peek() {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(v);
                }
                _ => return Err("expected ',' or ']'".to_string()),
            }
        }
    }
}

fn field<T>(name: &str, r: Result<T, String>) -> Result<T, String> {
    r.map_err(|e| format!("`{}`: {}", name, e))
}

fn once<T>(slot: &mut Option<T>, name: &str, v: T) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("duplicate key `{}`", name));
    }
    *slot = Some(v);
    Ok(())
}

fn non_empty(name: &str, s: &str) -> Result<(), String> {
    if s.is_empty() {
        Err(format!("`{}`: must not be empty", name))
    } else {
        Ok(())
    }
}

fn one_of(name: &str, s: &str, list: &[&str]) -> Result<(), String> {
    if list.contains(&s) {
        Ok(())
    } else {
        Err(format!("`{}`: must be one of {}", name, list.join(", ")))
    }
}

impl Ticket {
    /// Strict parse of the one ticket shape: eleven keys, each exactly once, no unknown key, no optional key, no default. The Err names
    /// the field. Text over 1 MiB is refused.
    pub fn parse(text: &str) -> Result<Ticket, String> {
        if text.len() > MAX_TICKET_BYTES {
            return Err("ticket is over 1 MiB".to_string());
        }
        let mut p = P { b: text.as_bytes(), i: 0 };
        p.eat(b'{').map_err(|_| "not a JSON object".to_string())?;
        let (mut id, mut repo_id, mut title, mut group) = (None, None, None, None);
        let (mut size, mut objective, mut acceptance_criteria) = (None, None, None);
        let (mut dependencies, mut risk, mut owner_role, mut evidence_links) = (None, None, None, None);
        if p.peek() == Some(b'}') {
            p.i += 1;
        } else {
            loop {
                let key = p.string().map_err(|e| format!("key: {}", e))?;
                p.eat(b':').map_err(|e| format!("`{}`: {}", key, e))?;
                match key.as_str() {
                    "id" => once(&mut id, "id", field("id", p.string())?)?,
                    "repo_id" => once(&mut repo_id, "repo_id", field("repo_id", p.string())?)?,
                    "title" => once(&mut title, "title", field("title", p.string())?)?,
                    "group" => once(&mut group, "group", field("group", p.string())?)?,
                    "size" => once(&mut size, "size", field("size", p.string())?)?,
                    "objective" => once(&mut objective, "objective", field("objective", p.string())?)?,
                    "acceptance_criteria" => once(&mut acceptance_criteria, "acceptance_criteria", field("acceptance_criteria", p.string_array())?)?,
                    "dependencies" => once(&mut dependencies, "dependencies", field("dependencies", p.string_array())?)?,
                    "risk" => once(&mut risk, "risk", field("risk", p.string())?)?,
                    "owner_role" => once(&mut owner_role, "owner_role", field("owner_role", p.string())?)?,
                    "evidence_links" => once(&mut evidence_links, "evidence_links", field("evidence_links", p.string_array())?)?,
                    _ => return Err(format!("unknown key `{}`", key)),
                }
                match p.peek() {
                    Some(b',') => p.i += 1,
                    Some(b'}') => {
                        p.i += 1;
                        break;
                    }
                    _ => return Err(format!("`{}`: expected ',' or '}}'", key)),
                }
            }
        }
        p.ws();
        if p.i != p.b.len() {
            return Err("trailing bytes".to_string());
        }
        let id = id.ok_or("missing field `id`")?;
        let repo_id = repo_id.ok_or("missing field `repo_id`")?;
        let title = title.ok_or("missing field `title`")?;
        let group = group.ok_or("missing field `group`")?;
        let size = size.ok_or("missing field `size`")?;
        let objective = objective.ok_or("missing field `objective`")?;
        let acceptance_criteria = acceptance_criteria.ok_or("missing field `acceptance_criteria`")?;
        let dependencies = dependencies.ok_or("missing field `dependencies`")?;
        let risk = risk.ok_or("missing field `risk`")?;
        let owner_role = owner_role.ok_or("missing field `owner_role`")?;
        let evidence_links = evidence_links.ok_or("missing field `evidence_links`")?;
        non_empty("id", &id)?;
        let repo_id = RepoId::parse(&repo_id)?;
        non_empty("title", &title)?;
        non_empty("group", &group)?;
        one_of("size", &size, &SIZES)?;
        non_empty("objective", &objective)?;
        if acceptance_criteria.is_empty() {
            return Err("`acceptance_criteria`: must not be empty".to_string());
        }
        one_of("risk", &risk, &RISKS)?;
        one_of("owner_role", &owner_role, &OWNER_ROLES)?;
        Ok(Ticket { id, repo_id, title, group, size, objective, acceptance_criteria, dependencies, risk, owner_role, evidence_links })
    }

    /// Read from raw bytes: over 1 MiB and not UTF-8 are refused before any parsing.
    pub fn parse_bytes(bytes: &[u8]) -> Result<Ticket, String> {
        if bytes.len() > MAX_TICKET_BYTES {
            return Err("ticket is over 1 MiB".to_string());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| "ticket is not utf-8".to_string())?;
        Ticket::parse(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The valid control ticket as ordered (key, raw JSON value) pairs.
    fn base() -> Vec<(&'static str, &'static str)> {
        vec![
            ("id", "\"T-1\""),
            ("repo_id", "\"scratch-1\""),
            ("title", "\"A title\""),
            ("group", "\"grp\""),
            ("size", "\"M\""),
            ("objective", "\"An objective\""),
            ("acceptance_criteria", "[\"one\",\"two\"]"),
            ("dependencies", "[\"T-0\"]"),
            ("risk", "\"normal\""),
            ("owner_role", "\"execution_verification\""),
            ("evidence_links", "[]"),
        ]
    }

    fn render(pairs: &[(&str, &str)]) -> String {
        let body: Vec<String> = pairs.iter().map(|(k, v)| format!("\"{}\":{}", k, v)).collect();
        format!("{{{}}}", body.join(","))
    }

    fn without(key: &str) -> String {
        let pairs: Vec<_> = base().into_iter().filter(|(k, _)| *k != key).collect();
        render(&pairs)
    }

    fn with(key: &str, raw: &str) -> String {
        let pairs: Vec<_> = base().into_iter().map(|(k, v)| if k == key { (k, raw) } else { (k, v) }).collect();
        render(&pairs)
    }

    #[test]
    fn a_complete_ticket_parses() {
        let t = Ticket::parse(&render(&base())).expect("the control ticket loads");
        assert_eq!(t.id, "T-1");
        assert_eq!(t.repo_id.as_str(), "scratch-1");
        assert_eq!(t.title, "A title");
        assert_eq!(t.group, "grp");
        assert_eq!(t.size, "M");
        assert_eq!(t.objective, "An objective");
        assert_eq!(t.acceptance_criteria, vec!["one".to_string(), "two".to_string()]);
        assert_eq!(t.dependencies, vec!["T-0".to_string()]);
        assert_eq!(t.risk, "normal");
        assert_eq!(t.owner_role, "execution_verification");
        assert!(t.evidence_links.is_empty());
        // The repo id is what was planted, not a constant.
        let other = Ticket::parse(&with("repo_id", "\"other.repo_2\"")).unwrap();
        assert_eq!(other.repo_id.as_str(), "other.repo_2");
    }

    #[test]
    fn the_example_file_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../examples/ticket-example.json");
        let bytes = std::fs::read(path).expect("the example is committed");
        let t = Ticket::parse_bytes(&bytes).expect("the example loads");
        assert_eq!(t.id, "EX-1");
        assert_eq!(t.repo_id.as_str(), "scratch-1");
        assert_eq!(t.size, "S");
        assert_eq!(t.risk, "normal");
        assert_eq!(t.owner_role, "execution_verification");
        assert_eq!(t.acceptance_criteria.len(), 2);
        assert_eq!(t.dependencies, vec!["EX-0".to_string()]);
        assert_eq!(t.evidence_links, vec!["docs/ticket-contract.md".to_string()]);
    }

    #[test]
    fn a_ticket_with_no_repo_id_is_refused() {
        let err = Ticket::parse(&without("repo_id")).expect_err("missing repo_id must be refused");
        assert!(err.contains("repo_id"), "names the field: {err}");
        for raw in ["\"\"", "null", "\"a/b\"", "\"..\"", "\"Repo\""] {
            let err = Ticket::parse(&with("repo_id", raw)).expect_err(&format!("repo_id {raw} must be refused"));
            assert!(err.contains("repo_id"), "names the field for {raw}: {err}");
        }
    }

    #[test]
    fn an_unknown_ticket_field_is_refused() {
        let mut pairs = base();
        pairs.push(("extra", "\"x\""));
        let err = Ticket::parse(&render(&pairs)).expect_err("unknown key must be refused");
        assert!(err.contains("extra"), "names the key: {err}");
        let mut dup = base();
        dup.push(("title", "\"again\""));
        let err = Ticket::parse(&render(&dup)).expect_err("duplicate key must be refused");
        assert!(err.contains("title") && err.contains("duplicate"), "names the key: {err}");
    }

    #[test]
    fn a_missing_required_field_is_refused_naming_the_field() {
        for key in FIELDS {
            let err = Ticket::parse(&without(key)).expect_err(&format!("{key} removed must be refused"));
            assert!(err.contains(&format!("`{key}`")), "names {key}: {err}");
        }
    }

    #[test]
    fn malformed_ticket_values_are_refused() {
        // Control arm: all-valid values load.
        assert!(Ticket::parse(&render(&base())).is_ok(), "control ticket loads");
        let cases: [(&str, &str); 17] = [
            ("size", "\"XL\""),
            ("size", "\"s\""),
            ("risk", "\"low\""),
            ("owner_role", "\"nobody\""),
            ("acceptance_criteria", "[]"),
            ("acceptance_criteria", "[1]"),
            ("dependencies", "\"T-0\""),
            ("evidence_links", "\"x\""),
            ("evidence_links", "[null]"),
            ("id", "\"\""),
            ("id", "5"),
            ("title", "\"\""),
            ("group", "\"\""),
            ("objective", "\"\""),
            ("title", "null"),
            ("size", "5"),
            ("acceptance_criteria", "\"one\""),
        ];
        for (key, raw) in cases {
            let err = Ticket::parse(&with(key, raw)).expect_err(&format!("{key}={raw} must be refused"));
            assert!(err.contains(&format!("`{key}`")), "names {key} for {raw}: {err}");
        }
        let valid = render(&base());
        assert!(Ticket::parse(&format!("{valid} junk")).expect_err("trailing text").contains("trailing"));
        assert!(Ticket::parse("").is_err(), "an empty file is refused");
        assert!(Ticket::parse_bytes(&[0xff, 0xfe, b'{']).expect_err("non-utf-8").contains("utf-8"));
        let big = format!("{{\"id\":\"{}\"}}", "a".repeat(MAX_TICKET_BYTES));
        assert!(Ticket::parse(&big).expect_err("over 1 MiB").contains("1 MiB"));
        assert!(Ticket::parse_bytes(big.as_bytes()).expect_err("over 1 MiB bytes").contains("1 MiB"));
    }

    fn required_of(schema: &str) -> Vec<String> {
        let at = schema.find("\"required\"").expect("schema has a required array");
        let open = at + schema[at..].find('[').unwrap();
        let close = open + schema[open..].find(']').unwrap();
        schema[open + 1..close].split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect()
    }

    #[test]
    fn the_committed_schema_matches_the_ticket_parser() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let schema = std::fs::read_to_string(repo.join("schemas/ticket.schema.json")).expect("the schema is committed and utf-8");
        let mut got = required_of(&schema);
        got.sort();
        let mut want: Vec<String> = FIELDS.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, want, "the schema's required set equals the reader's eleven fields");
        // Planted failing twin of the comparison: a drifted set must differ.
        let drifted = schema.replacen("\"evidence_links\"]", "]", 1);
        let mut drift = required_of(&drifted);
        drift.sort();
        assert_ne!(drift, want, "a schema that dropped a field is detected");
    }
}
