//! `schemas/resource-manifest.schema.json` mirrors the manifest loader's exact key set per shape (same idea as the event,
//! gate and ticket schema tests); this pins the two together, then exercises the library: line codec, the fence, the state
//! fold, damage handling and the pid identity read.
use harness::manifest::{
    apply, owns, parse_manifest, process_start_ms, read_manifest, Class, Line, ManifestError, Op, Query, Target, LINE_CAP,
    SCHEMA_VERSION,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

// ---------------------------------------------------------------- scratch

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// A scratch folder outside the repo, removed on drop.
struct Tmp(PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!(
            "manifest-core-{}-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst),
            nanos
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    fn sub(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn manifest_dir(&self) -> PathBuf {
        self.0.join("man")
    }

    fn bytes(&self, ticket: &str) -> Vec<u8> {
        std::fs::read(self.manifest_dir().join(format!("{ticket}.jsonl"))).unwrap()
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ts(n: u64) -> u64 {
    n
}

// ---------------------------------------------------------------- samples

/// One sample of every shape: root, the four path-class adds, process, port, remove.
fn samples() -> Vec<Line> {
    vec![
        Line::Root { ticket: "t-1".into(), path: "/scratch/roots/r".into(), ts_ms: ts(7) },
        Line::AddPath { ticket: "t-1".into(), id: "w-1".into(), class: Class::Worktree, path: "/scratch/roots/r/w".into(), ts_ms: ts(8) },
        Line::AddPath { ticket: "t-1".into(), id: "d-1".into(), class: Class::TempDir, path: "/scratch/roots/r/d".into(), ts_ms: ts(8) },
        Line::AddPath { ticket: "t-1".into(), id: "l-1".into(), class: Class::Log, path: "/scratch/roots/r/l".into(), ts_ms: ts(8) },
        Line::AddPath { ticket: "t-1".into(), id: "e-1".into(), class: Class::Evidence, path: "/scratch/roots/r/e".into(), ts_ms: ts(8) },
        Line::AddProcess { ticket: "t-1".into(), id: "p-1".into(), pid: 4242, pid_start_ms: 1730000000000, ts_ms: ts(9) },
        Line::AddPort { ticket: "t-1".into(), id: "n-1".into(), port: 8080, ts_ms: ts(10) },
        Line::Remove { ticket: "t-1".into(), id: "w-1".into(), ts_ms: ts(11) },
    ]
}

// ---------------------------------------------------------------- schema pin

fn required_in(section: &str) -> Result<Vec<String>, String> {
    let at = section.find("\"required\"").ok_or("no required array")?;
    let open = at + section[at..].find('[').ok_or("required is not an array")?;
    let close = open + section[open..].find(']').ok_or("required array is not closed")?;
    Ok(section[open + 1..close].split(',').map(|s| s.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty()).collect())
}

/// The top-level key names inside a branch's `properties` object (brace-depth scan; strings here never hold braces).
fn property_keys(section: &str) -> Result<Vec<String>, String> {
    let at = section.find("\"properties\"").ok_or("no properties object")?;
    let open = at + section[at..].find('{').ok_or("properties is not an object")?;
    let (mut depth, mut keys, mut cur, mut in_str) = (0usize, Vec::new(), String::new(), false);
    for (i, c) in section[open..].char_indices() {
        if in_str {
            if c == '"' {
                in_str = false;
                if depth == 1 && section[open + i + 1..].trim_start().starts_with(':') {
                    keys.push(cur.clone());
                }
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                cur.clear();
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(keys);
                }
            }
            _ => {}
        }
    }
    Err("properties object is not closed".to_string())
}

/// Err names the first way the schema text disagrees with the loader.
fn check(schema: &str) -> Result<(), String> {
    let marker = "\"title\": \"";
    let shapes = samples();
    if schema.matches(marker).count() != shapes.len() + 1 {
        return Err(format!("expected {} titles (the schema and one per shape), found {}", shapes.len() + 1, schema.matches(marker).count()));
    }
    if !schema.contains(&format!("{LINE_CAP} bytes")) {
        return Err("the schema does not state the line cap".to_string());
    }
    for s in &shapes {
        let name = s.shape_id();
        let start = schema.find(&format!("{marker}{name}\"")).ok_or(format!("no branch for shape {name}"))?;
        let end = schema[start + 1..].find(marker).map(|i| start + 1 + i).unwrap_or(schema.len());
        let section = &schema[start..end];
        let mut got = required_in(section).map_err(|e| format!("{name}: {e}"))?;
        let mut want: Vec<String> = s.keys().iter().map(|k| k.to_string()).collect();
        got.sort();
        want.sort();
        if got != want {
            return Err(format!("{name}: required set {got:?} differs from the loader's {want:?}"));
        }
        // Both directions: a property the loader does not know (an extra optional key the schema would allow) is drift too.
        let mut props = property_keys(section).map_err(|e| format!("{name}: {e}"))?;
        props.sort();
        if props != want {
            return Err(format!("{name}: property set {props:?} differs from the loader's {want:?}"));
        }
        for key in &want {
            if !section.contains(&format!("\"{key}\":")) {
                return Err(format!("{name}: required key {key} has no property"));
            }
        }
        if !section.contains(&format!("\"op\": {{ \"const\": \"{}\" }}", s.op())) {
            return Err(format!("{name}: the op const does not match the branch"));
        }
        if let Line::AddPath { .. } | Line::AddProcess { .. } | Line::AddPort { .. } = s {
            let class = match s {
                Line::AddPath { class, .. } => class.as_str(),
                Line::AddProcess { .. } => "process",
                _ => "port",
            };
            if !section.contains(&format!("\"class\": {{ \"const\": \"{class}\" }}")) {
                return Err(format!("{name}: the class const does not match the branch"));
            }
        }
        if !section.contains(&format!("\"v\": {{ \"const\": {SCHEMA_VERSION},")) {
            return Err(format!("{name}: the v const is not {SCHEMA_VERSION}"));
        }
        if !section.contains("\"additionalProperties\": false") {
            return Err(format!("{name}: extra keys are not refused"));
        }
    }
    let proc = branch(schema, "add_process");
    if !proc.contains("\"minimum\": 2") {
        return Err("add_process: pid minimum is not 2".to_string());
    }
    if !proc.contains("\"pid_start_ms\"") {
        return Err("add_process: no pid_start_ms property".to_string());
    }
    let port = branch(schema, "add_port");
    if !port.contains("\"minimum\": 1") || !port.contains("\"maximum\": 65535") {
        return Err("add_port: the port bounds are not 1..65535".to_string());
    }
    Ok(())
}

fn branch<'a>(schema: &'a str, name: &str) -> &'a str {
    let marker = "\"title\": \"";
    let start = schema.find(&format!("{marker}{name}\"")).unwrap();
    let end = schema[start + 1..].find(marker).map(|i| start + 1 + i).unwrap_or(schema.len());
    &schema[start..end]
}

fn schema_text() -> String {
    std::fs::read_to_string(repo().join("schemas/resource-manifest.schema.json")).expect("the schema is committed and utf-8")
}

#[test]
fn the_committed_schema_file_matches_the_manifest_loader() {
    check(&schema_text()).unwrap();
}

#[test]
fn planted_a_schema_that_drifted_from_the_loader_is_caught() {
    let good = schema_text();
    let first_required = "\"required\": [\"v\", \"op\", \"ticket\", \"id\", \"class\", \"path\", \"ts_ms\"]";
    assert!(good.contains(first_required));
    let mutants = [
        ("a key removed from one shape", good.replacen(first_required, "\"required\": [\"v\", \"op\", \"ticket\", \"id\", \"class\", \"path\"]", 1)),
        ("an extra required key", good.replacen(first_required, "\"required\": [\"v\", \"op\", \"ticket\", \"id\", \"class\", \"path\", \"ts_ms\", \"extra\"]", 1)),
        ("a wrong v const", good.replacen("\"v\": { \"const\": 1,", "\"v\": { \"const\": 2,", 1)),
        ("an op const that no longer matches", good.replacen("\"op\": { \"const\": \"remove\" }", "\"op\": { \"const\": \"rm\" }", 1)),
        ("a class const that no longer matches", good.replacen("\"class\": { \"const\": \"temp_dir\" }", "\"class\": { \"const\": \"temp\" }", 1)),
        ("extra keys allowed", good.replacen("\"additionalProperties\": false", "\"additionalProperties\": true", 1)),
        ("a shape branch missing", good.replacen("\"title\": \"add_process\"", "\"title\": \"add_proc\"", 1)),
        ("an extra optional property the loader does not know", good.replacen("\"path\": { \"type\": \"string\", \"pattern\": \"^/.+$\", \"description", "\"note\": { \"type\": \"string\" },\n        \"path\": { \"type\": \"string\", \"pattern\": \"^/.+$\", \"description", 1)),
        ("a stale line cap", good.replace("4096 bytes", "8192 bytes")),
        ("the pid minimum relaxed", good.replacen("\"minimum\": 2,", "\"minimum\": 0,", 1)),
        ("the port maximum relaxed", good.replacen("\"maximum\": 65535", "\"maximum\": 70000", 1)),
    ];
    for (name, m) in mutants {
        assert!(m != good, "{name}: the mutation changed nothing");
        assert!(check(&m).is_err(), "{name} must be caught");
    }
}

#[test]
fn the_doc_names_every_shape_class_and_the_cap() {
    let doc = std::fs::read_to_string(repo().join("docs/resource-manifest.md")).expect("the doc is committed and utf-8");
    for c in Class::ALL {
        assert!(doc.contains(&format!("`{}`", c.as_str())), "doc names {}", c.as_str());
    }
    for op in ["root", "add", "remove"] {
        assert!(doc.contains(&format!("`{op}`")), "doc names op {op}");
    }
    assert!(doc.contains(&LINE_CAP.to_string()));
    assert!(doc.contains("torn tail") && doc.contains("pid_start_ms"));
    // N4 (ruling): the list line of a process entry has FOUR tab-separated fields, every other class three.
    assert!(doc.contains("four tab-separated fields"), "the doc states the four-field process list line");
    assert!(doc.contains("exactly three fields"), "the doc states the three-field non-process list line");
    // N3: off Linux the start time jitters; the doc must say what tolerance a comparer needs.
    assert!(doc.contains("2000 ms"), "the doc states the off-Linux pid_start_ms tolerance");
    // INPUT-BOUND: the doc states the file cap.
    assert!(doc.contains("1 MiB"), "the doc states the 1 MiB file cap");
}

// ---------------------------------------------------------------- line codec

#[test]
fn every_shape_round_trips_byte_identical() {
    for s in samples() {
        let enc = s.encode().unwrap();
        let back = Line::decode(enc.trim_end_matches('\n')).unwrap();
        assert_eq!(back, s, "{} round-trips", s.shape_id());
        assert_eq!(back.encode().unwrap(), enc, "{} re-encodes byte-identical", s.shape_id());
    }
}

#[test]
fn encoded_lines_carry_the_exact_fixed_key_order() {
    assert_eq!(
        Line::Root { ticket: "t-1".into(), path: "/scratch/r".into(), ts_ms: 7 }.encode().unwrap(),
        "{\"v\":1,\"op\":\"root\",\"ticket\":\"t-1\",\"path\":\"/scratch/r\",\"ts_ms\":7}\n"
    );
    assert_eq!(
        Line::AddPath { ticket: "t-1".into(), id: "w-1".into(), class: Class::Worktree, path: "/scratch/r/w".into(), ts_ms: 8 }.encode().unwrap(),
        "{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"w-1\",\"class\":\"worktree\",\"path\":\"/scratch/r/w\",\"ts_ms\":8}\n"
    );
    assert_eq!(
        Line::AddProcess { ticket: "t-1".into(), id: "p-1".into(), pid: 42, pid_start_ms: 1730000000000, ts_ms: 9 }.encode().unwrap(),
        "{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"p-1\",\"class\":\"process\",\"pid\":42,\"pid_start_ms\":1730000000000,\"ts_ms\":9}\n"
    );
    assert_eq!(
        Line::AddPort { ticket: "t-1".into(), id: "n-1".into(), port: 8080, ts_ms: 10 }.encode().unwrap(),
        "{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"n-1\",\"class\":\"port\",\"port\":8080,\"ts_ms\":10}\n"
    );
    assert_eq!(
        Line::Remove { ticket: "t-1".into(), id: "w-1".into(), ts_ms: 11 }.encode().unwrap(),
        "{\"v\":1,\"op\":\"remove\",\"ticket\":\"t-1\",\"id\":\"w-1\",\"ts_ms\":11}\n"
    );
}

/// Cut the `,"key":<value>` segment out of an encoded line (no encoded value in the samples holds a comma).
fn drop_key(line: &str, key: &str) -> Option<String> {
    let pat = format!(",\"{key}\":");
    let start = line.find(&pat)?;
    let end = line[start + 1..].find(',').map(|i| start + 1 + i).unwrap_or(line.len() - 1);
    Some(format!("{}{}", &line[..start], &line[end..]))
}

#[test]
fn a_missing_extra_or_duplicate_key_is_refused_on_every_shape() {
    for s in samples() {
        let good = s.encode().unwrap();
        let good = good.trim_end_matches('\n').to_string();
        assert!(Line::decode(&good).is_ok(), "{}: twin, the untouched line decodes", s.shape_id());
        for key in s.keys() {
            let Some(bad) = drop_key(&good, key) else { continue };
            assert!(bad != good, "{}: dropping {key} changed nothing", s.shape_id());
            let err = Line::decode(&bad).unwrap_err();
            assert!(err.contains("missing"), "{} dropping {key}: {err}", s.shape_id());
        }
        let extra = good.replacen("}", ",\"extra\":\"x\"}", 1);
        assert!(Line::decode(&extra).unwrap_err().contains("unexpected key"), "{}: extra key", s.shape_id());
        let dup = good.replacen("{\"v\":1,", "{\"v\":1,\"v\":1,", 1);
        assert!(Line::decode(&dup).unwrap_err().contains("duplicate key"), "{}: duplicate key", s.shape_id());
    }
}

#[test]
fn an_unknown_version_op_or_class_is_refused_and_names_the_culprit() {
    let g = samples()[1].encode().unwrap(); // add worktree
    let g = g.trim_end_matches('\n');
    for (bad, want) in [
        (g.replace("\"v\":1", "\"v\":2"), "unsupported v 2"),
        (g.replacen("\"v\":1,", "", 1), "v missing"),
        (g.replace("\"v\":1", "\"v\":\"1\""), "v has the wrong type"),
        (g.replace("\"op\":\"add\"", "\"op\":\"clear\""), "unknown op"),
        (g.replace("\"class\":\"worktree\"", "\"class\":\"container\""), "unknown class"),
        (g.replace("\"class\":\"worktree\"", "\"class\":\"process\""), "unexpected key"),
        (g.replace("\"class\":\"worktree\"", "\"class\":\"port\""), "unexpected key"),
        (g.replace("\"ts_ms\":8", "\"ts_ms\":\"8\""), "ts_ms has the wrong type"),
        (g.replace("\"id\":\"w-1\"", "\"id\":\"W 1\""), "plain token"),
    ] {
        assert!(Line::decode(&bad).unwrap_err().contains(want), "{bad}: names {want}");
    }
    // v is checked before the rest, so a v2 line with an unknown op still names v.
    assert_eq!(Line::decode(&g.replace("\"v\":1", "\"v\":2").replace("add", "future")).unwrap_err(), "unsupported v 2");
}

#[test]
fn pid_and_port_bounds_are_refused_on_both_sides() {
    let twin = |pid: u64| {
        Line::AddProcess { ticket: "t-1".into(), id: "p-1".into(), pid, pid_start_ms: 1730000000000, ts_ms: 9 }
    };
    for pid in [0, 1] {
        assert!(twin(pid).encode().is_err(), "pid {pid} refused at encode");
        let bad = twin(4242).encode().unwrap().replace("\"pid\":4242", &format!("\"pid\":{pid}"));
        assert!(Line::decode(bad.trim_end()).unwrap_err().contains("> 1"), "pid {pid} refused at decode");
    }
    assert!(twin(2).encode().is_ok(), "twin: pid 2 is fine");
    let ptwin = |port: u64| Line::AddPort { ticket: "t-1".into(), id: "n-1".into(), port, ts_ms: 10 };
    for port in [0, 65536] {
        assert!(ptwin(port).encode().is_err(), "port {port} refused at encode");
        let bad = ptwin(8080).encode().unwrap().replace("\"port\":8080", &format!("\"port\":{port}"));
        assert!(Line::decode(bad.trim_end()).unwrap_err().contains("65535"), "port {port} refused at decode");
    }
    assert!(ptwin(1).encode().is_ok() && ptwin(65535).encode().is_ok(), "twin: the port bounds are inclusive");
}

#[test]
fn a_process_add_needs_an_integer_pid_start_ms() {
    let g = Line::AddProcess { ticket: "t-1".into(), id: "p-1".into(), pid: 42, pid_start_ms: 7, ts_ms: 9 }
        .encode()
        .unwrap();
    let g = g.trim_end_matches('\n');
    let missing = g.replacen(",\"pid_start_ms\":7", "", 1);
    assert!(Line::decode(&missing).unwrap_err().contains("pid_start_ms missing"), "missing pid_start_ms: {}", &missing);
    let not_int = g.replace("\"pid_start_ms\":7", "\"pid_start_ms\":\"7\"");
    assert!(Line::decode(&not_int).unwrap_err().contains("pid_start_ms has the wrong type"), "non-integer pid_start_ms: {}", &not_int);
    assert!(Line::decode(g).is_ok(), "twin: the integer one decodes");
}

#[test]
fn a_path_that_needs_escaping_a_relative_path_and_root_slash_are_refused() {
    for bad in [
        Line::AddPath { ticket: "t-1".into(), id: "a".into(), class: Class::TempDir, path: "rel/x".into(), ts_ms: 1 },
        Line::AddPath { ticket: "t-1".into(), id: "a".into(), class: Class::TempDir, path: "/a\"b".into(), ts_ms: 1 },
        Line::AddPath { ticket: "t-1".into(), id: "a".into(), class: Class::TempDir, path: "/a\\b".into(), ts_ms: 1 },
        Line::AddPath { ticket: "t-1".into(), id: "a".into(), class: Class::TempDir, path: "/a\u{1}b".into(), ts_ms: 1 },
        Line::Root { ticket: "t-1".into(), path: "/".into(), ts_ms: 1 },
    ] {
        assert!(bad.encode().is_err(), "{bad:?} refused at encode");
    }
    // decode side: an escape sequence in a stored path is refused outright
    let g = samples()[1].encode().unwrap();
    let escaped = g.replace("/scratch/roots/r/w", "/scratch/roots/r/\\u0020w");
    assert!(Line::decode(escaped.trim_end()).unwrap_err().contains("escape"), "{escaped}");
    // twin: a space needs no escape and is fine
    let spaced = g.replace("/scratch/roots/r/w", "/scratch/roots/r/a b");
    assert!(Line::decode(spaced.trim_end()).is_ok(), "{spaced}");
}

#[test]
fn a_line_over_the_cap_is_refused_and_the_cap_line_is_not() {
    let long_id = "a".repeat(LINE_CAP);
    let over = Line::AddPort { ticket: "t-1".into(), id: long_id, port: 1, ts_ms: 1 };
    assert!(matches!(over.encode(), Err(ManifestError::TooLong(_))));
    let over_line = over.encode().unwrap_err();
    assert!(over_line.to_string().contains("cap"));
    let r = parse_manifest(format!("{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"{}\",\"class\":\"port\",\"port\":1,\"ts_ms\":1}}\n", "a".repeat(LINE_CAP)).as_bytes(), "t-1");
    assert!(r.corrupt.unwrap().msg.contains("cap"), "an over-cap complete line is corruption");
    // twin: a line of exactly the cap (newline included) is fine
    let id_len = LINE_CAP - "{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"\",\"class\":\"port\",\"port\":1,\"ts_ms\":1}\n".len();
    let at_cap = Line::AddPort { ticket: "t-1".into(), id: "a".repeat(id_len), port: 1, ts_ms: 1 };
    let enc = at_cap.encode().unwrap();
    assert_eq!(enc.len(), LINE_CAP);
    assert!(Line::decode(enc.trim_end()).is_ok());
}

// ---------------------------------------------------------------- state and ops

fn init(t: &Tmp, roots: &[PathBuf]) -> Result<(), ManifestError> {
    apply(&t.manifest_dir(), "t-1", &Op::Init { roots }, ts(1))
}

#[test]
fn init_writes_one_root_line_per_root_resolved_and_outside_the_roots() {
    let t = Tmp::new("init");
    let r1 = t.sub("roots/r1");
    let r2 = t.sub("roots/r2");
    init(&t, &[r1.clone(), r2.clone()]).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(read.is_clean());
    assert_eq!(read.roots, vec![r1.canonicalize().unwrap().to_string_lossy().into_owned(), r2.canonicalize().unwrap().to_string_lossy().into_owned()]);
    assert!(read.live.is_empty(), "a fresh manifest is empty (roots do not count)");
    // the file lives under the manifest dir, not under a root
    assert!(t.manifest_dir().join("t-1.jsonl").exists());
    assert!(!r1.join("t-1.jsonl").exists());
}

#[test]
fn a_second_init_an_add_and_a_remove_before_init_are_refused_unchanged() {
    let t = Tmp::new("order");
    let r = t.sub("roots/r");
    // before init: no file at all
    let err = apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n-1", port: 8080 }, ts(2)).unwrap_err();
    assert!(err.to_string().contains("run init first"), "{err}");
    assert!(matches!(apply(&t.manifest_dir(), "t-1", &Op::Remove { id: "n-1" }, ts(2)), Err(ManifestError::Invalid(_))));
    init(&t, &[r.clone()]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n-1", port: 8080 }, ts(3)).unwrap();
    let before = t.bytes("t-1");
    let err = init(&t, &[r]).unwrap_err();
    assert!(err.to_string().contains("second init"), "{err}");
    assert_eq!(t.bytes("t-1"), before, "the refused init wrote nothing");
}

#[test]
fn the_fence_refuses_outside_paths_traversal_and_symlink_escapes() {
    let t = Tmp::new("fence");
    let r = t.sub("roots/r");
    let o = t.sub("outside/o");
    std::os::unix::fs::symlink(&o, r.join("link")).unwrap();
    init(&t, &[r.clone()]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "ok", class: Class::TempDir, path: &r.join("in").join("x") }, ts(2)).unwrap();

    let before = t.bytes("t-1");
    for (name, p) in [
        ("outside", o.join("x")),
        ("traversal", r.join("..").join("outside").join("o").join("x")),
        ("symlink", r.join("link").join("x")),
    ] {
        let err = apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "bad", class: Class::TempDir, path: &p }, ts(3)).unwrap_err();
        assert!(err.to_string().contains("outside every root"), "{name}: {err}");
        assert_eq!(t.bytes("t-1"), before, "{name}: the file is byte-identical");
    }
    // twin: a not-yet-existing path whose parent resolves inside the root is fine
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "future", class: Class::TempDir, path: &r.join("in").join("y").join("z") }, ts(4)).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert_eq!(read.live.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["ok", "future"]);
}

