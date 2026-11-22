//! `harness review recall <cases-file> --results <dir>` (std only; `docs/review-recall.md` is the
//! contract). Scores a replay of the fresh reviewer over stored FAIL heads and clean controls: recall
//! per stage and false blocks per set. A FAIL case is a hit at a stage only when that stage and every
//! earlier stage has verdict fail and matched yes, so a finding that never reaches the fix round is a
//! miss at stage fixround. A control is a false block when its report verdict is fail. Read only:
//! nothing is written. The printed lines hold ids, counts and the fixed words only, never an answer
//! key or a head. An error line about one case names it by id (by line number when the id is
//! unreadable) and the cause; no error line holds a field value other than the case id, so no answer
//! key and no head reaches stderr. Fail closed: exit 2 with one stderr
//! line and an empty stdout, and no case is skipped.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

pub const USAGE: &str = "usage: harness review recall <absolute-cases-file> --results <absolute-dir>";

/// The replay stages in order: a hit at a later stage needs every earlier stage too.
const STAGES: [&str; 3] = ["report", "record", "fixround"];

/// One case with its graded results. `through[i]` is set for FAIL cases only: the stages up to and
/// including `i` all had verdict fail and matched yes. A control uses `report_fail` only.
struct Case {
    id: String,
    fail: bool,
    test: bool,
    report_fail: bool,
    through: [bool; 3],
}

fn abs_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

fn id_ok(s: &str) -> bool {
    (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn head_ok(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A tiny JSON reader for the one shape a case line and a result file have: one object of string
/// fields. A non-string value, a duplicate key or anything after the object refuses the whole text.
struct Json<'a> {
    s: &'a str,
    i: usize,
}

impl Json<'_> {
    fn ws(&mut self) {
        while matches!(&self.s.as_bytes()[self.i..].first(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.s.as_bytes().get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = self.s.get(self.i..self.i + 4)?;
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }

    /// One JSON string with its quotes; escapes are decoded, a raw control byte refuses it.
    fn string(&mut self) -> Option<String> {
        if !self.eat(b'"') {
            return None;
        }
        let mut out = String::new();
        loop {
            let c = self.s[self.i..].chars().next()?;
            match c {
                '"' => {
                    self.i += 1;
                    return Some(out);
                }
                '\\' => {
                    self.i += 1;
                    let e = self.s[self.i..].chars().next()?;
                    self.i += 1;
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let mut u = self.hex4()?;
                            u = if (0xD800..0xDC00).contains(&u) {
                                // A high surrogate is read only with its low surrogate after it.
                                if self.s.as_bytes().get(self.i) != Some(&b'\\')
                                    || self.s.as_bytes().get(self.i + 1) != Some(&b'u')
                                {
                                    return None;
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return None;
                                }
                                0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00)
                            } else if (0xDC00..0xE000).contains(&u) {
                                return None;
                            } else {
                                u
                            };
                            out.push(char::from_u32(u)?);
                        }
                        _ => return None,
                    }
                }
                c if (c as u32) < 0x20 => return None,
                c => {
                    out.push(c);
                    self.i += c.len_utf8();
                }
            }
        }
    }

    /// The whole text as one object of string fields, or None.
    fn object(mut self) -> Option<BTreeMap<String, String>> {
        self.ws();
        if !self.eat(b'{') {
            return None;
        }
        let mut m = BTreeMap::new();
        self.ws();
        if self.eat(b'}') {
            self.ws();
            return (self.i == self.s.len()).then_some(m);
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            if !self.eat(b':') {
                return None;
            }
            self.ws();
            let v = self.string()?;
            if m.insert(k, v).is_some() {
                return None;
            }
            self.ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                break;
            }
            return None;
        }
        self.ws();
        (self.i == self.s.len()).then_some(m)
    }
}

fn parse_object(t: &str) -> Option<BTreeMap<String, String>> {
    Json { s: t, i: 0 }.object()
}

