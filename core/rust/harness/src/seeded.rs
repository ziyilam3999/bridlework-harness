//! Seeded random-input tests (std only, Unix, test builds). Hand-made inputs missed shapes of the words-only reader
//! (`words_only::strip_comments`), so a small generator feeds it, and the caps record reader (`caps::Record::decode`),
//! inputs nobody wrote by hand.
//!
//! Generator: `Rng` is splitmix64. `SEEDS` is a fixed list of 16 seeds; each seed drives `CASES` cases. A case gets its
//! own stream from `Rng::for_case(seed, case)`, so no clock, thread id, environment value, address or hash map order
//! feeds an input: two runs see the same bytes (`the_seed_list_gives_the_same_inputs_twice`).
//!
//! Replay: a failing case panics with its seed, its case number and the input as escaped bytes. `replay(prop, seed,
//! case)` runs one seed and case of one property (`prop` is one of the `w1`, `w2`, `w3`, `c1`, `c2`, `c3` functions);
//! each test runs its cases through it.
//!
//! Words-only: W1 raw bytes do not panic and give the same answer twice; W2 text with no plain comment is unchanged;
//! W3 a plain `//` comment added at a line end is removed, and a line of only blanks and a plain comment is dropped.
//! Caps reader: C1 raw bytes and near-records do not panic and give the same answer twice; C2 when decode accepts
//! bytes, the result has the asked ticket and repo, passes `state_ok`, and encodes to the same bytes; C3 decode of
//! encode gives the record back for each state `state_ok` accepts, with seeded ticket and repo text.

use crate::caps::Record;
use crate::words_only::strip_comments;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicUsize, Ordering};

const SEEDS: [u64; 16] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_002A,
    0x0000_0000_DEAD_BEEF,
    0x0123_4567_89AB_CDEF,
    0xFEDC_BA98_7654_3210,
    0x1357_9BDF_0246_8ACE,
    0xA5A5_A5A5_5A5A_5A5A,
    0x0F0F_0F0F_F0F0_F0F0,
    0xCAFE_F00D_0BAD_C0DE,
    0x9E37_79B9_7F4A_7C15,
    0x0000_0001_0000_0001,
    0x7FFF_FFFF_FFFF_FFFF,
    0x8000_0000_0000_0000,
    0x00FF_00FF_00FF_00FF,
    0x1BAD_B002_1BAD_B002,
    0x5EED_5EED_5EED_5EED,
];
const CASES: u32 = 200;
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

fn fin(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn for_case(seed: u64, case: u32) -> Rng {
        Rng(fin(seed.wrapping_add(GOLDEN)) ^ fin(u64::from(case).wrapping_add(GOLDEN)))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(GOLDEN);
        fin(self.0)
    }

    /// 0..n (n above 0).
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// True one time in n.
    fn chance(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[self.below(xs.len())]
    }
}

type Fail = (Vec<u8>, String);
type Prop = fn(&mut Rng) -> Result<(), Fail>;

fn esc(b: &[u8]) -> String {
    b.iter().flat_map(|&x| std::ascii::escape_default(x)).map(char::from).collect()
}

/// Runs one case of one property; a failure panics with the seed, the case and the input as escaped bytes.
fn replay(prop: Prop, seed: u64, case: u32) {
    let mut rng = Rng::for_case(seed, case);
    if let Err((input, why)) = prop(&mut rng) {
        panic!("seeded case failed: seed={seed:#x} case={case} input=\"{}\": {why}", esc(&input));
    }
}

fn run_all(prop: Prop) {
    for &seed in &SEEDS {
        for case in 0..CASES {
            replay(prop, seed, case);
        }
    }
}

fn pieces(r: &mut Rng, pool: &[&str], max: usize) -> String {
    let n = r.below(max + 1);
    (0..n).map(|_| r.pick(pool)).collect()
}

// ---- words-only inputs ----

const LEXY: &[u8] = b"/*\"'#r\\\n\r \tb![]cx0e{u";