#[test]
fn add_then_remove_round_trips_and_ownership_follows_the_live_state() {
    let t = Tmp::new("cycle");
    let r = t.sub("roots/r");
    init(&t, &[r.clone()]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "a", class: Class::TempDir, path: &r.join("x") }, ts(2)).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert_eq!(read.live.len(), 1);
    assert_eq!(read.live[0].target, Target::Path(r.canonicalize().unwrap().join("x").to_string_lossy().into_owned()));
    assert!(owns(&read, Query::Path(&r.join("x").join("y"))));
    assert!(!owns(&read, Query::Path(&r.join("other"))));
    assert!(!owns(&read, Query::Pid(4242)));
    apply(&t.manifest_dir(), "t-1", &Op::Remove { id: "a" }, ts(3)).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(read.live.is_empty(), "manifest empty after the remove");
    assert!(!owns(&read, Query::Path(&r.join("x").join("y"))), "not owned after the remove");
    let before = t.bytes("t-1");
    let err = apply(&t.manifest_dir(), "t-1", &Op::Remove { id: "a" }, ts(4)).unwrap_err();
    assert!(err.to_string().contains("not live"), "{err}");
    assert_eq!(t.bytes("t-1"), before);
    // twin: after the remove the id and the path are both free again
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "a", class: Class::TempDir, path: &r.join("x") }, ts(5)).unwrap();
    assert_eq!(read_manifest(&t.manifest_dir(), "t-1").unwrap().live.len(), 1);
}

