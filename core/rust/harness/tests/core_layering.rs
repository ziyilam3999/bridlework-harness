//! Core-layering smoke: planted scratch workspaces, the live repo graph, and the committed gate file.
//! Scratch roots live under the temp dir and are removed by a Drop guard.
use harness::core_layering::{graph, judge, Dep, Graph, Member, Verdict};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// `base` is removed on drop; `root` (= base/repo) is the repo root handed to `graph`.
struct Scratch {
    base: PathBuf,
    root: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn scratch() -> Scratch {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let base = std::env::temp_dir().join(format!("hcl-{}-{}", std::process::id(), n));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("repo");
    std::fs::create_dir_all(&root).unwrap();
    Scratch { base, root }
}

fn krate(dir: &Path, name: &str, deps: &str) {
    write(&dir.join("Cargo.toml"), &format!("[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{}", name, deps));
    write(&dir.join("src/lib.rs"), "");
}

/// The PRE-3 layout: `core/rust/<members>` plus unreferenced adapter crates.
fn layout(members: &[(&str, &str)]) -> Scratch {
    let s = scratch();
    let names: Vec<String> = members.iter().map(|(n, _)| format!("\"{}\"", n)).collect();
    write(&s.root.join("core/rust/Cargo.toml"), &format!("[workspace]\nmembers = [{}]\nresolver = \"2\"\n", names.join(", ")));
    for (n, deps) in members {
        krate(&s.root.join("core/rust").join(n), n, deps);
    }
    krate(&s.root.join("providers/px"), "px", "");
    krate(&s.root.join("plugins/pg"), "pg", "");
    krate(&s.root.join("mcp/mx"), "mx", "");
    s
}

fn violation(v: Verdict) -> (String, String, String, String, String) {
    match v {
        Verdict::Violation { member, dep, kind, folder, dep_rel } => (member, dep, kind, folder, dep_rel),
        Verdict::Clean => panic!("expected a violation, got Clean"),
    }
}

#[test]
fn a_clean_scratch_workspace_is_clean() {
    let s = layout(&[("a", "")]);
    let g = graph(&s.root).unwrap();
    assert_eq!(g.members.len(), 1);
    assert_eq!(g.members[0].name, "a");
    assert_eq!(g.members[0].manifest_rel, "core/rust/a/Cargo.toml");
    assert_eq!(judge(&g), Verdict::Clean);
}

#[test]
fn a_planted_core_to_adapter_dependency_is_the_named_violation() {
    let s = layout(&[("a", "[dependencies]\npx = { path = \"../../../providers/px\" }\n")]);
    let g = graph(&s.root).unwrap();
    let (member, dep, kind, folder, dep_rel) = violation(judge(&g));
    assert_eq!(member, "a");
    assert_eq!(dep, "px");
    assert_eq!(kind, "normal");
    assert_eq!(folder, "providers");
    assert_eq!(dep_rel, "providers/px");
}

#[test]
fn every_dependency_kind_and_every_folder_is_judged() {
    let arms = [
        ("[dev-dependencies]\npx = { path = \"../../../providers/px\" }\n", "px", "dev", "providers"),
        ("[build-dependencies]\npg = { path = \"../../../plugins/pg\" }\n", "pg", "build", "plugins"),
        ("[dependencies]\nmx = { path = \"../../../mcp/mx\" }\n", "mx", "normal", "mcp"),
    ];
    for (deps, dep, kind, folder) in arms {
        let s = layout(&[("a", deps)]);
        let (m, d, k, f, _) = violation(judge(&graph(&s.root).unwrap()));
        assert_eq!(m, "a");
        assert_eq!(d, dep);
        assert_eq!(k, kind);
        assert_eq!(f, folder);
    }
}

#[test]
fn escaping_and_case_spelled_paths_are_judged() {
    // (i) an uppercase folder that really exists: counts as `providers`, spelling survives.
    let s = scratch();
    write(&s.root.join("core/rust/Cargo.toml"), "[workspace]\nmembers = [\"a\"]\nresolver = \"2\"\n");
    krate(&s.root.join("Providers/pu"), "pu", "");
    krate(&s.root.join("core/rust/a"), "a", "[dependencies]\npu = { path = \"../../../Providers/pu\" }\n");
    let (_, dep, _, folder, dep_rel) = violation(judge(&graph(&s.root).unwrap()));
    assert_eq!(dep, "pu");
    assert_eq!(folder, "providers");
    assert_eq!(dep_rel, "Providers/pu");

    // (ii) a declared spelling with `..` segments: the finding carries the normalised path.
    let s = layout(&[("a", "[dependencies]\npx = { path = \"../../../core/rust/../../providers/px\" }\n")]);
    let (_, _, _, folder, dep_rel) = violation(judge(&graph(&s.root).unwrap()));
    assert_eq!(folder, "providers");
    assert_eq!(dep_rel, "providers/px");

    // (iii) the pure judge needs no subprocess; a prefix that is not a whole component is not a folder.
    let member = |rel: &str| Member {
        name: "m".to_string(),
        manifest_rel: "core/rust/m/Cargo.toml".to_string(),
        deps: vec![Dep { name: "x".to_string(), kind: "normal".to_string(), path_rel: Some(rel.to_string()) }],
    };
    let (_, _, _, folder, dep_rel) = violation(judge(&Graph { members: vec![member("mcp/mx")] }));
    assert_eq!((folder.as_str(), dep_rel.as_str()), ("mcp", "mcp/mx"));
    assert_eq!(judge(&Graph { members: vec![member("plugins-docs/x")] }), Verdict::Clean);
}

#[test]
fn a_dependency_outside_the_repo_root_refuses_the_graph() {
    let s = layout(&[("a", "[dependencies]\nox = { path = \"../../../../outside/ox\" }\n")]);
    krate(&s.base.join("outside/ox"), "ox", "");
    let e = graph(&s.root).unwrap_err();
    assert!(e.contains("ox"), "{}", e);
    assert!(e.contains("outside the repo root"), "{}", e);
}

#[test]
fn an_unreadable_graph_refuses_and_never_passes() {
    // arm 1: no workspace manifest at all.
    let s = scratch();
    assert!(graph(&s.root).is_err());

    // arm 2: a declared dependency whose target is missing (cargo itself exits non-zero).
    let s = layout(&[("a", "[dependencies]\ngone = { path = \"../../../providers/gone\" }\n")]);
    assert!(graph(&s.root).is_err());

    // arm 3: the plain package IS the manifest at core/rust/Cargo.toml (no [workspace] of its own) and the enclosing
    // workspace at the scratch root LISTS "core/rust": cargo walks up and reports the outer root.
    let s = scratch();
    write(&s.root.join("Cargo.toml"), "[workspace]\nmembers = [\"core/rust\"]\nresolver = \"2\"\n");
    krate(&s.root.join("core/rust"), "hx", "");
    let outer = s.root.canonicalize().unwrap();
    let expected = outer.join("core/rust");
    let e = graph(&s.root).unwrap_err();
    assert!(e.contains("workspace root mismatch"), "{}", e);
    assert!(e.contains(&format!("cargo reports `{}`", outer.display())), "{}", e);
    assert!(e.contains(&format!("expected `{}`", expected.display())), "{}", e);

    // arm 3b: not listed in the outer workspace: cargo itself refuses (a different, cargo-failure refusal).
    let s = scratch();
    write(&s.root.join("Cargo.toml"), "[workspace]\nmembers = []\nresolver = \"2\"\n");
    krate(&s.root.join("core/rust"), "hx", "");
    let e = graph(&s.root).unwrap_err();
    assert!(!e.contains("workspace root mismatch"), "{}", e);
}

#[test]
fn the_first_finding_is_deterministic() {
    let s = layout(&[
        ("b", "[dependencies]\nmx = { path = \"../../../mcp/mx\" }\n"),
        ("a", "[dependencies]\npx = { path = \"../../../providers/px\" }\n"),
    ]);
    let g = graph(&s.root).unwrap();
    let first = judge(&g);
    let (member, dep, _, folder, _) = violation(judge(&g));
    assert_eq!((member.as_str(), dep.as_str(), folder.as_str()), ("a", "px", "providers"));
    assert_eq!(first, judge(&g));
    // declaration order must not matter to the pure judge either.
    let mut rev = g.clone();
    rev.members.reverse();
    assert_eq!(first, judge(&rev));
}

#[test]
fn the_real_repo_graph_is_clean_and_the_gate_file_loads() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let g = graph(&root).unwrap();
    let names: Vec<&str> = g.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["harness"]);
    assert_eq!(judge(&g), Verdict::Clean);

    let gates = harness::gates::Gates::load(&root).unwrap();
    assert_eq!(gates.ids(), ["cargo-test", "core-layering"]);
    let gate = gates.get("core-layering").unwrap();
    assert_eq!(gate.program, "ci/core-layering.sh");
    harness::gates::to_check(&root, gate).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.join("ci/core-layering.sh")).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "the check script must be executable");
    }
}

#[test]
fn scratch_roots_are_removed_after_the_run() {
    let s = layout(&[("a", "")]);
    let base = s.base.clone();
    assert!(base.exists());
    drop(s);
    assert!(!base.exists());

    // a failing assert while the guard is alive still cleans up.
    let seen: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let s = layout(&[("a", "")]);
        *seen.lock().unwrap() = Some(s.base.clone());
        assert_eq!(1, 2, "planted failure");
    }));
    assert!(r.is_err());
    let leaked = seen.lock().unwrap().clone().unwrap();
    assert!(!leaked.exists());
}
