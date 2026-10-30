//! The fixture corpus against the engine, with the synthetic list text injected (no real list is read).
use harness::detect::classes_for_line;
use harness::fixtures;
use harness::overlay;
use std::process::Command;

fn list() -> overlay::List {
    overlay::parse(&fixtures::synthetic_list()).unwrap()
}

#[test]
fn thirteen_cases_each_report_their_class() {
    let cases = fixtures::cases();
    assert_eq!(cases.len(), 13);
    let l = list();
    for c in &cases {
        let got = classes_for_line(&c.line, &l);
        assert!(got.contains(&c.class), "case {} did not report its class", c.name);
        assert!(c.line.contains(&c.marker), "case {} marker not in its line", c.name);
    }
}

#[test]
fn eight_refusing_classes_are_covered() {
    let mut seen: Vec<&str> = fixtures::cases().iter().map(|c| c.class).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 8);
    assert!(!seen.contains(&"synthetic-test"));
}

#[test]
fn deny_token_planted_twice_and_home_path_five_forms() {
    let cases = fixtures::cases();
    assert_eq!(cases.iter().filter(|c| c.class == "deny-token").count(), 2);
    assert_eq!(cases.iter().filter(|c| c.class == "home-path").count(), 5);
}

#[test]
fn near_misses_stay_clean() {
    let l = list();
    for line in fixtures::near_misses() {
        assert!(classes_for_line(&line, &l).is_empty());
    }
}

#[test]
fn synthetic_marker_has_its_class() {
    assert!(classes_for_line(&fixtures::synthetic_line(), &list()).contains(&"synthetic-test"));
}

#[test]
fn extras_exist() {
    for n in ["addr", "session-trailer", "coauthor-trailer", "synthetic"] {
        assert!(fixtures::extra(n).is_some());
    }
    assert!(fixtures::extra("nope").is_none());
}

#[test]
fn fixtures_subcommand_prints_material() {
    let bin = env!("CARGO_BIN_EXE_harness");
    let o = Command::new(bin).args(["fixtures", "names"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().count(), 13);
    let o = Command::new(bin).args(["fixtures", "class", "email"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "email");
    let o = Command::new(bin).args(["fixtures", "line", "no-such-case"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}