fn raw_bytes(r: &mut Rng) -> Vec<u8> {
    let n = r.below(513);
    (0..n).map(|_| if r.chance(2) { r.next_u64() as u8 } else { r.pick(LEXY) }).collect()
}

const STR_PIECES: [&str; 12] = ["a", "b c", "//", "/*", "*/", "\\\"", "\\\\", "\\n", "'", "#", "é", " // x"];
const RAW_PIECES: [&str; 9] = ["a", "b c", "//", "/*", "*/", "\\", "'", "é", " // x"];
const DOC_WORDS: [&str; 8] = ["x", "a_b", "\"q\"", "'c", "//", "\\", "/* n */", "é"];
const DOC_LINE_WORDS: [&str; 9] = ["x", "a_b", "\"q\"", "'c", "//", "\\", "/*", "*/", "r#\""];
const CHARS: [&str; 10] = ["'a'", "'\\n'", "'\\''", "'\"'", "'/'", "'é'", "'\\u{41}'", "'\\\\'", "'*'", "' '"];
const LIFETIMES: [&str; 7] = ["'a", "'static", "'_", "'x1", "'é", "'r", "'b"];
const NUMBERS: [&str; 7] = ["0", "42", "1.5", "0x1f", "1e10", "7u8", "0b1"];
const PUNCT: [&str; 20] =
    ["->", "=>", "::", "..", "&&", "||", "+=", "<=", "/", "(", ")", "{", "}", "[", "]", ";", ",", "*", "!", "#[derive(X)]"];

fn ident(r: &mut Rng) -> String {
    let mut s = String::new();
    s.push(r.pick(&['a', 'b', 'r', 'c', 'x', '_', 'z', 'é']));
    for _ in 0..r.below(6) {
        s.push(r.pick(&['a', 'b', 'r', 'c', 'x', '_', 'z', 'é', '0', '1']));
    }
    s
}

fn string_tok(r: &mut Rng, multi: bool) -> String {
    let mut pool = STR_PIECES.to_vec();
    if multi {
        pool.push("\n");
    }
    let pre = r.pick(&["", "b", "c"]);
    format!("{pre}\"{}\"", pieces(r, &pool, 4))
}

fn raw_tok(r: &mut Rng, multi: bool) -> String {
    let pre = r.pick(&["r", "br", "cr"]);
    let k = r.below(4);
    let h = "#".repeat(k);
    let mut pool = RAW_PIECES.to_vec();
    if k > 0 {
        pool.push("\"");
    }
    if multi {
        pool.push("\n");
    }
    format!("{pre}{h}\"{}\"{h}", pieces(r, &pool, 4))
}

fn doc_block(r: &mut Rng, multi: bool) -> String {
    let mut s = String::from(r.pick(&["/**", "/*!"]));
    for _ in 0..=r.below(3) {
        s.push_str(if multi && r.chance(3) { r.pick(&["\n", "\r\n"]) } else { " " });
        s.push_str(r.pick(&DOC_WORDS));
    }
    s.push_str(" */");
    s
}

fn token(r: &mut Rng, multi: bool) -> String {
    match r.below(10) {
        0 | 1 => ident(r),
        2 => r.pick(&NUMBERS).to_string(),
        3 | 4 => r.pick(&PUNCT).to_string(),
        5 => string_tok(r, multi),
        6 => raw_tok(r, multi),
        7 => r.pick(&CHARS).to_string(),
        8 => {
            if r.chance(4) {
                r.pick(&["r#type", "r#fn"]).to_string()
            } else {
                r.pick(&LIFETIMES).to_string()
            }
        }
        _ => doc_block(r, multi),
    }
}

struct Line {
    text: Vec<u8>,
    eol: &'static [u8],
    /// The line ends in a doc line comment: no comment can be added after it.
    doc_end: bool,
}

