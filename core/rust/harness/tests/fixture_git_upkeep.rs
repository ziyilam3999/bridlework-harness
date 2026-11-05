//! Ticket #94 guard: every scratch repo a test creates through `git init` must have git's own
//! background upkeep (auto maintenance, auto gc, gc --detach) turned OFF in that repo's own config,
//! before any commit. Git's detached upkeep can write under `.git` (e.g. `.git/objects/maintenance.lock`,
//! packed refs, commit-graph) at any moment, which flakes any test that snapshots the tree strictly.
//! PR 45/46 set the three keys at most init sites; nothing stopped a new site (or a cleanup) from
//! silently dropping them, so this test reads the test sources as text and fails naming the file and
//! line of every site that lacks a key. It is a source-scan tripwire over the call shapes used today
//! (`git(&x, &["init", ...])`), not a behaviour proof; fixture calls such as `manifest init` are not
//! git inits and must not match.

use std::fs;
use std::path::PathBuf;

const KEYS: [&str; 3] = ["maintenance.auto", "gc.auto", "gc.autoDetach"];

#[test]
fn every_scratch_git_init_disables_background_upkeep() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in ["tests", "src"] {
        for e in fs::read_dir(root.join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().map(|x| x == "rs").unwrap_or(false) {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(files.len() > 20, "the source scan found no files under {}: {}", root.display(), files.len());
    let mut bad: Vec<String> = Vec::new();
    let mut sites = 0usize;
    for f in &files {
        let text = fs::read_to_string(f).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // A site is a line whose code contains an array literal led by "init" (`["init"`); the
        // `manifest init` fixture calls never lead the array, so they do not match.
        let mut mine: Vec<usize> = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            if l.trim_start().starts_with("//") {
                continue;
            }
            if l.contains("[\"init\"") {
                mine.push(i);
            }
        }
        for (n, &i) in mine.iter().enumerate() {
            sites += 1;
            // The config must sit between THIS init and the next one, so a neighbour's config
            // can never cover an unconfigured site.
            let end = mine.get(n + 1).copied().unwrap_or(lines.len());
            let window: String = lines[i..end].join("\n");
            for key in KEYS {
                if !window.contains(key) {
                    bad.push(format!(
                        "{}:{}: git init without `{}` — set maintenance.auto=false, gc.auto=0 and gc.autoDetach=false in the repo's own config before the first commit",
                        f.display(),
                        i + 1,
                        key
                    ));
                }
            }
        }
    }
    assert!(sites > 20, "the scan matched no git init sites: {}", sites);
    assert!(
        bad.is_empty(),
        "{} upkeep key(s) missing across {} scanned sources:\n{}",
        bad.len(),
        files.len(),
        bad.join("\n")
    );
}