#[test]
fn duplicate_live_ids_paths_pids_and_ports_are_refused() {
    let t = Tmp::new("dup");
    let r = t.sub("roots/r");
    init(&t, &[r.clone()]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "a", class: Class::TempDir, path: &r.join("x") }, ts(2)).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddProcess { id: "p", pid: 4242, pid_start_ms: 7 }, ts(3)).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(4)).unwrap();
    let before = t.bytes("t-1");
    for (name, op) in [
        ("id", Op::AddPath { id: "a", class: Class::TempDir, path: &r.join("y") }),
        ("path", Op::AddPath { id: "b", class: Class::TempDir, path: &r.join("x") }),
        ("pid", Op::AddProcess { id: "p2", pid: 4242, pid_start_ms: 8 }),
        ("port", Op::AddPort { id: "n2", port: 8080 }),
    ] {
        let err = apply(&t.manifest_dir(), "t-1", &op, ts(5)).unwrap_err();
        assert!(err.to_string().contains("twice live") || err.to_string().contains("duplicate live id"), "{name}: {err}");
        assert_eq!(t.bytes("t-1"), before, "{name}: the file is byte-identical");
    }
    // bounds at apply time
    assert!(apply(&t.manifest_dir(), "t-1", &Op::AddProcess { id: "p3", pid: 1, pid_start_ms: 7 }, ts(5)).unwrap_err().to_string().contains("> 1"));
    assert!(apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n3", port: 65536 }, ts(5)).unwrap_err().to_string().contains("65535"));
}

