//! Process-level smoke for `harness session done-check` / `close` (P1B-17). The real binary runs as a child; the host is a stub shell
//! script under a scratch folder. The real host is never run. The stub keeps its listing in a file: `agents` prints it, `stop <id>` logs
//! and marks the entry done, `rm <id>` logs and deletes the entry. Names are built at run time.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../config/harness.example.yaml"
);
static SEQ: AtomicUsize = AtomicUsize::new(0);

const STUB: &str = r#"#!/bin/sh
d="$STUB_DIR"
case "$1" in
  agents)
    if [ -n "$STUB_AGENTS_EXIT" ]; then exit "$STUB_AGENTS_EXIT"; fi
    if [ -n "$STUB_AGENTS_RAW" ]; then echo "$STUB_AGENTS_RAW"; exit 0; fi
    printf '['; paste -sd, "$d/list"; printf ']\n' ;;
  stop)
    echo "stop $2" >> "$d/calls"
    if [ -n "$STUB_STOP_EXIT" ]; then exit "$STUB_STOP_EXIT"; fi
    sed "/\"id\":\"$2\"/s/\"state\":\"[a-z]*\"/\"state\":\"done\"/" "$d/list" > "$d/list.n"; mv "$d/list.n" "$d/list" ;;
  rm)
    echo "rm $2" >> "$d/calls"
    if [ -n "$STUB_RM_EXIT" ]; then exit "$STUB_RM_EXIT"; fi
    if [ -z "$STUB_RM_NOOP" ]; then grep -v "\"id\":\"$2\"" "$d/list" > "$d/list.n"; mv "$d/list.n" "$d/list"; fi
    echo "removed $2" ;;
esac
exit 0
"#;

struct Env {
    root: PathBuf,
}

fn name(lane: u32, ticket: &str) -> String {
    [
        "exec".to_string(),
        format!("L{lane}"),
        format!("t{ticket}"),
        "r1".into(),
        "M".into(),
        "son".into(),
        "med".into(),
    ]
    .join("-")
}

fn ent(id: &str, name: &str, state: &str) -> String {
    format!("{{\"id\":\"{id}\",\"name\":\"{name}\",\"state\":\"{state}\"}}")
}