/// `harness review recall <cases-file> --results <dir>`: reads the case file and the per-case,
/// per-stage result files, then prints one line per set and stage, one line per set, and the case
/// count. Exit 0 scored; 2 any refusal (usage, a bad case line, a bad or missing result file, a
/// result file that names no case, a set with no FAIL case, a test set with no control), with one
/// stderr line, an empty stdout and nothing written anywhere.
pub fn review_cmd(a: &[String]) -> i32 {
    let fail = |m: &str| -> i32 {
        eprintln!("harness: error: {m}");
        2
    };
    if a.first().map(String::as_str) != Some("recall") {
        eprintln!("{USAGE}");
        return 2;
    }
    let (cases_path, dir) = match (a.get(1), a.get(2), a.get(3), a.get(4)) {
        (Some(c), Some(f), Some(v), None) if f == "--results" => (c.as_str(), v.as_str()),
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    if !abs_ok(cases_path) || !abs_ok(dir) {
        eprintln!("{USAGE}");
        return 2;
    }
    let unreadable = "the cases file is not a readable regular file";
    if !std::fs::metadata(cases_path).map(|m| m.is_file()).unwrap_or(false) {
        return fail(unreadable);
    }
    let Ok(bytes) = std::fs::read(cases_path) else {
        return fail(unreadable);
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return fail("the cases file is not UTF-8 text");
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut cases: Vec<Case> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let Some(f) = parse_object(line) else {
            return fail(&format!("case line {}: the line is not a JSON object of string fields", n + 1));
        };
        // The id names the case in each later error about this case; while it is unreadable the line number does.
        let Some(id) = f.get("id").filter(|v| id_ok(v)) else {
            return fail(&format!("case line {}: the id is missing or not 1 to 32 characters of a-z 0-9 and -", n + 1));
        };
        let bad = |m: &str| format!("case {id}: {m}");
        let fail_kind = match f.get("kind").map(String::as_str) {
            Some("fail") => true,
            Some("control") => false,
            _ => return fail(&bad("kind is missing or unknown")),
        };
        let test = match f.get("set").map(String::as_str) {
            Some("test") => true,
            Some("tune") => false,
            _ => return fail(&bad("set is missing or unknown")),
        };
        if !f.get("head").is_some_and(|h| head_ok(h)) {
            return fail(&bad("head is missing or not 40 lowercase hex characters"));
        }
        if f.get("repo").is_none_or(|r| r.is_empty()) {
            return fail(&bad("repo is missing or empty"));
        }
        let answer = f.get("answer_key");
        if fail_kind {
            if answer.is_none_or(|a| a.is_empty()) {
                return fail(&bad("answer_key is missing or empty"));
            }
        } else if answer.is_some() {
            return fail(&bad("a control case must not carry an answer_key"));
        }
        if !seen.insert(id.clone()) {
            return fail(&bad("duplicate id"));
        }
        cases.push(Case { id: id.clone(), fail: fail_kind, test, report_fail: false, through: [false; 3] });
    }
    let in_set = |test: bool, fail_kind: bool| cases.iter().filter(|c| c.test == test && c.fail == fail_kind).count();
    // The tuning half cannot grade itself: each set needs a FAIL case, and the test set a control.
    if in_set(false, true) == 0 {
        return fail("the tune set has no fail case");
    }
    if in_set(true, true) == 0 {
        return fail("the test set has no fail case");
    }
    if in_set(true, false) == 0 {
        return fail("the test set has no control");
    }
    let dir = PathBuf::from(dir);
    if !std::fs::metadata(&dir).map(|m| m.is_dir()).unwrap_or(false) {
        return fail("the results dir is missing or not a folder");
    }
    // Every file in the results dir must be one case's one stage: anything else names no case.
    let entries = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return fail("the results dir cannot be read"),
    };
    for entry in entries {
        let Ok(entry) = entry else { return fail("the results dir cannot be read") };
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            return fail("the results dir holds a result file that names no case");
        };
        match name.rsplit_once('.') {
            Some((stem, stage)) if STAGES.contains(&stage) && cases.iter().any(|c| c.id == stem) => {}
            _ => return fail("the results dir holds a result file that names no case"),
        }
    }
    for c in &mut cases {
        for (si, stage) in STAGES.iter().enumerate() {
            let path = dir.join(format!("{}.{}", c.id, stage));
            if !std::fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false) {
                return fail(&format!("case {}: the {stage} result file is missing or unreadable", c.id));
            }
            let Ok(bytes) = std::fs::read(&path) else {
                return fail(&format!("case {}: the {stage} result file is missing or unreadable", c.id));
            };
            let Ok(text) = String::from_utf8(bytes) else {
                return fail(&format!("case {}: the {stage} result file is not UTF-8 text", c.id));
            };
            let Some(f) = parse_object(&text) else {
                return fail(&format!("case {}: the {stage} result is not a JSON object of string fields", c.id));
            };
            let verdict_fail = match f.get("verdict").map(String::as_str) {
                Some("fail") => true,
                Some("pass") => false,
                _ => return fail(&format!("case {}: the {stage} result verdict is not pass or fail", c.id)),
            };
            // The grader's mark is read for a FAIL case only; a control result must not carry it.
            let yes = if c.fail {
                match f.get("matched").map(String::as_str) {
                    Some("yes") => true,
                    Some("no") => false,
                    None => return fail(&format!("case {}: the {stage} result has no matched field", c.id)),
                    Some(_) => return fail(&format!("case {}: the {stage} result matched field is not yes or no", c.id)),
                }
            } else {
                if f.contains_key("matched") {
                    return fail(&format!("case {}: the {stage} result of a control carries a matched field", c.id));
                }
                false
            };
            let earlier = si == 0 || c.through[si - 1];
            if c.fail {
                c.through[si] = earlier && verdict_fail && yes;
            } else if si == 0 {
                c.report_fail = verdict_fail;
            }
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for (set, test) in [("tune", false), ("test", true)] {
        for (si, stage) in STAGES.iter().enumerate() {
            let n = cases.iter().filter(|c| c.test == test && c.fail).count();
            let h = cases.iter().filter(|c| c.test == test && c.fail && c.through[si]).count();
            lines.push(format!("set={set} stage={stage} fail_cases={n} hits={h} recall={h}/{n}"));
        }
    }
    for (set, test) in [("tune", false), ("test", true)] {
        let ctl = cases.iter().filter(|c| c.test == test && !c.fail).count();
        let blocks = cases.iter().filter(|c| c.test == test && !c.fail && c.report_fail).count();
        lines.push(format!("set={set} controls={ctl} false_blocks={blocks}"));
    }
    let tune = cases.iter().filter(|c| !c.test).count();
    let test = cases.iter().filter(|c| c.test).count();
    lines.push(format!("cases={} tune={tune} test={test}", cases.len()));
    // Buffered until every case is graded, so a refusal prints nothing on stdout.
    for l in lines {
        println!("{l}");
    }
    0
}