#[test]
fn a_manifest_dir_inside_a_root_is_refused_and_nothing_is_created() {
    let t = Tmp::new("f2");
    let r = t.sub("roots/r");
    let inside = r.join("man");
    let err = apply(&inside, "t-1", &Op::Init { roots: &[r.clone()] }, ts(1)).unwrap_err();
    assert!(err.to_string().contains("inside one of the ticket's roots"), "{err}");
    assert!(!inside.exists(), "the refused init created no folder");
    // the root itself as the manifest dir is refused the same way
    assert!(apply(&r, "t-1", &Op::Init { roots: &[r.clone()] }, ts(1)).is_err());
    // twin: a dir outside the roots is fine
    apply(&t.manifest_dir(), "t-1", &Op::Init { roots: &[r] }, ts(1)).unwrap();
}

#[test]
fn a_bad_root_is_refused_relative_missing_a_file_or_slash() {
    let t = Tmp::new("badroot");
    let r = t.sub("roots/r");
    let file = t.0.join("plain-file");
    std::fs::write(&file, b"x").unwrap();
    for (name, root) in [
        ("relative", PathBuf::from("roots/r")),
        ("missing", t.0.join("roots/nope")),
        ("a file", file),
        ("slash", PathBuf::from("/")),
    ] {
        let err = apply(&t.manifest_dir(), "t-1", &Op::Init { roots: &[root] }, ts(1)).unwrap_err();
        assert!(matches!(err, ManifestError::Invalid(_)), "{name}: {err}");
        assert!(!t.manifest_dir().exists(), "{name}: nothing was created");
    }
    // twin: the good root inits
    apply(&t.manifest_dir(), "t-1", &Op::Init { roots: &[r] }, ts(1)).unwrap();
}

#[test]
fn pid_and_port_entries_and_ownership_survive_the_writing_call() {
    let t = Tmp::new("pp");
    let r = t.sub("roots/r");
    init(&t, &[r]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddProcess { id: "p", pid: 4242, pid_start_ms: 1730000000000 }, ts(2)).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(3)).unwrap();
    // a fresh read, as a later invocation sees it
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert_eq!(read.live[0].target, Target::Process { pid: 4242, pid_start_ms: 1730000000000 });
    assert_eq!(read.live[1].target, Target::Port(8080));
    assert!(owns(&read, Query::Pid(4242)));
    assert!(owns(&read, Query::Port(8080)));
    assert!(!owns(&read, Query::Pid(4243)));
    assert!(!owns(&read, Query::Port(8081)));
}

// ---------------------------------------------------------------- damage

#[test]
fn a_torn_tail_is_reported_never_parsed_and_refuses_writes() {
    let t = Tmp::new("torn");
    let r = t.sub("roots/r");
    init(&t, &[r]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(2)).unwrap();
    let clean = t.bytes("t-1");
    let next = Line::Remove { ticket: "t-1".into(), id: "n".into(), ts_ms: 3 }.encode().unwrap();
    // cut the next line short: a final fragment with no newline is a torn tail
    let mut torn = clean.clone();
    torn.extend_from_slice(&next.as_bytes()[..next.len() - 10]);
    std::fs::write(t.manifest_dir().join("t-1.jsonl"), &torn).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(!read.is_clean());
    assert!(read.torn_tail.is_some() && read.corrupt.is_none());
    assert_eq!(read.lines.len(), 2, "the complete lines before the fragment are intact");
    let before = t.bytes("t-1");
    let err = apply(&t.manifest_dir(), "t-1", &Op::Remove { id: "n" }, ts(3)).unwrap_err();
    assert!(matches!(err, ManifestError::Damaged { .. }), "{err}");
    assert_eq!(t.bytes("t-1"), before, "the refused write changed nothing");
    // twin: the same file with the whole line and its newline is clean, the entry removed
    let mut whole = clean.clone();
    whole.extend_from_slice(next.as_bytes());
    std::fs::write(t.manifest_dir().join("t-1.jsonl"), &whole).unwrap();
    let whole_read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(whole_read.is_clean());
    assert!(whole_read.live.is_empty());
}

#[test]
fn a_complete_garbage_line_is_corruption_and_wins_over_a_torn_tail() {
    let t = Tmp::new("corrupt");
    let r = t.sub("roots/r");
    init(&t, &[r]).unwrap();
    let mut bytes = t.bytes("t-1");
    bytes.extend_from_slice(b"garbage\n{\"v\":1");
    let read = parse_manifest(&bytes, "t-1");
    assert_eq!(read.lines.len(), 1);
    assert_eq!(read.corrupt.as_ref().map(|c| c.line), Some(2));
    assert!(read.torn_tail.is_none(), "corruption wins over the torn tail");
    assert!(read.live.is_empty(), "a damaged read carries no state");
}

#[test]
fn a_line_from_another_ticket_and_a_broken_invariant_are_corruption() {
    let t = Tmp::new("ticket");
    let r = t.sub("roots/r");
    // a line whose ticket differs from the file's
    let other = Line::AddPort { ticket: "t-2".into(), id: "n".into(), port: 8080, ts_ms: 1 }.encode().unwrap();
    let root_line = Line::Root { ticket: "t-1".into(), path: r.canonicalize().unwrap().to_string_lossy().into_owned(), ts_ms: 1 }.encode().unwrap();
    let read = parse_manifest(format!("{root_line}{other}").as_bytes(), "t-1");
    let c = read.corrupt.unwrap();
    assert_eq!(c.line, 2);
    assert!(c.msg.contains("differs from the file's"), "{}", c.msg);
    // an add before init, a remove of a dead id, a root line after an operation: all corruption on read
    let add = Line::AddPort { ticket: "t-1".into(), id: "n".into(), port: 8080, ts_ms: 2 }.encode().unwrap();
    let rm = Line::Remove { ticket: "t-1".into(), id: "ghost".into(), ts_ms: 3 }.encode().unwrap();
    for (name, bytes) in [
        ("add before init", add.clone()),
        ("remove of a dead id", format!("{root_line}{rm}")),
        ("root after an operation", format!("{root_line}{add}{root_line}")),
    ] {
        let read = parse_manifest(bytes.as_bytes(), "t-1");
        assert!(read.corrupt.is_some(), "{name} is corruption, not a clean state");
    }
}

// ---------------------------------------------------------------- pid identity

#[test]
fn process_start_ms_reads_a_live_pid_and_refuses_an_absent_one() {
    let me = std::process::id() as u64;
    let start = process_start_ms(me).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    assert!(start > 0 && start <= now, "start {start} is between the epoch and now {now}");
    assert!(now - start < 3_600_000, "this test process started less than an hour ago");
    assert!(process_start_ms(999_999_999_u64).is_err(), "a pid no OS holds is refused");
    assert!(process_start_ms(0).is_err() && process_start_ms(1).is_err(), "pid 0 and 1 are refused");
}

// ---------------------------------------------------------------- fix round 1 (review pr49)

/// A relative folder under the crate folder (integration tests run with the crate folder as cwd), removed on drop.
struct RelDir(String);

impl RelDir {
    fn new(tag: &str) -> RelDir {
        RelDir(format!(".tmp-manifest-{tag}-{}", std::process::id()))
    }

    fn path(&self) -> PathBuf {
        PathBuf::from(&self.0)
    }
}