/// Comment-free lines: no blank at a line end, no plain comment, no shebang.
fn gen_lines(r: &mut Rng, multi: bool, final_eol: bool) -> Vec<Line> {
    let n = 1 + r.below(8);
    let mut lines = Vec::new();
    for k in 0..n {
        let mut eol: &'static [u8] = if r.chance(2) { b"\n" } else { b"\r\n" };
        if k == n - 1 && !final_eol && r.chance(4) {
            eol = b"";
        }
        let mut text = String::new();
        let mut doc_end = false;
        if !r.chance(8) {
            for _ in 0..r.below(4) {
                text.push(r.pick(&[' ', '\t']));
            }
            let toks = 1 + r.below(6);
            for i in 0..toks {
                if i > 0 {
                    text.push(' ');
                }
                if i == toks - 1 && r.chance(8) {
                    let words: Vec<&str> = (0..1 + r.below(3)).map(|_| r.pick(&DOC_LINE_WORDS)).collect();
                    text.push_str(&format!("{} {}", r.pick(&["///", "//!"]), words.join(" ")));
                    doc_end = true;
                } else {
                    text.push_str(&token(r, multi));
                }
            }
        }
        lines.push(Line { text: text.into_bytes(), eol, doc_end });
    }
    lines
}

fn render(lines: &[Line]) -> Vec<u8> {
    let mut v = Vec::new();
    for l in lines {
        v.extend_from_slice(&l.text);
        v.extend_from_slice(l.eol);
    }
    v
}

fn w2_text(r: &mut Rng) -> Vec<u8> {
    render(&gen_lines(r, true, false))
}

/// A plain line comment (`//` then text; not `///`, not `//!`; no newline).
fn plain_comment(r: &mut Rng) -> String {
    let mut s = String::from("//");
    s.push_str(r.pick(&[" ", "x", "*", "\"", "'", "//"]));
    for _ in 0..r.below(6) {
        s.push_str(r.pick(&["a", "b", " ", "/*", "*/", "\"", "'", "\\", "/", "é", "\t"]));
    }
    s
}

fn blanks(r: &mut Rng, lo: usize) -> String {
    (0..lo + r.below(3)).map(|_| r.pick(&[' ', '\t'])).collect()
}

fn eol(r: &mut Rng) -> &'static [u8] {
    if r.chance(2) {
        b"\n"
    } else {
        b"\r\n"
    }
}

/// Returns the text with comments added and the base text it must read like.
fn w3_texts(r: &mut Rng) -> (Vec<u8>, Vec<u8>) {
    let lines = gen_lines(r, false, true);
    let mut v = Vec::new();
    for l in &lines {
        if r.chance(6) {
            v.extend_from_slice(format!("{}{}", blanks(r, 0), plain_comment(r)).as_bytes());
            v.extend_from_slice(eol(r));
        }
        v.extend_from_slice(&l.text);
        if !l.doc_end && !l.text.is_empty() && r.chance(3) {
            v.extend_from_slice(format!("{}{}", blanks(r, 1), plain_comment(r)).as_bytes());
        }
        v.extend_from_slice(l.eol);
    }
    if r.chance(6) {
        v.extend_from_slice(format!("{}{}", blanks(r, 0), plain_comment(r)).as_bytes());
        v.extend_from_slice(eol(r));
    }
    (v, render(&lines))
}

fn w1(r: &mut Rng) -> Result<(), Fail> {
    let s = raw_bytes(r);
    let a = catch_unwind(|| strip_comments(&s));
    let b = catch_unwind(|| strip_comments(&s));
    match (a, b) {
        (Ok(x), Ok(y)) if x == y => Ok(()),
        (Ok(_), Ok(_)) => Err((s, "two runs gave different answers".into())),
        _ => Err((s, "strip_comments panicked".into())),
    }
}

fn w2(r: &mut Rng) -> Result<(), Fail> {
    let s = w2_text(r);
    match strip_comments(&s) {
        Some(out) if out == s => Ok(()),
        Some(out) => Err((s, format!("the output differs: \"{}\"", esc(&out)))),
        None => Err((s, "the reader refused the text".into())),
    }
}

