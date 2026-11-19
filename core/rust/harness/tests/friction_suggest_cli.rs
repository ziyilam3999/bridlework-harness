//! Ask A32: `harness friction suggest` — free BM25 ranking of the inbox groups for a row not yet stored. The real
//! binary runs as a child on a scratch inbox. Every row and needle is synthetic; every privacy needle is assembled
//! from fragments at run time.
#![cfg(unix)]
mod friction_common;
use friction_common::*;
use std::fs;
use std::os::unix::fs::symlink;

fn suggest(t: &Tmp, extra: &[&str]) -> std::process::Output {
    let cfg = t.config();
    let mut args: Vec<&str> = vec!["friction", "suggest"];
    args.extend_from_slice(extra);
    args.push("--config");
    args.push(p(&cfg));
    run(&args)
}

fn lines(o: &std::process::Output) -> Vec<String> {
    out(o).lines().map(|l| l.to_string()).collect()
}

fn plain(id: &str, breaks: &str, text: &str) -> String {
    row(id, "2026-11-01", breaks, text, &[])
}

/// The T1 inbox. Query tokens (text "cache warm zeta issue-9", why "warm cache") are the DISTINCT {cache, warm,
/// zeta}; `issue-9` is stripped. N = 7; document lengths 1,1,2,2,2,3,3 so avg = 2.0.
/// The `c-1` text is `zeta f-20991231-01zeta issue-5 closed`: with the ids stripped it is the tokens zeta, zeta,
/// closed (tf(zeta) = 2, len 3); an id left in would glue `01zeta` and add `issue`.
fn t1_rows() -> Vec<String> {
    vec![
        plain("a-1", "", "warm"),
        plain("b-1", "", "cache"),
        plain("b-2", "b-1", "cache misses"),
        plain("b-3", "b-2", "cache problems"),
        plain("b-4", "b-3", "cache again"),
        plain("c-1", "", "zeta f-20991231-01zeta issue-5 closed"),
        plain("d-1", "", "unrelated words here"),
    ]
}

const T1_ARGS: [&str; 4] = ["--text", "cache warm zeta issue-9", "--why", "warm cache"];

#[test]
fn t1_top3_exact_order_and_scores() {
    // Hand computation. df(cache) = 4, df(warm) = df(zeta) = 1, N = 7, avg = 2.
    //   idf(cache) = ln(1 + 3.5/4.5) = ln(16/9) = 0.575364;  idf(warm) = idf(zeta) = ln(1 + 6.5/1.5) = ln(16/3) = 1.673976
    //   k(len) = 1.2 * (0.25 + 0.75 * len/2): len1 = 0.75, len2 = 1.2, len3 = 1.65
    //   a-1  (len1, warm tf1)  : 1.673976 * 2.2/(1+0.75)   = 2.104427 -> 2.10
    //   c-1  (len3, zeta tf2)  : 1.673976 * 4.4/(2+1.65)   = 2.017943 -> 2.02
    //   b-1  (len1, cache tf1) : 0.575364 * 2.2/(1+0.75)   = 0.723315 -> 0.72   (b-2..b-4: 0.575364 * 2.2/2.2 = 0.58)
    // Group B under SUM would be 0.7233 + 3 * 0.5754 = 2.4495, above both others: max keeps it third.
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let o = suggest(&t, &T1_ARGS);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "suggest: rank=1 group=a-1 row=a-1 score=2.10 via=bm25\n\
         suggest: rank=2 group=c-1 row=c-1 score=2.02 via=bm25\n\
         suggest: rank=3 group=b-1 row=b-1 score=0.72 via=bm25\n"
    );
}