impl Drop for RelDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// B1: an add or remove on an EXISTING file that holds no init (an init that crashed between create and write
/// leaves exactly a 0-byte file) is refused, the file stays 0 bytes, and a later init still unblocks the ticket.
#[test]
fn an_add_or_remove_on_an_existing_init_less_file_is_refused_run_init_first() {
    let t = Tmp::new("noinit");
    let r = t.sub("roots/r");
    std::fs::create_dir_all(t.manifest_dir()).unwrap();
    std::fs::write(t.manifest_dir().join("t-1.jsonl"), b"").unwrap();
    for (name, op) in [
        ("add port", Op::AddPort { id: "n-1", port: 8080 }),
        ("add process", Op::AddProcess { id: "p-1", pid: 4242, pid_start_ms: 7 }),
        ("add path", Op::AddPath { id: "a", class: Class::TempDir, path: &r.join("x") }),
        ("remove", Op::Remove { id: "n-1" }),
    ] {
        let err = apply(&t.manifest_dir(), "t-1", &op, ts(2)).unwrap_err();
        assert!(err.to_string().contains("run init first"), "{name}: {err}");
    }
    assert_eq!(t.bytes("t-1"), Vec::<u8>::new(), "the empty file is untouched");
    // twin: init on the same empty file works and unblocks the ticket
    init(&t, &[r]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n-1", port: 8080 }, ts(3)).unwrap();
    assert_eq!(read_manifest(&t.manifest_dir(), "t-1").unwrap().live.len(), 1);
}

/// B2: a damaged manifest owns nothing — a torn tail answers "not owned" for a path, a pid and a port that
/// were live before the damage; the same bytes with a whole final line own all three again.
#[test]
fn a_damaged_manifest_owns_nothing_for_path_pid_and_port() {
    let t = Tmp::new("owns-dmg");
    let r = t.sub("roots/r");
    init(&t, &[r.clone()]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "a", class: Class::TempDir, path: &r.join("x") }, ts(2)).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddProcess { id: "p", pid: 4242, pid_start_ms: 7 }, ts(3)).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(4)).unwrap();
    let clean = t.bytes("t-1");
    let mut torn = clean.clone();
    torn.extend_from_slice(b"{\"v\":1,\"op\":\"re"); // a final fragment with no newline
    std::fs::write(t.manifest_dir().join("t-1.jsonl"), &torn).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(!read.is_clean());
    assert!(!owns(&read, Query::Path(&r.join("x").join("y"))), "a torn tail owns no path");
    assert!(!owns(&read, Query::Pid(4242)), "a torn tail owns no pid");
    assert!(!owns(&read, Query::Port(8080)), "a torn tail owns no port");
    // twin: the same file without the fragment is clean and owns all three
    std::fs::write(t.manifest_dir().join("t-1.jsonl"), &clean).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(read.is_clean());
    assert!(owns(&read, Query::Path(&r.join("x").join("y"))));
    assert!(owns(&read, Query::Pid(4242)));
    assert!(owns(&read, Query::Port(8080)));
}

/// B3: a relative root that DOES exist relative to the test's working folder is refused as *not absolute*,
/// not as missing (an accepting build that deletes the absoluteness check passes a root that exists here).
#[test]
fn a_relative_root_that_exists_in_the_cwd_is_refused_as_not_absolute() {
    let t = Tmp::new("rel-root");
    // cargo runs integration tests with the crate folder as cwd, so `tests` and `./tests` exist relative to it
    for name in ["tests", "./tests"] {
        let err = apply(&t.manifest_dir(), "t-1", &Op::Init { roots: &[PathBuf::from(name)] }, ts(1)).unwrap_err();
        assert!(err.to_string().contains("not absolute"), "{name}: {err}");
        assert!(!t.manifest_dir().exists(), "{name}: nothing was created");
    }
    // a ./-spelled relative root built under the working folder, existing too
    let rel = RelDir::new("relroot");
    std::fs::create_dir_all(rel.path().join("r")).unwrap();
    let root = PathBuf::from(".").join(&rel.0).join("r");
    let err = apply(&t.manifest_dir(), "t-1", &Op::Init { roots: &[root] }, ts(1)).unwrap_err();
    assert!(err.to_string().contains("not absolute"), "./-spelled: {err}");
    assert!(!t.manifest_dir().exists(), "the refused init created no folder");
    assert!(rel.path().join("r").exists(), "the relative root itself is untouched");
}

/// N1: a relative manifest dir whose parent does not exist yet is answered for the current folder (joined on
/// before the root check), so it is accepted when it lands outside the roots — and still refused inside one.
#[test]
fn a_relative_manifest_dir_with_a_missing_parent_is_answered_for_the_cwd() {
    let t = Tmp::new("rel-dir");
    let r = t.sub("roots/r");
    let rel = RelDir::new("reldir");
    // no part of the relative dir exists yet, so the walk has to reach the current folder before anything resolves
    let dir = rel.path().join("fresh").join("manifests");
    apply(&dir, "t-1", &Op::Init { roots: &[r.clone()] }, ts(1)).unwrap();
    assert!(dir.join("t-1.jsonl").exists(), "the relative dir was joined onto the cwd and created");
    // twin: a relative dir that lands INSIDE a root is still refused
    let root2_rel = rel.path().join("roots").join("r");
    std::fs::create_dir_all(&root2_rel).unwrap();
    let root2 = root2_rel.canonicalize().unwrap();
    let inside = PathBuf::from(".").join(&rel.0).join("roots").join("r").join("man");
    let err = apply(&inside, "t-1", &Op::Init { roots: &[root2] }, ts(1)).unwrap_err();
    assert!(err.to_string().contains("inside one of the ticket's roots"), "{err}");
    assert!(!inside.exists(), "the refused init created no folder");
}

/// N2: a hand-written path entry that lies outside every declared root is a fence break: corruption on read,
/// it owns nothing, and a write onto that file is refused unchanged (fail closed).
#[test]
fn a_hand_written_path_entry_outside_every_root_is_corruption_on_read() {
    let t = Tmp::new("fence-read");
    let r = t.sub("roots/r");
    let root_line = Line::Root { ticket: "t-1".into(), path: r.canonicalize().unwrap().to_string_lossy().into_owned(), ts_ms: 1 }.encode().unwrap();
    let outside = Line::AddPath { ticket: "t-1".into(), id: "etc".into(), class: Class::TempDir, path: "/etc".into(), ts_ms: 2 }.encode().unwrap();
    std::fs::create_dir_all(t.manifest_dir()).unwrap();
    std::fs::write(t.manifest_dir().join("t-1.jsonl"), format!("{root_line}{outside}")).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(!read.is_clean());
    let c = read.corrupt.as_ref().unwrap();
    assert!(c.msg.contains("outside every root"), "{}", c.msg);
    assert!(!owns(&read, Query::Path(Path::new("/etc/passwd"))), "a fence break owns nothing");
    let before = t.bytes("t-1");
    assert!(matches!(apply(&t.manifest_dir(), "t-1", &Op::Remove { id: "etc" }, ts(3)), Err(ManifestError::Damaged { .. })));
    assert_eq!(t.bytes("t-1"), before, "the refused write changed nothing");
}

/// N5: a live `R/x` owns `R/x/y` but NOT the sibling `R/xy` (a shared string prefix is not ownership).
#[test]
fn a_live_path_owns_children_but_not_a_sibling_sharing_a_string_prefix() {
    let t = Tmp::new("sib");
    let r = t.sub("roots/r");
    init(&t, &[r.clone()]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "a", class: Class::TempDir, path: &r.join("x") }, ts(2)).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(read.is_clean());
    assert!(owns(&read, Query::Path(&r.join("x").join("y"))), "R/x owns R/x/y");
    assert!(!owns(&read, Query::Path(&r.join("xy"))), "R/x does not own the sibling R/xy");
    assert!(owns(&read, Query::Path(&r.join("x"))), "R/x owns itself (equal counts as inside)");
}

/// INPUT-BOUND: a manifest file over 1 MiB is refused as corrupt (fail closed, no unbounded read); a write
/// onto it is refused unchanged; a file AT the cap still gets its normal read (a torn fragment is a torn tail).
#[test]
fn a_manifest_file_over_one_mib_is_refused_as_corrupt() {
    const CAP: usize = 1024 * 1024;
    let t = Tmp::new("bigfile");
    let r = t.sub("roots/r");
    init(&t, &[r]).unwrap();
    let mut big = t.bytes("t-1");
    big.extend(std::iter::repeat(b'x').take(CAP + 1));
    std::fs::write(t.manifest_dir().join("t-1.jsonl"), &big).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    let c = read.corrupt.as_ref().expect("an over-cap file is corruption");
    assert!(c.msg.contains("1 MiB"), "{}", c.msg);
    assert!(!owns(&read, Query::Port(8080)), "an over-cap file owns nothing");
    let before = t.bytes("t-1");
    assert!(matches!(apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(2)), Err(ManifestError::Damaged { .. })));
    assert_eq!(t.bytes("t-1"), before, "the refused write changed nothing");
    // twin: a file of exactly the cap is not corrupt (a missing final newline there is a torn tail, as always)
    let mut at_cap = t.bytes("t-1");
    at_cap.truncate(0);
    at_cap.extend_from_slice(b"{\"v\":1,\"op\":\"root\",\"ticket\":\"t-1\",\"path\":\"/scratch/r\",\"ts_ms\":1}\n");
    let fill = CAP - at_cap.len();
    at_cap.extend(std::iter::repeat(b'x').take(fill));
    assert_eq!(at_cap.len(), CAP);
    let read = parse_manifest(&at_cap, "t-1");
    assert!(read.corrupt.is_none(), "a file at the cap is not corrupt");
    assert!(read.torn_tail.is_some(), "the unterminated tail at the cap is still a torn tail");
    // and the library-level read of the oversized bytes agrees
    assert!(parse_manifest(&vec![b'x'; CAP + 1], "t-1").corrupt.unwrap().msg.contains("1 MiB"));
}