fn w3(r: &mut Rng) -> Result<(), Fail> {
    let (with, base) = w3_texts(r);
    let want = strip_comments(&base);
    let got = strip_comments(&with);
    if want.is_some() && want == got {
        return Ok(());
    }
    let shown = got.map_or("refused".to_string(), |g| esc(&g));
    Err((with, format!("the base text reads as \"{}\", this reads as \"{shown}\"", want.map_or("refused".to_string(), |w| esc(&w)))))
}

// ---- caps record inputs ----

fn id(r: &mut Rng) -> String {
    let n = 1 + r.below(10);
    (0..n).map(|_| r.pick(&['a', 'b', 'x', 'z', '0', '1', '9', '-', '_', '.', 'A', 'Z'])).collect()
}

fn odd_text(r: &mut Rng) -> String {
    pieces(r, &["a", "Z", "0", " ", "\"", "\\", ",", "{", "}", "é", "\n", "-", ":", "\u{1F600}", "\\\""], 12)
}

fn fields(t: &str, rp: &str, fr: u8, inf: bool) -> Vec<String> {
    vec![
        "\"v\":1".to_string(),
        format!("\"ticket\":\"{t}\""),
        format!("\"repo_id\":\"{rp}\""),
        format!("\"fix_rounds\":{fr}"),
        format!("\"in_flight\":{inf}"),
    ]
}

fn join(f: &[String]) -> Vec<u8> {
    format!("{{{}}}\n", f.join(",")).into_bytes()
}

/// Bytes for decode, and the ticket and repo it is asked for.
fn near_record(r: &mut Rng) -> (Vec<u8>, String, String) {
    let (t, rp) = (id(r), id(r));
    let fr = r.below(5) as u8;
    let inf = fr != 4 && r.chance(2);
    let mut f = fields(&t, &rp, fr, inf);
    let bytes = match r.below(12) {
        0 | 1 => join(&f),
        2 => {
            for i in (1..f.len()).rev() {
                let j = r.below(i + 1);
                f.swap(i, j);
            }
            join(&f)
        }
        3 => {
            let wide: Vec<(usize, String)> = vec![
                (0, "\"v\":01".into()),
                (0, "\"v\":10".into()),
                (3, format!("\"fix_rounds\":0{fr}")),
                (3, format!("\"fix_rounds\":{fr}.0")),
                (3, format!("\"fix_roundss\":{fr}")),
                (4, format!("\"in_flight\":{inf} ")),
                (4, "\"in_flight\":TRUE".into()),
            ];
            let (i, s) = wide[r.below(wide.len())].clone();
            f[i] = s;
            join(&f)
        }
        4 => {
            let at = r.below(f.len() + 1);
            f.insert(at, "\"x\":1".into());
            join(&f)
        }
        5 => {
            let mut b = join(&f);
            let at = r.below(b.len() + 1);
            b.insert(at, r.pick(b" \t\n\r"));
            b
        }
        6 => join(&fields(&t, &rp, 5, false)),
        7 => join(&fields(&t, &rp, 4, true)),
        8 => {
            let odd: Vec<(usize, &str)> = vec![
                (3, "\"fix_rounds\":255"),
                (3, "\"fix_rounds\":256"),
                (3, "\"fix_rounds\":-1"),
                (3, "\"fix_rounds\":\"2\""),
                (3, "\"fix_rounds\":null"),
                (3, "\"fix_rounds\":1e0"),
                (3, "\"fix_rounds\":18446744073709551615"),
                (4, "\"in_flight\":1"),
                (4, "\"in_flight\":\"true\""),
                (4, "\"in_flight\":null"),
                (0, "\"v\":2"),
                (0, "\"v\":\"1\""),
            ];
            let (i, s) = odd[r.below(odd.len())];
            f[i] = s.to_string();
            join(&f)
        }
        9 => {
            let (t2, rp2) = if r.chance(2) { (format!("{t}x"), rp.clone()) } else { (t.clone(), format!("y{rp}")) };
            join(&fields(&t2, &rp2, fr, inf))
        }
        10 => {
            let mut b = join(&f);
            let at = r.below(b.len());
            match r.below(4) {
                0 => b[at] = r.next_u64() as u8,
                1 => {
                    b.remove(at);
                }
                2 => b.insert(at, r.next_u64() as u8),
                _ => b.truncate(at),
            }
            b
        }
        _ => {
            let n = r.below(80);
            (0..n).map(|_| if r.chance(2) { r.next_u64() as u8 } else { r.pick(b"{}\":,\n_vticketrpofxwnlsa0123456789") }).collect()
        }
    };
    (bytes, t, rp)
}

