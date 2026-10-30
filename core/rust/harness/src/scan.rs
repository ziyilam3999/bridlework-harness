//! Turns named blocks of lines into hits. Hits hold a name, a line number and a class id, never bytes.
use crate::detect;
use crate::overlay::List;

pub struct Ctx<'a> {
    pub list: &'a List,
    pub testmode: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Hit {
    pub name: String,
    pub line: usize,
    pub class: &'static str,
}

pub fn scan_lines(ctx: &Ctx, name: &str, lines: &[(usize, String)], out: &mut Vec<Hit>) {
    for (n, text) in lines {
        for c in detect::classes_for_line(text.trim_end_matches('\r'), ctx.list) {
            if ctx.testmode && c == "synthetic-test" {
                continue;
            }
            let h = Hit { name: name.to_string(), line: *n, class: c };
            if !out.contains(&h) {
                out.push(h);
            }
        }
    }
}

/// Scan a whole text; lines are numbered from 1.
pub fn scan_text(ctx: &Ctx, name: &str, text: &str) -> Vec<Hit> {
    let lines: Vec<(usize, String)> = text.lines().enumerate().map(|(i, l)| (i + 1, l.to_string())).collect();
    let mut out = Vec::new();
    scan_lines(ctx, name, &lines, &mut out);
    out
}