// ---------------------------------------------------------------- fix round 2 (review pr49, round 2)

/// Every `.jsonl` file anywhere under `p` (the ticket IS the file name, so a bad one must not plant a file anywhere).
fn jsonl_under(p: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let ep = e.path();
            if ep.is_dir() {
                jsonl_under(&ep, out);
            } else if ep.extension().is_some_and(|x| x == "jsonl") {
                out.push(ep);
            }
        }
    }
}

/// A `&'static str` view of an owned string, for `Op`'s borrows inside spawned threads.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// B4: a non-plain `ticket` is refused for every op, naming the plain-token rule, and creates no file anywhere
/// under the scratch parent — the ticket becomes the file name, so `../escape` must not write outside the dir.
#[test]
fn a_non_plain_ticket_is_refused_for_every_op_and_creates_nothing_anywhere() {
    let t = Tmp::new("ticket4");
    let r = t.sub("roots/r");
    for bad in ["../escape", "a/b", "UPPER", "", ".", ".."] {
        for (name, op) in [
            ("init", Op::Init { roots: &[r.clone()] }),
            ("add", Op::AddPort { id: "n", port: 8080 }),
            ("remove", Op::Remove { id: "n" }),
        ] {
            let err = apply(&t.manifest_dir(), bad, &op, ts(1)).unwrap_err();
            assert!(matches!(err, ManifestError::Invalid(_)), "{bad:?} {name}: {err}");
            assert!(err.to_string().contains("plain token"), "{bad:?} {name}: {err}");
        }
        assert!(!t.manifest_dir().exists(), "{bad:?}: no manifest dir was created");
    }
    let mut found = Vec::new();
    jsonl_under(&t.0, &mut found);
    assert!(found.is_empty(), "no manifest file anywhere under the scratch parent: {found:?}");
    // twin: the plain lowercase ticket inits on the same dir
    apply(&t.manifest_dir(), "t-1", &Op::Init { roots: &[r] }, ts(1)).unwrap();
    assert!(t.manifest_dir().join("t-1.jsonl").exists());
}

/// B4 (decode half): a stored line carrying a non-plain ticket is refused by the strict decoder too.
#[test]
fn a_stored_line_with_a_non_plain_ticket_is_refused_on_decode() {
    let raw = "{\"v\":1,\"op\":\"add\",\"ticket\":\"T-1\",\"id\":\"n\",\"class\":\"port\",\"port\":8080,\"ts_ms\":1}";
    let err = Line::decode(raw).unwrap_err();
    assert!(err.contains("plain token"), "{err}");
}

/// B5(a): N threads add the SAME id; the lock must cover read+validate+append, so exactly one wins per round
/// and the file reads clean with one live id (bounded: 8 threads, 25 rounds).
#[test]
fn concurrent_adds_of_one_id_leave_exactly_one_winner_and_a_clean_file() {
    let t = Tmp::new("race-id");
    let r = t.sub("roots/r");
    for round in 0..25 {
        let dir = t.0.join(format!("man-{round}"));
        apply(&dir, "t-1", &Op::Init { roots: &[r.clone()] }, ts(1)).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let b = barrier.clone();
            let d = dir.clone();
            handles.push(std::thread::spawn(move || {
                b.wait();
                apply(&d, "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(2)).is_ok()
            }));
        }
        let oks: usize = handles.into_iter().map(|h| h.join().unwrap() as usize).sum();
        assert_eq!(oks, 1, "round {round}: exactly one add of the same id wins");
        let read = read_manifest(&dir, "t-1").unwrap();
        assert!(read.is_clean(), "round {round}: {:?}", read.corrupt.as_ref().map(|c| &c.msg));
        assert_eq!(read.live.len(), 1, "round {round}: one live id");
        assert_eq!(read.live[0].id, "n");
    }
}

/// B5(a): two concurrent removes of one id — one wins, the loser refuses (a dead-id remove), the file stays readable.
#[test]
fn concurrent_removes_of_one_id_leave_the_file_readable() {
    let t = Tmp::new("race-rm");
    let r = t.sub("roots/r");
    for round in 0..25 {
        let dir = t.0.join(format!("man-{round}"));
        apply(&dir, "t-1", &Op::Init { roots: &[r.clone()] }, ts(1)).unwrap();
        apply(&dir, "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(2)).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let b = barrier.clone();
            let d = dir.clone();
            handles.push(std::thread::spawn(move || {
                b.wait();
                apply(&d, "t-1", &Op::Remove { id: "n" }, ts(3)).is_ok()
            }));
        }
        let oks: usize = handles.into_iter().map(|h| h.join().unwrap() as usize).sum();
        assert_eq!(oks, 1, "round {round}: exactly one remove wins");
        let read = read_manifest(&dir, "t-1").unwrap();
        assert!(read.is_clean(), "round {round}: {:?}", read.corrupt.as_ref().map(|c| &c.msg));
        assert!(read.live.is_empty(), "round {round}: the id is gone");
    }
}

/// B5(b): a hand-written file the writer would refuse is corruption on READ too — a duplicate live id, the same
/// path, the same pid and the same port twice live each stop the read (the reader is the fail-closed backstop
/// for a file written by a buggy or racing writer).
#[test]
fn a_duplicate_live_id_or_the_same_path_pid_or_port_twice_live_is_corruption_on_read() {
    let t = Tmp::new("dup-read");
    let r = t.sub("roots/r");
    let root = r.canonicalize().unwrap().to_string_lossy().into_owned();
    let root_line = Line::Root { ticket: "t-1".into(), path: root.clone(), ts_ms: 1 }.encode().unwrap();
    let port = |id: &str, port: u64, ts_ms: u64| {
        Line::AddPort { ticket: "t-1".into(), id: id.into(), port, ts_ms }.encode().unwrap()
    };
    let path = |id: &str, p: String, ts_ms: u64| {
        Line::AddPath { ticket: "t-1".into(), id: id.into(), class: Class::TempDir, path: p, ts_ms }.encode().unwrap()
    };
    let proc = |id: &str, pid: u64, ts_ms: u64| {
        Line::AddProcess { ticket: "t-1".into(), id: id.into(), pid, pid_start_ms: 7, ts_ms }.encode().unwrap()
    };
    let x = r.join("x").to_string_lossy().into_owned(); // stored paths need not exist; the reader fences lexically
    for (name, bytes, needle) in [
        ("duplicate live id", format!("{root_line}{}{}", port("n", 8080, 2), port("n", 8081, 3)), "duplicate live id"),
        ("same path twice live", format!("{root_line}{}{}", path("a", x.clone(), 2), path("b", x, 3)), "path is listed twice live"),
        ("same pid twice live", format!("{root_line}{}{}", proc("p", 4242, 2), proc("q", 4242, 3)), "pid is listed twice live"),
        ("same port twice live", format!("{root_line}{}{}", port("n1", 8080, 2), port("n2", 8080, 3)), "port is listed twice live"),
    ] {
        let read = parse_manifest(bytes.as_bytes(), "t-1");
        assert!(!read.is_clean(), "{name} is corruption on read");
        let c = read.corrupt.as_ref().unwrap();
        assert!(c.msg.contains(needle), "{name}: {}", c.msg);
        assert!(read.roots.is_empty() && read.live.is_empty(), "{name}: a corrupt read carries no state");
    }
}

/// S1: a complete line with trailing bytes after the closing `}` and a number with a leading zero are each
/// corruption — the parser's strictness is pinned, not incidental.
#[test]
fn trailing_bytes_after_the_object_and_a_leading_zero_number_are_corruption() {
    let good = Line::AddPort { ticket: "t-1".into(), id: "n".into(), port: 8080, ts_ms: 1 }.encode().unwrap();
    let trailing = format!("{}x", good.trim_end_matches('\n'));
    let err = Line::decode(&trailing).unwrap_err();
    assert!(err.contains("trailing bytes"), "{err}");
    let lz = "{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\",\"id\":\"n\",\"class\":\"port\",\"port\":08080,\"ts_ms\":1}";
    let err = Line::decode(lz).unwrap_err();
    assert!(err.contains("leading zero"), "{err}");
    // twin: the same line without the trailing byte and without the leading zero decodes
    assert!(Line::decode(good.trim_end_matches('\n')).is_ok());
}