fn c1(r: &mut Rng) -> Result<(), Fail> {
    let (b, t, rp) = near_record(r);
    let x = catch_unwind(|| Record::decode(&b, &t, &rp));
    let y = catch_unwind(|| Record::decode(&b, &t, &rp));
    match (x, y) {
        (Ok(p), Ok(q)) if p == q => Ok(()),
        (Ok(_), Ok(_)) => Err((b, "two runs gave different answers".into())),
        _ => Err((b, "decode panicked".into())),
    }
}

static ACCEPTED: AtomicUsize = AtomicUsize::new(0);

fn c2(r: &mut Rng) -> Result<(), Fail> {
    let (b, t, rp) = near_record(r);
    let Ok(rec) = Record::decode(&b, &t, &rp) else { return Ok(()) };
    ACCEPTED.fetch_add(1, Ordering::Relaxed);
    if rec.ticket != t || rec.repo_id != rp {
        return Err((b, "the result holds another ticket or repo".into()));
    }
    if !Record::state_ok(rec.fix_rounds, rec.in_flight) {
        return Err((b, format!("accepted a state state_ok refuses: {rec:?}")));
    }
    if rec.encode().as_bytes() != b.as_slice() {
        return Err((b, "accepted bytes that are not the canonical encoding".into()));
    }
    Ok(())
}

fn c3(r: &mut Rng) -> Result<(), Fail> {
    let (t, rp) = (odd_text(r), odd_text(r));
    for fix_rounds in 0..=6u8 {
        for in_flight in [false, true] {
            if !Record::state_ok(fix_rounds, in_flight) {
                continue;
            }
            let rec = Record { ticket: t.clone(), repo_id: rp.clone(), fix_rounds, in_flight };
            let enc = rec.encode();
            match Record::decode(enc.as_bytes(), &t, &rp) {
                Ok(back) if back == rec => {}
                other => return Err((enc.into_bytes(), format!("the round trip gave {other:?}"))),
            }
        }
    }
    Ok(())
}

#[test]
fn words_only_raw_bytes_do_not_panic() {
    run_all(w1);
}

#[test]
fn words_only_comment_free_text_is_unchanged() {
    run_all(w2);
}

#[test]
fn words_only_plain_line_comments_are_removed() {
    run_all(w3);
}

#[test]
fn caps_reader_raw_bytes_do_not_panic() {
    run_all(c1);
}

#[test]
fn caps_reader_accepts_canonical_bytes_only() {
    ACCEPTED.store(0, Ordering::Relaxed);
    run_all(c2);
    assert!(ACCEPTED.load(Ordering::Relaxed) > 100, "the inputs held too few canonical records");
}

#[test]
fn caps_record_round_trips() {
    run_all(c3);
}

#[test]
fn the_seed_list_gives_the_same_inputs_twice() {
    let mut sorted = SEEDS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), SEEDS.len(), "a seed is listed twice");
    for &seed in &SEEDS {
        for case in 0..CASES {
            let (mut a, mut b) = (Rng::for_case(seed, case), Rng::for_case(seed, case));
            assert_eq!(raw_bytes(&mut a), raw_bytes(&mut b), "raw bytes, seed={seed:#x} case={case}");
            assert_eq!(w2_text(&mut a), w2_text(&mut b), "w2 text, seed={seed:#x} case={case}");
            assert_eq!(w3_texts(&mut a), w3_texts(&mut b), "w3 text, seed={seed:#x} case={case}");
            assert_eq!(near_record(&mut a), near_record(&mut b), "near record, seed={seed:#x} case={case}");
        }
    }
}