#[test]
fn chain_three_deep_is_suggested_under_its_root_id() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let o = suggest(&t, &["--text", "cache", "--top", "9"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let l = lines(&o);
    assert!(l.iter().any(|x| x.starts_with("suggest: rank=1 group=b-1 row=b-1 ")), "{l:?}");
    assert!(!out(&o).contains("group=b-2") && !out(&o).contains("group=b-3") && !out(&o).contains("group=b-4"), "{l:?}");
}

#[test]
fn the_group_id_is_the_root_while_row_is_the_best_matching_member() {
    let t = Tmp::new();
    t.write_inbox(&[plain("x-1", "", "unrelated start"), plain("x-2", "x-1", "rarity token"), plain("y-1", "", "filler words")]);
    let o = suggest(&t, &["--text", "rarity", "--top", "1"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(out(&o).starts_with("suggest: rank=1 group=x-1 row=x-2 score="), "{}", out(&o));
}

#[test]
fn an_event_rooted_group_is_suggested_under_its_signature_with_a_real_row_id() {
    // N = 3, lens 2,2,1, avg 5/3, df(event) = 2: ln(1.6) * 2.2/(1 + 1.2 * (0.25 + 0.75 * 1.2)) = 0.470004 * 0.924370 = 0.43
    let t = Tmp::new();
    let sig = "evt.gate_fail.3_abc";
    t.write_inbox(&[event(sig, "2026-w44", "2026-10-28", ""), event(sig, "2026-w45", "2026-11-04", ""), hand("h-1", "2026-11-05", "", "")]);
    let o = suggest(&t, &["--text", "event"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        format!("suggest: rank=1 group={sig} row={sig}.2026-w44 score=0.43 via=bm25\nsuggest: rank=2 group=h-1 row=h-1 score=0.00 via=bm25\n")
    );
}

#[test]
fn t6_a_tie_goes_to_the_group_seen_first_in_inbox_order() {
    for (first, second) in [("z-1", "a-1"), ("a-1", "z-1")] {
        let t = Tmp::new();
        t.write_inbox(&[plain(first, "", "tie word"), plain(second, "", "tie word"), plain("o-1", "", "other thing")]);
        let o = suggest(&t, &["--text", "tie"]);
        assert_eq!(code(&o), 0, "{}", err(&o));
        let l = lines(&o);
        assert!(l[0].starts_with(&format!("suggest: rank=1 group={first} row={first} score=")), "{l:?}");
        assert!(l[1].starts_with(&format!("suggest: rank=2 group={second} row={second} score=")), "{l:?}");
        assert_eq!(l[0][l[0].find("score=").unwrap()..], l[1][l[1].find("score=").unwrap()..], "a true tie");
        let again = suggest(&t, &["--text", "tie"]);
        assert_eq!(o.stdout, again.stdout, "the same input twice is byte-identical");
    }
}

#[test]
fn before_ranks_only_the_earlier_rows_and_takes_the_statistics_over_them() {
    // Rows before c-1: a-1, b-1..b-4. N = 5, lens 1,1,2,2,2, avg 1.6, df(cache) = 4, df(warm) = 1, df(zeta) = 0.
    //   idf(cache) = ln(1 + 1.5/4.5) = 0.287682; idf(warm) = ln(1 + 4.5/1.5) = ln 4 = 1.386294
    //   k(len1) = 1.2 * (0.25 + 0.75 * 0.625) = 0.8625; k(len2) = 1.2 * (0.25 + 0.75 * 1.25) = 1.425
    //   a-1: 1.386294 * 2.2/1.8625 = 1.637508 -> 1.64;  b-1: 0.287682 * 1.181208 = 0.339812 -> 0.34
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let mut args = T1_ARGS.to_vec();
    args.extend(["--before", "c-1"]);
    let o = suggest(&t, &args);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "suggest: rank=1 group=a-1 row=a-1 score=1.64 via=bm25\nsuggest: rank=2 group=b-1 row=b-1 score=0.34 via=bm25\n");
}

#[test]
fn before_the_first_row_is_none_and_an_unknown_row_refuses() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let o = suggest(&t, &["--text", "cache", "--before", "a-1"]);
    assert_eq!((code(&o), out(&o)), (0, "suggest: none\n".to_string()), "{}", err(&o));
    let o = suggest(&t, &["--text", "cache", "--before", "no-such-row"]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("--before"), "{}", err(&o));
    assert!(!err(&o).contains("no-such-row"), "{}", err(&o));
}

#[test]
fn an_empty_inbox_is_none_and_the_usage_line_names_the_verb() {
    let t = Tmp::new();
    t.write_inbox(&[]);
    let o = suggest(&t, &["--text", "anything"]);
    assert_eq!((code(&o), out(&o)), (0, "suggest: none\n".to_string()), "{}", err(&o));
    let o = run(&["friction"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("friction suggest --text <t> [--why <w>] [--top <k>] [--before <row id>]"), "{}", err(&o));
}

#[test]
fn top_limits_the_listing_and_defaults_to_three() {
    let t = Tmp::new();
    let rows: Vec<String> = (1..=5).map(|i| plain(&format!("r-{i}"), "", "common words")).collect();
    t.write_inbox(&rows);
    assert_eq!(lines(&suggest(&t, &["--text", "common"])).len(), 3);
    assert_eq!(lines(&suggest(&t, &["--text", "common", "--top", "5"])).len(), 5);
    assert_eq!(lines(&suggest(&t, &["--text", "common", "--top", "1"])).len(), 1);
    assert_eq!(code(&suggest(&t, &["--text", "common", "--top", "0"])), 2);
}

#[test]
fn a_privacy_hit_in_the_query_refuses_and_names_the_field_never_the_bytes() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let before = fs::read(t.inbox()).unwrap();
    let home = ["/Us", "ers/", "zztester", "/proj"].concat();
    let cred = ["AK", "IA", "ZZZZ", "ZZZZ", "ZZZZ", "ZZZZ"].concat();
    for (flag, needle, class) in [
        ("--text", home.as_str(), "home-path"),
        ("--text", cred.as_str(), "credential"),
        ("--why", home.as_str(), "home-path"),
        ("--why", cred.as_str(), "credential"),
        ("--why", DENY, "deny-token"),
    ] {
        let value = format!("cache {needle} here");
        let args: Vec<&str> = if flag == "--text" { vec!["--text", &value] } else { vec!["--text", "cache", "--why", &value] };
        let o = suggest(&t, &args);
        assert_eq!(code(&o), 1, "{flag}: {}", err(&o));
        assert!(out(&o).is_empty(), "stdout must be empty: {}", out(&o));
        let field = flag.trim_start_matches("--");
        assert!(err(&o).contains(&format!("field {field} has a privacy hit ({class})")), "{}", err(&o));
        assert!(!err(&o).contains(needle) && !err(&o).contains(&value), "the bytes must not be echoed: {}", err(&o));
    }
    assert_eq!(fs::read(t.inbox()).unwrap(), before);
}

#[test]
fn a_damaged_inbox_refuses_exit_1_and_no_privacy_list_refuses_exit_2() {
    let t = Tmp::new();
    let good = plain("a-1", "", "cache");
    let cases: Vec<(&str, String)> = vec![
        ("torn tail", format!("{good}\n{{\"id\":\"a-2\"")),
        ("bad row", format!("{good}\nnot json at all\n")),
        ("privacy hit in a stored row", format!("{good}\n{}\n", plain("a-2", "", &format!("has {DENY} inside")))),
    ];
    for (what, body) in cases {
        fs::write(t.inbox(), &body).unwrap();
        let o = suggest(&t, &["--text", "cache"]);
        assert_eq!(code(&o), 1, "{what}: {}", err(&o));
        assert!(out(&o).is_empty(), "{what}");
        assert_eq!(fs::read(t.inbox()).unwrap(), body.as_bytes(), "{what}: the inbox is byte-identical");
    }
    t.write_inbox(&[good]);
    let cfg = t.config();
    let mut c = base();
    c.args(["friction", "suggest", "--text", "cache", "--config", p(&cfg)]).stdin(std::process::Stdio::null());
    let o = c.output().unwrap();
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("no privacy list"), "{}", err(&o));
}

#[test]
fn record_appends_one_fixed_key_line_without_any_row_text() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let rec = t.0.join("suggest.jsonl");
    let unique = ["qx", "uniq", "word", "zz"].concat();
    let text = format!("cache {unique}");
    let o = suggest(&t, &["--text", &text, "--record", p(&rec)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let _ = suggest(&t, &["--text", &text, "--why", "second", "--record", p(&rec)]);
    let body = fs::read_to_string(&rec).unwrap();
    assert_eq!(body.lines().count(), 2, "{body}");
    assert!(!body.contains(&unique) && !body.contains("cache") && !body.contains("second"), "{body}");
    let first = body.lines().next().unwrap();
    assert!(first.starts_with("{\"ts\":\"20") && first.contains("\"query_sha256\":\"") && first.contains("\"top\":[\"b-1\"") && first.ends_with("\"via\":\"bm25\"}"), "{first}");
    let hashes: Vec<&str> = body.lines().map(|l| l.split("\"query_sha256\":\"").nth(1).unwrap().split('"').next().unwrap()).collect();
    assert!(hashes.iter().all(|h| h.len() == 64 && h.bytes().all(|c| c.is_ascii_hexdigit())), "{hashes:?}");
    assert_ne!(hashes[0], hashes[1]);
    // sha256 of text + newline + why: "abc" + "\n" + "" is the bytes of `echo abc`.
    let rec2 = t.0.join("known.jsonl");
    let o = suggest(&t, &["--text", "abc", "--record", p(&rec2)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(fs::read_to_string(&rec2).unwrap().contains("edeaaff3f1774ad2888673770c6d64097e391bc362d7d6fb34982ddf0efd18cb"));
}

#[test]
fn record_naming_the_inbox_is_refused_exit_2_even_through_a_link() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let before = fs::read(t.inbox()).unwrap();
    let link = t.0.join("link.jsonl");
    symlink(t.inbox(), &link).unwrap();
    for path in [t.inbox(), link] {
        let o = suggest(&t, &["--text", "cache", "--record", p(&path)]);
        assert_eq!(code(&o), 2, "{}", err(&o));
        assert!(out(&o).is_empty());
        assert!(err(&o).contains("--record"), "{}", err(&o));
        assert_eq!(fs::read(t.inbox()).unwrap(), before);
    }
}

#[test]
fn record_naming_the_inbox_through_a_hard_link_is_refused_exit_2() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let before = fs::read(t.inbox()).unwrap();
    let hard = t.0.join("hard.jsonl");
    fs::hard_link(t.inbox(), &hard).unwrap();
    let o = suggest(&t, &["--text", "cache", "--record", p(&hard)]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("--record"), "{}", err(&o));
    assert_eq!(fs::read(t.inbox()).unwrap(), before, "the inbox is byte-identical");
    assert_eq!(fs::read(&hard).unwrap(), before, "the hard link shows the same bytes");
}

#[test]
fn rank_record_naming_the_inbox_through_a_hard_link_is_refused_exit_2() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let before = fs::read(t.inbox()).unwrap();
    let hard = t.0.join("hard-rank.jsonl");
    fs::hard_link(t.inbox(), &hard).unwrap();
    let cfg = t.config();
    let o = run(&["friction", "rank", "--record", p(&hard), "--config", p(&cfg)]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert_eq!(fs::read(t.inbox()).unwrap(), before, "the inbox is byte-identical");
}

#[test]
fn an_empty_or_blank_text_is_a_usage_error() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let before = fs::read(t.inbox()).unwrap();
    for text in ["", "   "] {
        let o = suggest(&t, &["--text", text, "--why", "cache"]);
        assert_eq!(code(&o), 2, "{text:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert!(err(&o).contains("--text must not be empty"), "{}", err(&o));
    }
    assert_eq!(fs::read(t.inbox()).unwrap(), before);
}

#[test]
fn the_inbox_is_byte_identical_after_success_and_after_refusals() {
    let t = Tmp::new();
    t.write_inbox(&t1_rows());
    let before = fs::read(t.inbox()).unwrap();
    for args in [
        vec!["--text", "cache"],
        vec!["--text", "cache", "--before", "c-1"],
        vec!["--text", "cache", "--before", "nope"],
        vec!["--text", "cache", "--top", "x"],
        vec!["--why", "no text"],
    ] {
        let _ = suggest(&t, &args);
        assert_eq!(fs::read(t.inbox()).unwrap(), before, "{args:?}");
    }
}

#[test]
fn tokens_follow_the_spec() {
    use harness::friction_suggest::tokens;
    assert_eq!(tokens("Retry f-20991231-01 issue-42 #77 ok 123 a1b Backoff-2 x"), vec!["retry", "a1b", "backoff"]);
    assert_eq!(tokens("f-20991231-01zeta"), vec!["zeta"]);
}
