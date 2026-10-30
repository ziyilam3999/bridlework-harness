//! Synthetic fixture generators. Every marker is assembled at call time from fragments and is never stored whole.
//! Generators take no input: no environment, no file, no list source is read here.

pub struct Case {
    pub name: &'static str,
    pub class: &'static str,
    /// The sensitive substring; it must never appear in any output line.
    pub marker: String,
    /// A line of text that carries the marker.
    pub line: String,
}

fn j(parts: &[&str]) -> String {
    parts.concat()
}

fn case(name: &'static str, class: &'static str, marker: String, line: String) -> Case {
    Case { name, class, marker, line }
}

/// The synthetic list used with these cases (two deny-token kinds and one tagged regulated line).
pub fn synthetic_list() -> String {
    format!(
        "# synthetic fixture list\n{}\n{}\nregulated:{}\n",
        j(&["zzprofile", "namezz"]),
        j(&["zzemployer", "patternzz"]),
        j(&["zzreg", "idzz"])
    )
}

fn addr() -> String {
    j(&["fixture.user", "@", "example", ".invalid"])
}

/// The 13 planted cases. `deny-token` appears twice (profile-name shaped, employer-pattern shaped).
pub fn cases() -> Vec<Case> {
    let user = "fixtureuser";
    let profile = j(&["zzprofile", "namezz"]);
    let employer = j(&["zzemployer", "patternzz"]);
    let regulated = j(&["zzreg", "idzz"]);
    let raw = j(&["/Us", "ers/", user, "/work"]);
    let pct = j(&["%2FUs", "ers%2F", user, "%2Fwork"]);
    let esc = j(&["\\/Us", "ers\\/", user, "\\/work"]);
    let dash = j(&["-Us", "ers-", user, "-work"]);
    let under = j(&["_Us", "ers_", user, "_work"]);
    let cred = j(&["AK", "IA", "FIXTUREKEY123456"]);
    let link = j(&["https://clau", "de.ai/code/", "session_FIXTURE01"]);
    let trailer = j(&["Co-authored", "-by: Fixture <", &addr(), ">"]);
    let metric = j(&["base", "line.", "fixture_metric: 4242"]);
    vec![
        case("deny-profile", "deny-token", profile.clone(), format!("owner: {}", profile)),
        case("deny-employer", "deny-token", employer.clone(), format!("worked at {}", employer)),
        case("email", "email", addr(), format!("contact: {}", addr())),
        case("home-raw", "home-path", raw.clone(), format!("path {}", raw)),
        case("home-percent", "home-path", pct.clone(), format!("path {}", pct)),
        case("home-escaped", "home-path", esc.clone(), format!("path {}", esc)),
        case("home-dash", "home-path", dash.clone(), format!("dir {}", dash)),
        case("home-underscore", "home-path", under.clone(), format!("dir {}", under)),
        case("credential", "credential", cred.clone(), format!("access = {}", cred)),
        case("session-link", "session-link", link.clone(), format!("see {}", link)),
        case("coauthor", "coauthor-personal", addr(), trailer),
        case("regulated", "regulated-id", regulated.clone(), format!("ref {}", regulated)),
        case("baseline", "baseline-number", j(&["fixture", "_metric"]), metric),
    ]
}

/// Lines that must stay clean: a number with no key, and a key with no number.
pub fn near_misses() -> Vec<String> {
    vec![
        j(&["fixture", "_metric: 4242 recorded"]),
        j(&["base", "line.", "fixture_metric: n/a"]),
    ]
}

/// A synthetic-class marker line (admitted only in test mode).
pub fn synthetic_line() -> String {
    j(&["note ", "Synthetic", "-Test", "-Marker"])
}

/// Extra named pieces for commit-metadata tests.
pub fn extra(name: &str) -> Option<String> {
    match name {
        "addr" => Some(addr()),
        "session-trailer" => Some(j(&["Claude-", "Session: fixture"])),
        "coauthor-trailer" => Some(j(&["Co-authored", "-by: Fixture <", &addr(), ">"])),
        "synthetic" => Some(synthetic_line()),
        _ => None,
    }
}