/// S2: `Init { roots: &[] }` is refused and creates no manifest dir.
#[test]
fn an_init_with_no_roots_is_refused_and_creates_no_manifest_dir() {
    let t = Tmp::new("noroots2");
    let err = apply(&t.manifest_dir(), "t-1", &Op::Init { roots: &[] }, ts(1)).unwrap_err();
    assert!(matches!(err, ManifestError::Invalid(_)), "{err}");
    assert!(err.to_string().contains("--root"), "{err}");
    assert!(!t.manifest_dir().exists(), "the refused init created no folder");
    // twin: one root still inits
    let r = t.sub("roots/r");
    apply(&t.manifest_dir(), "t-1", &Op::Init { roots: &[r] }, ts(1)).unwrap();
}

/// N-a: a torn-tail read carries NO roots and NO live entries ("filled only when the file is clean" must hold),
/// so a later `list` cannot print entries from a torn file by mistake.
#[test]
fn a_torn_tail_read_carries_no_roots_and_no_live_entries() {
    let t = Tmp::new("torn-state");
    let r = t.sub("roots/r");
    init(&t, &[r]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(2)).unwrap();
    let mut torn = t.bytes("t-1");
    torn.extend_from_slice(b"{\"v\":1,\"op\":\"re");
    std::fs::write(t.manifest_dir().join("t-1.jsonl"), &torn).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(read.torn_tail.is_some() && read.corrupt.is_none());
    assert!(read.roots.is_empty(), "a torn tail carries no roots");
    assert!(read.live.is_empty(), "a torn tail carries no live entries");
    assert_eq!(read.lines.len(), 2, "the complete lines before the fragment are intact");
}

/// HELD-OUT v1 (must-fire): 8 threads add DISTINCT ids but the SAME port; exactly one may win and the file
/// must read clean — the duplicate-port refusal is only sound inside the critical section.
#[test]
fn concurrent_adds_of_distinct_ids_sharing_one_port_leave_one_winner() {
    let t = Tmp::new("race-port");
    let r = t.sub("roots/r");
    for round in 0..25 {
        let dir = t.0.join(format!("man-{round}"));
        apply(&dir, "t-1", &Op::Init { roots: &[r.clone()] }, ts(1)).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for i in 0..8 {
            let b = barrier.clone();
            let d = dir.clone();
            handles.push(std::thread::spawn(move || {
                b.wait();
                let id = leak(format!("n-{i}"));
                apply(&d, "t-1", &Op::AddPort { id, port: 8080 }, ts(2)).is_ok()
            }));
        }
        let oks: usize = handles.into_iter().map(|h| h.join().unwrap() as usize).sum();
        assert_eq!(oks, 1, "round {round}: exactly one same-port add wins");
        let read = read_manifest(&dir, "t-1").unwrap();
        assert!(read.is_clean(), "round {round}: {:?}", read.corrupt.as_ref().map(|c| &c.msg));
        assert_eq!(read.live.len(), 1);
    }
}

/// HELD-OUT v2 (must-fire): two concurrent inits of one fresh manifest; exactly one wins, the file holds one init.
#[test]
fn concurrent_inits_of_one_fresh_manifest_leave_exactly_one_init() {
    let t = Tmp::new("race-init");
    let r = t.sub("roots/r");
    for round in 0..25 {
        let dir = t.0.join(format!("man-{round}"));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let b = barrier.clone();
            let d = dir.clone();
            let root = r.clone();
            handles.push(std::thread::spawn(move || {
                b.wait();
                apply(&d, "t-1", &Op::Init { roots: &[root] }, ts(1)).is_ok()
            }));
        }
        let oks: usize = handles.into_iter().map(|h| h.join().unwrap() as usize).sum();
        assert_eq!(oks, 1, "round {round}: exactly one init wins");
        let read = read_manifest(&dir, "t-1").unwrap();
        assert!(read.is_clean(), "round {round}: {:?}", read.corrupt.as_ref().map(|c| &c.msg));
        assert_eq!(read.roots.len(), 1, "round {round}: the file holds one init");
    }
}

/// HELD-OUT v3 (must-fire): a ticket over 200 characters follows the existing name rule — there is no separate
/// length cap, so a plain long token is accepted and its file lands INSIDE the manifest dir, never outside it.
#[test]
fn an_over_long_plain_ticket_is_still_fenced_inside_the_manifest_dir() {
    let t = Tmp::new("longticket");
    let r = t.sub("roots/r");
    let long = format!("x{}", "a".repeat(200));
    apply(&t.manifest_dir(), &long, &Op::Init { roots: &[r.clone()] }, ts(1)).unwrap();
    assert!(t.manifest_dir().join(format!("{long}.jsonl")).exists(), "the long plain ticket inits inside the dir");
    let mut found = Vec::new();
    jsonl_under(&t.0, &mut found);
    assert_eq!(found.len(), 1, "exactly one manifest file, inside the dir: {found:?}");
    assert_eq!(found[0], t.manifest_dir().join(format!("{long}.jsonl")));
    // the non-plain spelling is refused regardless of length
    let bad = format!("x{}", "A".repeat(200));
    assert!(matches!(apply(&t.manifest_dir(), &bad, &Op::Init { roots: &[r] }, ts(1)), Err(ManifestError::Invalid(_))));
}

/// HELD-OUT v5 (must-not-fire): 8 threads add distinct ids with distinct ports; all eight land and the file
/// reads clean with 8 live entries — the lock serialises without refusing independent work.
#[test]
fn concurrent_adds_of_distinct_ids_and_ports_all_land() {
    let t = Tmp::new("race-distinct");
    let r = t.sub("roots/r");
    init(&t, &[r]).unwrap();
    let dir = t.manifest_dir();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for i in 0..8 {
        let b = barrier.clone();
        let d = dir.clone();
        handles.push(std::thread::spawn(move || {
            b.wait();
            let id = leak(format!("n-{i}"));
            apply(&d, "t-1", &Op::AddPort { id, port: 9000 + i }, ts(2)).is_ok()
        }));
    }
    let oks: usize = handles.into_iter().map(|h| h.join().unwrap() as usize).sum();
    assert_eq!(oks, 8, "every independent add lands");
    let read = read_manifest(&dir, "t-1").unwrap();
    assert!(read.is_clean());
    assert_eq!(read.live.len(), 8);
}

/// HELD-OUT v6 (must-not-fire): a lower-case ticket with digits and hyphens is still a plain token.
#[test]
fn a_lower_case_ticket_with_digits_and_hyphens_is_accepted() {
    let t = Tmp::new("plain-ticket");
    let r = t.sub("roots/r");
    apply(&t.manifest_dir(), "p1b-02", &Op::Init { roots: &[r.clone()] }, ts(1)).unwrap();
    assert!(t.manifest_dir().join("p1b-02.jsonl").exists());
    let read = read_manifest(&t.manifest_dir(), "p1b-02").unwrap();
    assert!(read.is_clean() && read.roots.len() == 1);
}

/// HELD-OUT v7 (must-not-fire): single-thread add then remove of one id still works and reads clean.
#[test]
fn a_single_writer_add_then_remove_still_round_trips() {
    let t = Tmp::new("single");
    let r = t.sub("roots/r");
    init(&t, &[r]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPort { id: "n", port: 8080 }, ts(2)).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::Remove { id: "n" }, ts(3)).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(read.is_clean() && read.live.is_empty());
}

// ---------------------------------------------------------------- round 3: B6, S3, N-f

/// Every path under `p`, sorted: a before/after picture of what a refused call left behind.
fn tree(p: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        out.push(d.clone());
        if d.is_dir() {
            for e in std::fs::read_dir(&d).unwrap() {
                stack.push(e.unwrap().path());
            }
        }
    }
    out.sort();
    out
}

/// A path add that must be refused as `Invalid`, leaving the manifest byte-identical and creating nothing anywhere.
fn assert_path_refused_and_nothing_created(tag: &str, rel_to_root: &str) {
    let t = Tmp::new(tag);
    let r = t.sub("roots/r");
    init(&t, &[r.clone()]).unwrap();
    let before = t.bytes("t-1");
    let tree_before = tree(&t.0);
    let p = r.join(rel_to_root); // `join` keeps the text as given, `.` and `..` included
    let err = apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "x", class: Class::TempDir, path: &p }, ts(2)).unwrap_err();
    assert!(matches!(err, ManifestError::Invalid(_)), "{rel_to_root}: {err}");
    assert_eq!(t.bytes("t-1"), before, "{rel_to_root}: the manifest is byte-identical");
    assert_eq!(tree(&t.0), tree_before, "{rel_to_root}: nothing was created");
}

