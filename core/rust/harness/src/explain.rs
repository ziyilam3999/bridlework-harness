//! Output lines. Hits print as name:line:class. The explain form prints a fixed placeholder, never matched bytes.
use crate::scan::Hit;

fn safe(name: &str) -> String {
    name.chars().map(|c| if c == ':' || c.is_control() { '_' } else { c }).collect()
}

pub fn hit_line(h: &Hit) -> String {
    format!("{}:{}:{}", safe(&h.name), h.line, h.class)
}

pub fn text_line(h: &Hit) -> String {
    format!("{}:{}", safe(&h.name), h.class)
}

pub fn explain_line(h: &Hit, text_mode: bool) -> String {
    if text_mode {
        format!("explain: {} class={} value=[redacted]", safe(&h.name), h.class)
    } else {
        format!("explain: {}:{} class={} value=[redacted]", safe(&h.name), h.line, h.class)
    }
}