impl Env {
    fn new(entries: &[String]) -> Env {
        let root = std::env::temp_dir().join(format!(
            "scc-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let stub = root.join("stub.sh");
        fs::write(&stub, STUB).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            root.join("list"),
            entries.iter().map(|e| format!("{e}\n")).collect::<String>(),
        )
        .unwrap();
        fs::write(root.join("calls"), "").unwrap();
        fs::copy(EXAMPLE, root.join("config.yaml")).unwrap();
        Env { root }
    }
    fn p(&self, s: &str) -> String {
        self.root.join(s).to_str().unwrap().to_string()
    }
    fn run(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut c = Command::new(BIN);
        c.args(args)
            .stdin(Stdio::null())
            .env_remove("HARNESS_CONFIG")
            .env("STUB_DIR", &self.root);
        for (k, v) in envs {
            c.env(k, v);
        }
        c.output().unwrap()
    }
    fn sess(&self, verb: &str, flag: &str, val: &str, envs: &[(&str, &str)]) -> Output {
        let (h, c) = (self.p("stub.sh"), self.p("config.yaml"));
        self.run(
            &["session", verb, "--host", &h, "--config", &c, flag, val],
            envs,
        )
    }
    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("calls")).unwrap()
    }
    fn list(&self) -> String {
        fs::read_to_string(self.root.join("list")).unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

#[test]
fn done_check_names_planted_done_session() {
    let (n1, n2) = (name(1, "t1"), name(2, "t2"));
    let e = Env::new(&[ent("a1", &n1, "working"), ent("b2", &n2, "working")]);
    let o = e.sess("done-check", "--done", "t1", &[]);
    assert_eq!(code(&o), 1, "{}", out(&o));
    let s = out(&o);
    assert!(
        s.contains(&format!("session=open name={n1} id=a1 ticket=t1")),
        "{s}"
    );
    assert!(s.contains("done-check=fail open=1"), "{s}");
    assert!(!s.contains("b2"), "{s}");
    assert_eq!(e.calls(), "");
}

#[test]
fn close_skips_unparsed_and_ticketless() {
    let n1 = name(1, "t1");
    let e = Env::new(&[
        ent("a1", &n1, "working"),
        ent("o1", "Orchestrator", "working"),
        ent("o2", "D16", "working"),
        ent("o3", "builder2-t1", "working"),
        ent("o4", &name(9, "t1"), "working"),
    ]);
    let o = e.sess("close", "--ticket", "t1", &[]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert!(
        out(&o).contains("close=pass ticket=t1 closed=1"),
        "{}",
        out(&o)
    );
    assert_eq!(e.calls(), "stop a1\nrm a1\n");
    let l = e.list();
    for id in ["o1", "o2", "o3", "o4"] {
        assert!(l.contains(&format!("\"id\":\"{id}\"")), "{l}");
    }
}

#[test]
fn close_reads_legacy_map() {
    let e = Env::new(&[ent("l1", "builder1-p1a13", "done")]);
    let o = e.sess("close", "--ticket", "P1A-14", &[]);
    assert_eq!(code(&o), 0);
    assert_eq!(e.calls(), "");
    let o = e.sess("close", "--ticket", "P1A-13", &[]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert_eq!(
        e.calls(),
        "rm l1\n",
        "a session already done is not stopped"
    );
}

#[test]
fn close_usage_refused() {
    let n1 = name(1, "t1");
    let e = Env::new(&[ent("a1", &n1, "working")]);
    let (h, c) = (e.p("stub.sh"), e.p("config.yaml"));
    let dir = e.p("");
    let cases: Vec<Output> = vec![
        e.sess("close", "--ticket", "-", &[]),
        e.run(
            &[
                "session", "close", "--host", &dir, "--config", &c, "--ticket", "t1",
            ],
            &[],
        ),
        e.run(
            &[
                "session", "close", "--host", "stub.sh", "--config", &c, "--ticket", "t1",
            ],
            &[],
        ),
        e.run(&["session", "close", "--config", &c, "--ticket", "t1"], &[]),
        e.run(&["session", "close", "--host", &h, "--ticket", "t1"], &[]),
        e.run(
            &["session", "done-check", "--host", &h, "--config", &c],
            &[],
        ),
        e.run(
            &[
                "session", "close", "--host", &h, "--config", &c, "--ticket", "t1", "--ticket",
                "t2",
            ],
            &[],
        ),
    ];
    for (i, o) in cases.iter().enumerate() {
        assert_eq!(code(o), 2, "case {i}: {}", out(o));
        assert!(out(o).is_empty(), "case {i}");
    }
    assert_eq!(e.calls(), "");
    assert!(e.list().contains("a1"));
}

#[test]
fn close_failures_stop_at_first_and_unreadable_listing_exits_3() {
    let (n1, n2) = (name(1, "t1"), name(2, "t1"));
    let e = Env::new(&[ent("a1", &n1, "working"), ent("a2", &n2, "working")]);
    let o = e.sess("close", "--ticket", "t1", &[("STUB_STOP_EXIT", "1")]);
    assert_eq!(code(&o), 1);
    assert!(
        out(&o).contains(&format!("close=fail name={n1} id=a1 step=stop")),
        "{}",
        out(&o)
    );
    assert_eq!(e.calls(), "stop a1\n");
    let o = e.sess("close", "--ticket", "t1", &[("STUB_RM_EXIT", "1")]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("step=rm"), "{}", out(&o));
    for raw in [
        "{}",
        "[{\"id\":\"-x\",\"name\":\"n\"}]",
        "not json",
        "[{\"id\":\"a\"}]",
    ] {
        let o = e.sess("done-check", "--done", "t1", &[("STUB_AGENTS_RAW", raw)]);
        assert_eq!(code(&o), 3, "{raw}");
    }
    let o = e.sess("done-check", "--done", "t1", &[("STUB_AGENTS_EXIT", "1")]);
    assert_eq!(code(&o), 3);
}

#[test]
fn done_check_hyphenated_long_slug_unresolved() {
    let e = Env::new(&[ent("h1", "execution-l4-q1-1", "working")]);
    let o = e.sess("done-check", "--done", "Q1-1", &[]);
    assert_eq!(code(&o), 3, "{}", out(&o));
    let s = out(&o);
    assert!(
        s.contains("session=unresolved name=execution-l4-q1-1 id=h1 ticket=q1-1"),
        "{s}"
    );
    assert!(s.contains("done-check=cannot-check unresolved=1"), "{s}");
    assert!(!s.contains("done-check=pass"), "{s}");
    let o = e.sess("close", "--ticket", "Q1-1", &[]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert_eq!(e.calls(), "");
    assert!(e.list().contains("h1"));
    // control: a different ticket
    let o = e.sess("done-check", "--done", "Q2", &[]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert!(out(&o).contains("done-check=pass open=0"), "{}", out(&o));
    // an open session still wins over an unresolved one
    let n1 = name(1, "q11");
    fs::write(
        e.root.join("list"),
        format!(
            "{}\n{}\n",
            ent("h1", "execution-l4-q1-1", "working"),
            ent("a1", &n1, "working")
        ),
    )
    .unwrap();
    let o = e.sess("done-check", "--done", "Q1-1", &[]);
    assert_eq!(code(&o), 1, "{}", out(&o));
    assert!(out(&o).contains("session=unresolved"), "{}", out(&o));
}