/// B6 (a): a `..` after a component that does not exist yet used to be dropped, so `R/ne/../../O/x` was stored as
/// `R/ne/O/x` while the caller's folder landed outside the root.
#[test]
fn b6_a_dot_dot_after_a_missing_component_that_escapes_is_refused_and_nothing_is_created() {
    assert_path_refused_and_nothing_created("b6a", "ne/../../O/x");
}

/// B6 (b): pinned as REFUSED (never folded to `R/x`, because folding is wrong across symlinks).
#[test]
fn b6_a_dot_dot_after_a_missing_component_that_stays_inside_is_refused_too() {
    assert_path_refused_and_nothing_created("b6b", "ne/../x");
}

/// B6 (c) twin: a plain missing tail is still accepted and stored as given.
#[test]
fn b6_a_plain_missing_component_is_still_accepted_and_stored_as_given() {
    let t = Tmp::new("b6c");
    let r = t.sub("roots/r");
    init(&t, &[r.clone()]).unwrap();
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "x", class: Class::TempDir, path: &r.join("ne").join("x") }, ts(2)).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert_eq!(read.live[0].target, Target::Path(r.join("ne").join("x").to_string_lossy().into_owned()));
}

/// HELD-OUT v1 (must-fire): two missing components, then three `..`.
#[test]
fn v1_two_missing_components_then_three_dot_dots_is_refused_and_nothing_is_created() {
    assert_path_refused_and_nothing_created("v1", "a/b/../../../O/x");
}

/// HELD-OUT v2 (must-fire): a `.` component inside the missing tail.
#[test]
fn v2_a_dot_component_in_the_missing_tail_is_refused_and_nothing_is_created() {
    assert_path_refused_and_nothing_created("v2", "ne/./x");
}

/// HELD-OUT v3 (must-fire): the path ends in `..` after a missing component.
#[test]
fn v3_a_path_ending_in_dot_dot_after_a_missing_component_is_refused() {
    assert_path_refused_and_nothing_created("v3", "ne/..");
}

/// HELD-OUT v5 (must-not-fire): two missing components, plain names, accepted as given.
#[test]
fn v5_two_plain_missing_components_are_accepted_as_given() {
    let t = Tmp::new("v5");
    let r = t.sub("roots/r");
    init(&t, &[r.clone()]).unwrap();
    let p = r.join("ne").join("deep").join("er");
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "x", class: Class::TempDir, path: &p }, ts(2)).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert_eq!(read.live[0].target, Target::Path(p.to_string_lossy().into_owned()));
}

/// HELD-OUT v6 (must-not-fire): `..` through ancestors that all exist still resolves; stored as `R/x`.
#[test]
fn v6_a_dot_dot_through_existing_ancestors_still_resolves_and_is_accepted() {
    let t = Tmp::new("v6");
    let r = t.sub("roots/r");
    init(&t, &[r.clone()]).unwrap();
    let p = r.join("..").join("r").join("x");
    apply(&t.manifest_dir(), "t-1", &Op::AddPath { id: "x", class: Class::TempDir, path: &p }, ts(2)).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert_eq!(read.live[0].target, Target::Path(r.join("x").to_string_lossy().into_owned()));
}

/// S3 (writer): an id that is live in ANY class is refused by every class's add, with a distinct target each time so
/// only the id check can refuse. `first` is added, then `second` (same id, different target).
#[test]
fn s3_a_live_id_is_refused_by_every_class_of_add_and_the_file_is_byte_identical() {
    #[derive(Clone, Copy)]
    enum K {
        Path,
        Process,
        Port,
    }
    let add = |t: &Tmp, r: &Path, k: K, id: &str, n: u64, at: u64| {
        let dir = t.manifest_dir();
        match k {
            K::Path => apply(&dir, "t-1", &Op::AddPath { id, class: Class::TempDir, path: &r.join(format!("p{n}")) }, ts(at)),
            K::Process => apply(&dir, "t-1", &Op::AddProcess { id, pid: 4000 + n, pid_start_ms: 7 }, ts(at)),
            K::Port => apply(&dir, "t-1", &Op::AddPort { id, port: 9000 + n }, ts(at)),
        }
    };
    let names = ["path", "process", "port"];
    let cases = [
        (K::Path, K::Port, "a"),
        (K::Path, K::Process, "a"),
        (K::Process, K::Path, "p"),
        (K::Process, K::Port, "p"),
        (K::Port, K::Path, "q"),
        (K::Port, K::Process, "q"),
        (K::Process, K::Port, "x"), // HELD-OUT v4: process x live, then a port x with a distinct port
    ];
    for (first, second, id) in cases {
        let t = Tmp::new("s3");
        let r = t.sub("roots/r");
        init(&t, &[r.clone()]).unwrap();
        add(&t, &r, first, id, 1, 2).unwrap();
        let before = t.bytes("t-1");
        let err = add(&t, &r, second, id, 2, 3).unwrap_err();
        let what = format!("{} {id} then {} {id}", names[first as usize], names[second as usize]);
        assert!(matches!(&err, ManifestError::Invalid(m) if m.contains("duplicate live id")), "{what}: {err}");
        assert_eq!(t.bytes("t-1"), before, "{what}: the file is byte-identical");
        // twin: the same second add under a fresh id is accepted, so the id was the only reason
        add(&t, &r, second, "fresh", 2, 4).unwrap();
    }
    // the same-class repeat for each class (id check on the arm itself)
    for k in [K::Path, K::Process, K::Port] {
        let t = Tmp::new("s3-same");
        let r = t.sub("roots/r");
        init(&t, &[r.clone()]).unwrap();
        add(&t, &r, k, "z", 1, 2).unwrap();
        let before = t.bytes("t-1");
        let err = add(&t, &r, k, "z", 2, 3).unwrap_err();
        assert!(matches!(&err, ManifestError::Invalid(m) if m.contains("duplicate live id")), "{err}");
        assert_eq!(t.bytes("t-1"), before);
    }
}

/// S3 (reader): a duplicate live id on hand-written files is corruption for the path arm and the process arm too, with
/// distinct targets so the same-path / same-pid checks cannot be the reason.
#[test]
fn s3_a_duplicate_live_id_on_two_path_lines_or_two_process_lines_is_corruption_on_read() {
    let t = Tmp::new("s3-read");
    let r = t.sub("roots/r");
    let root = r.canonicalize().unwrap().to_string_lossy().into_owned();
    let root_line = Line::Root { ticket: "t-1".into(), path: root, ts_ms: 1 }.encode().unwrap();
    let path = |id: &str, name: &str, ts_ms: u64| {
        let p = r.canonicalize().unwrap().join(name).to_string_lossy().into_owned();
        Line::AddPath { ticket: "t-1".into(), id: id.into(), class: Class::TempDir, path: p, ts_ms }.encode().unwrap()
    };
    let proc = |id: &str, pid: u64, ts_ms: u64| {
        Line::AddProcess { ticket: "t-1".into(), id: id.into(), pid, pid_start_ms: 7, ts_ms }.encode().unwrap()
    };
    for (name, bytes) in [
        ("two path lines, one id", format!("{root_line}{}{}", path("a", "x", 2), path("a", "y", 3))),
        ("two process lines, one id", format!("{root_line}{}{}", proc("p", 4242, 2), proc("p", 4243, 3))),
    ] {
        let read = parse_manifest(bytes.as_bytes(), "t-1");
        assert!(!read.is_clean(), "{name} is corruption on read");
        let c = read.corrupt.as_ref().unwrap();
        assert!(c.msg.contains("duplicate live id"), "{name}: {}", c.msg);
        assert!(read.roots.is_empty() && read.live.is_empty(), "{name}: a corrupt read carries no state");
    }
    // twin: the same two lines under two ids are clean
    let ok = format!("{root_line}{}{}", path("a", "x", 2), path("b", "y", 3));
    assert!(parse_manifest(ok.as_bytes(), "t-1").is_clean());
}

/// HELD-OUT v7 (must-not-fire): a normal init with three short roots succeeds and every root line is on disk.
#[test]
fn v7_a_normal_init_with_three_short_roots_writes_every_root_line() {
    let t = Tmp::new("v7");
    let roots = [t.sub("roots/r1"), t.sub("roots/r2"), t.sub("roots/r3")];
    init(&t, &roots).unwrap();
    let read = read_manifest(&t.manifest_dir(), "t-1").unwrap();
    assert!(read.is_clean());
    assert_eq!(read.roots.len(), 3);
    let text = String::from_utf8(t.bytes("t-1")).unwrap();
    assert_eq!(text.lines().filter(|l| l.contains("\"op\":\"root\"")).count(), 3);
}
