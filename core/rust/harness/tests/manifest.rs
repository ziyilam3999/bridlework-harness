//! `schemas/resource-manifest.schema.json` mirrors the manifest loader's exact key set per shape (same idea as the event,
//! gate and ticket schema tests); this pins the two together, then exercises the library: line codec, the fence, the state
//! fold, damage handling and the pid identity read.
use harness::manifest::{
    apply, owns, parse_manifest, process_start_ms, read_manifest, Class, Line, ManifestError, Op, Query, Target, LINE_CAP,
    SCHEMA_VERSION,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

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
