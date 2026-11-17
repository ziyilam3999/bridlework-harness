//! Process-level tests for `harness ticket words-only`. The real binary runs as a child against a temp git
//! repo built with local git only (no network). Each refusal test checks the exact stderr line and an empty
//! stdout. No test waits on a clock.
#![cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const USAGE: &str = "usage: harness ticket words-only --repo-dir <absolute-dir> --from <full-sha> --to <full-sha>";
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("wonly-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn gitcmd(dir: &Path, args: &[&str]) -> Command {
    let contact = format!("user.email=t{}scratch", '@');
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str(), "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        c.env_remove(k);
    }
    c.stdin(Stdio::null());
    c
}

fn g(dir: &Path, args: &[&str]) -> String {
    let out = gitcmd(dir, args).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn commit(dir: &Path) -> String {
    g(dir, &["commit", "-q", "-m", "m"]);
    g(dir, &["rev-parse", "HEAD"])
}

fn commit_all(dir: &Path, files: &[(&str, &[u8])]) -> String {
    for (p, b) in files {
        write(dir, p, b);
    }
    g(dir, &["add", "-A"]);
    commit(dir)
}

struct Case {
    t: Tmp,
    dir: PathBuf,
    from: String,
    to: String,
}

fn init() -> (Tmp, PathBuf) {
    let t = Tmp::new();
    let dir = t.0.join("repo");
    fs::create_dir_all(&dir).unwrap();
    g(&dir, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(&dir, &["config", k, v]);
    }
    (t, dir)
}

/// Two commits: the first writes `a`, the second writes `b` over it.
fn case(a: &[(&str, &[u8])], b: &[(&str, &[u8])]) -> Case {
    let (t, dir) = init();
    let from = commit_all(&dir, a);
    let to = commit_all(&dir, b);
    Case { t, dir, from, to }
}

fn rs_case(old: &str, new: &str) -> Case {
    case(&[("a.rs", old.as_bytes())], &[("a.rs", new.as_bytes())])
}

fn md_case(path: &str, old: &str, new: &str) -> Case {
    case(&[(path, old.as_bytes())], &[(path, new.as_bytes())])
}

fn run_with(args: &[String], cwd: Option<&Path>, envs: &[(&str, &Path)]) -> Output {
    let mut c = Command::new(BIN);
    c.arg("ticket").arg("words-only").args(args).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn args_of(dir: &Path, from: &str, to: &str) -> Vec<String> {
    ["--repo-dir", dir.to_str().unwrap(), "--from", from, "--to", to].iter().map(|s| s.to_string()).collect()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn check_pass(o: &Output, from: &str, to: &str, n: usize) {
    assert_eq!(code(o), 0, "{}", err(o));
    assert_eq!(err(o), "");
    assert_eq!(out(o), format!("words-only yes from={from} to={to} files={n}\n"));
}

fn check_refused(o: &Output, line: &str) {
    assert_eq!(code(o), 1, "{}", err(o));
    assert_eq!(out(o), "");
    assert_eq!(err(o), format!("harness: words-only refused: {line}\n"));
}

fn check_exit2(o: &Output, line: &str) {
    assert_eq!(code(o), 2);
    assert_eq!(out(o), "");
    assert_eq!(err(o), format!("{line}\n"));
}

impl Case {
    fn run(&self) -> Output {
        run_with(&args_of(&self.dir, &self.from, &self.to), None, &[])
    }
    fn pass(&self, n: usize) {
        check_pass(&self.run(), &self.from, &self.to, n);
    }
    fn refused(&self, line: &str) {
        check_refused(&self.run(), line);
    }
}

const FN_BODY: &str = "fn f() {\n    // one\n    x();\n}\n";

// ---- passes ----

#[test]
fn the_worked_case() {
    let c = case(
        &[("src/a.rs", FN_BODY.as_bytes()), ("docs/g.md", b"# G\none\n")],
        &[("src/a.rs", b"fn f() {\n    // two\n    x();\n}\n"), ("docs/g.md", b"# G\ntwo\n")],
    );
    c.pass(2);
}

#[test]
fn a_line_comment_edit_passes() {
    rs_case(FN_BODY, "fn f() {\n    // two words\n    x();\n}\n").pass(1);
}

#[test]
fn added_and_removed_comment_lines_pass() {
    rs_case("fn f() {\n    // a\n    x();\n}\n", "// top\nfn f() {\n    x();\n    // tail\n    // more\n}\n").pass(1);
}

#[test]
fn a_trailing_comment_edit_passes() {
    rs_case("fn f() {\n    x(); // a\n}\n", "fn f() {\n    x(); // b\n}\n").pass(1);
}

#[test]
fn a_doc_comment_edit_is_refused() {
    rs_case("//! Crate.\n/// Does a thing.\nfn f() {}\n", "//! Crate text.\n/// Does another thing.\n///\n/// More words.\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn a_four_slash_and_triple_star_comment_edit_passes() {
    rs_case("//// a\n/*** x ***/\n/**/\nfn f() {}\n", "//// b\n/*** y ***/\n/**/\nfn f() {}\n").pass(1);
}

#[test]
fn a_block_comment_edit_passes() {
    rs_case("/* a */\nfn f() {\n    x(); /* b\n    c */\n}\n", "/* changed */\nfn f() {\n    x(); /* d */\n}\n").pass(1);
}

#[test]
fn a_nested_block_comment_edit_passes() {
    rs_case("/* a /* b */ c */\nfn f() {}\n", "/* a /* x */ y */\nfn f() {}\n").pass(1);
}

#[test]
fn a_docs_markdown_edit_passes() {
    md_case("docs/x.md", "# X\nold\n", "# X\nnew text\n\nmore\n").pass(1);
}

#[test]
fn a_root_readme_edit_passes() {
    md_case("README.md", "old\n", "new\n").pass(1);
}

#[test]
fn works_on_a_linked_work_tree() {
    let c = rs_case(FN_BODY, "fn f() {\n    // two\n    x();\n}\n");
    let wt = c.t.0.join("wt");
    g(&c.dir, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap(), "HEAD"]);
    check_pass(&run_with(&args_of(&wt, &c.from, &c.to), None, &[]), &c.from, &c.to, 1);
}

// ---- hostile ----

#[test]
fn a_code_edit_is_refused() {
    rs_case("fn f() { x(1); }\n", "fn f() { x(2); }\n").refused("a.rs code-changed");
}

#[test]
fn an_indent_change_is_refused() {
    rs_case("fn f() {\n    x();\n}\n", "fn f() {\n  x();\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_blank_line_added_between_code_lines_is_refused() {
    rs_case("fn f() {\n    x();\n    y();\n}\n", "fn f() {\n    x();\n\n    y();\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_doctest_edit_is_refused() {
    rs_case("/// Intro.\n///\n/// ```\n/// let a = 1;\n/// ```\nfn f() {}\n", "/// Intro.\n///\n/// ```\n/// let a = 2;\n/// ```\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn a_new_fence_in_a_doc_comment_is_refused() {
    rs_case("/// Intro.\nfn f() {}\n", "/// Intro.\n/// ```\n/// x();\n/// ```\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn a_tilde_fence_doctest_edit_is_refused() {
    rs_case("/// Intro.\n///\n/// ~~~\n/// let a = 1;\n/// ~~~\nfn f() {}\n", "/// Intro.\n///\n/// ~~~\n/// let a = 2;\n/// ~~~\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn an_indented_doctest_edit_is_refused() {
    rs_case("/// Intro.\n///\n///     let a = 1;\nfn f() {}\n", "/// Intro.\n///\n///     let a = 2;\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn a_block_doc_comment_edit_is_refused() {
    rs_case("/** a word */\nfn f() {}\n", "/** a nord */\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn a_inner_line_doc_comment_edit_is_refused() {
    rs_case("//! Crate.\nfn f() {}\n", "//! Crate text.\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn an_inner_block_doc_comment_edit_is_refused() {
    rs_case("/*! a word */\nfn f() {}\n", "/*! a nord */\nfn f() {}\n").refused("a.rs code-changed");
}

// Doctest shapes that a fence rule missed: each is a doc comment change, so each is code.
fn doc_edit(line: &str) -> Case {
    rs_case(&format!("/// text\n///\n{line}\nfn f() {{}}\n"), &format!("/// text\n///\n{}\nfn f() {{}}\n", line.replace("1, 1", "1, 2")))
}

#[test]
fn a_doc_attribute_fence_around_a_doc_line_is_refused() {
    rs_case("#[doc = \"```\"]\n/// assert_eq!(1, 1);\n#[doc = \"```\"]\nfn f() {}\n", "#[doc = \"```\"]\n/// assert_eq!(1, 2);\n#[doc = \"```\"]\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn indented_code_inside_a_quote_is_refused() {
    doc_edit("///  >     assert_eq!(1, 1);").refused("a.rs code-changed");
}

#[test]
fn two_blanks_and_a_tab_in_a_doc_line_is_refused() {
    doc_edit("///  \tassert_eq!(1, 1);").refused("a.rs code-changed");
}

#[test]
fn indented_code_after_a_list_marker_is_refused() {
    doc_edit("/// -     assert_eq!(1, 1);").refused("a.rs code-changed");
}

#[test]
fn indented_code_after_a_numbered_marker_is_refused() {
    doc_edit("/// 1.     assert_eq!(1, 1);").refused("a.rs code-changed");
}

#[test]
fn a_new_doctest_in_a_file_with_no_fence_is_refused() {
    rs_case("/// text\nfn f() {}\n", "/// text\n///\n/// >     assert!(false);\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn a_quote_before_a_digit_is_unparsed() {
    rs_case("fn f() { m!('1); } // a\n", "fn f() { m!('1); } // b\n").refused("a.rs unparsed");
}

#[test]
fn slashes_inside_a_string_are_code() {
    rs_case("fn f() {\n    let s = \"a // b\";\n}\n", "fn f() {\n    let s = \"a // c\";\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_comment_like_line_in_a_raw_string_is_code() {
    // Read as a plain string, the inner quote would end it and the comment-like line would be removed.
    rs_case("fn f() {\n    let s = r#\"a\"\n// a\nb \"x\"#;\n}\n", "fn f() {\n    let s = r#\"a\"\n// b\nb \"x\"#;\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_block_opener_inside_a_string_is_code() {
    rs_case("fn f() {\n    let a = \"/*\"; let b = 1;\n}\n", "fn f() {\n    let a = \"/*\"; let b = 2;\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_block_opener_inside_a_line_comment_is_ignored() {
    rs_case("fn f() {\n    // has /*\n    let a = 1;\n}\n", "fn f() {\n    // has /*\n    let a = 2;\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_removed_block_comment_does_not_join_tokens() {
    rs_case("fn f() -> u8 {\n    return/*c*/x;\n}\n", "fn f() -> u8 {\n    returnx;\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_raw_identifier_is_not_a_raw_string() {
    rs_case("fn f() {\n    let r#type = 1; let s = \"a // b\";\n}\n", "fn f() {\n    let r#type = 1; let s = \"a // c\";\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_byte_string_is_code() {
    rs_case("fn f() {\n    let s = b\"a // b\";\n}\n", "fn f() {\n    let s = b\"a // c\";\n}\n").refused("a.rs code-changed");
}

#[test]
fn a_char_literal_quote_does_not_open_a_string() {
    rs_case("fn f() {\n    let c = '\"'; // a\n}\n", "fn f() {\n    let c = '\"'; // b\n}\n").pass(1);
    rs_case("fn f() {\n    let c = '\"'; x(1);\n}\n", "fn f() {\n    let c = '\"'; x(2);\n}\n").refused("a.rs code-changed");
}

// An escaped quote does not end the string: a lexer that ends it there reads the rest as a comment.
#[test]
fn a_code_edit_after_an_escaped_quote_in_a_string_is_refused() {
    rs_case("fn f() {\n    let s = \"a\\\" // b\";\n}\n", "fn f() {\n    let s = \"a\\\" // c\";\n}\n").refused("a.rs code-changed");
}

// The char literal '\'' holds an escaped quote: a lexer that ends it at that quote leaves a stray quote.
#[test]
fn a_char_literal_of_an_escaped_quote_is_read_to_its_end() {
    rs_case("fn f() {\n    let q = '\\''; // a\n}\n", "fn f() {\n    let q = '\\''; // b\n}\n").pass(1);
    rs_case("fn f() {\n    let q = '\\''; x(1);\n}\n", "fn f() {\n    let q = '\\''; x(2);\n}\n").refused("a.rs code-changed");
}

// Three quotes of lifetimes: a lexer that reads each quote as a char opener pairs the first two, opens a
// third literal that does not close and answers unparsed (exit 1) where the right answer is a pass.
#[test]
fn a_lifetime_is_not_a_char_literal() {
    rs_case("fn f<'a>(x: &'a u8, y: &'a u8) {} // one\n", "fn f<'a>(x: &'a u8, y: &'a u8) {} // two\n").pass(1);
}

// The lifetime 'a is one token: a lexer that reads 'a as a char opener and falls back to a lifetime still
// reads the quote in the string wrong.
#[test]
fn a_lifetime_before_a_string_with_a_quote_is_code() {
    rs_case(
        "fn f<'a>(t: &'a str) { let u: &'a str = \"q'; // x\"; }\n",
        "fn f<'a>(t: &'a str) { let u: &'a str = \"q'; // y\"; }\n",
    )
    .refused("a.rs code-changed");
}

// A token tail is code: a following r" is a plain string with escapes, not a raw string.
fn tail_case(line: &str) -> Case {
    rs_case(&format!("{line}\n"), &format!("{}\n", line.replace("// x", "// y")))
}

#[test]
fn a_char_literal_suffix_r_does_not_open_a_raw_string() {
    tail_case("const S: &str = stringify!('a'r\"q\\\" // x\");").refused("a.rs code-changed");
}

#[test]
fn a_lifetime_named_r_does_not_open_a_raw_string() {
    tail_case("const S: &str = stringify!('r\"q\\\" // x\");").refused("a.rs code-changed");
}

#[test]
fn a_string_literal_suffix_r_does_not_open_a_raw_string() {
    tail_case("const S: &str = stringify!(\"z\"r\"q\\\" // x\");").refused("a.rs code-changed");
}

#[test]
fn a_raw_identifier_named_r_does_not_open_a_raw_string() {
    tail_case("const S: &str = stringify!(r#r\"q\\\" // x\");").refused("a.rs code-changed");
}

#[test]
fn a_crlf_line_end_change_is_refused() {
    rs_case("fn f() {\r\n    x();\r\n}\r\n", "fn f() {\r\n    x();\n}\r\n").refused("a.rs code-changed");
}

#[test]
fn a_code_line_with_a_trailing_comment_changed_from_crlf_to_lf_is_refused() {
    rs_case("fn f() {\r\n    x(); // a\r\n}\r\n", "fn f() {\r\n    x(); // a\n}\r\n").refused("a.rs code-changed");
}

#[test]
fn a_fence_after_a_list_marker_is_refused() {
    rs_case("/// - ```\n///   assert_eq!(1, 1);\n///   ```\nfn f() {}\n", "/// - ```\n///   assert_eq!(1, 2);\n///   ```\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn a_fence_after_a_quote_marker_is_refused() {
    rs_case("/// > ```\n/// > assert_eq!(1, 1);\n/// > ```\nfn f() {}\n", "/// > ```\n/// > assert_eq!(1, 2);\n/// > ```\nfn f() {}\n").refused("a.rs code-changed");
}

#[test]
fn an_unterminated_string_is_unparsed() {
    rs_case("fn f() {\n    let s = \"abc\";\n}\n", "fn f() {\n    let s = \"abc;\n}\n").refused("a.rs unparsed");
}

#[test]
fn an_unterminated_block_comment_is_unparsed() {
    rs_case("/* a */\nfn f() {}\n", "/* a\nfn f() {}\n").refused("a.rs unparsed");
}

#[test]
fn a_shell_file_is_refused() {
    case(&[("run.sh", b"echo a\n")], &[("run.sh", b"echo b\n")]).refused("run.sh not-words-file");
    case(&[("tool", b"#!/bin/bash\necho a\n")], &[("tool", b"#!/bin/bash\necho b\n")]).refused("tool not-words-file");
}

#[test]
fn markdown_outside_docs_is_refused() {
    md_case("gates/README.md", "a\n", "b\n").refused("gates/README.md not-words-file");
    md_case("a/CLAUDE.md", "a\n", "b\n").refused("a/CLAUDE.md not-words-file");
}

#[test]
fn a_capitalised_docs_folder_is_refused() {
    md_case("Docs/x.md", "a\n", "b\n").refused("Docs/x.md not-words-file");
}

#[test]
fn a_new_file_is_refused() {
    case(&[("a.rs", b"fn f() {}\n")], &[("b.rs", b"// c\n")]).refused("b.rs new-file");
}

#[test]
fn a_deleted_file_is_refused() {
    let (t, dir) = init();
    let from = commit_all(&dir, &[("docs/x.md", b"a\n"), ("docs/y.md", b"a\n")]);
    fs::remove_file(dir.join("docs/x.md")).unwrap();
    g(&dir, &["add", "-A"]);
    let c = Case { t, dir: dir.clone(), from, to: commit(&dir) };
    c.refused("docs/x.md deleted-file");
}

#[test]
fn a_rename_is_refused() {
    let (t, dir) = init();
    let from = commit_all(&dir, &[("docs/a.md", b"one two three four\n")]);
    g(&dir, &["mv", "docs/a.md", "docs/b.md"]);
    let c = Case { t, dir: dir.clone(), from, to: commit(&dir) };
    c.refused("docs/a.md deleted-file");
}

#[test]
fn a_mode_change_is_refused() {
    let (t, dir) = init();
    let from = commit_all(&dir, &[("docs/x.md", b"a\n")]);
    g(&dir, &["update-index", "--chmod=+x", "docs/x.md"]);
    let c = Case { t, dir: dir.clone(), from, to: commit(&dir) };
    c.refused("docs/x.md mode");
}

#[test]
fn a_symbolic_link_is_refused() {
    let (t, dir) = init();
    fs::create_dir_all(dir.join("docs")).unwrap();
    symlink("a", dir.join("docs/l.md")).unwrap();
    g(&dir, &["add", "-A"]);
    let from = commit(&dir);
    fs::remove_file(dir.join("docs/l.md")).unwrap();
    symlink("b", dir.join("docs/l.md")).unwrap();
    g(&dir, &["add", "-A"]);
    let c = Case { t, dir: dir.clone(), from, to: commit(&dir) };
    c.refused("docs/l.md link");
}

#[test]
fn a_submodule_entry_is_refused() {
    let (t, dir) = init();
    g(&dir, &["update-index", "--add", "--cacheinfo", &format!("160000,{},sub", "1".repeat(40))]);
    let from = commit(&dir);
    g(&dir, &["update-index", "--add", "--cacheinfo", &format!("160000,{},sub", "2".repeat(40))]);
    let c = Case { t, dir: dir.clone(), from, to: commit(&dir) };
    c.refused("sub submodule");
}

#[test]
fn a_non_utf8_rust_file_is_refused() {
    case(&[("a.rs", b"// a\nfn f() {}\n")], &[("a.rs", b"// \xff\nfn f() {}\n")]).refused("a.rs not-utf8");
}

#[test]
fn a_blob_over_1_mib_is_refused() {
    let big = "a".repeat(1024 * 1024 + 1);
    case(&[("docs/big.md", b"small\n")], &[("docs/big.md", big.as_bytes())]).refused("docs/big.md too-large");
}

/// A commit pair that changes `n` docs files.
fn many_files(n: usize) -> Case {
    let (t, dir) = init();
    for i in 0..n {
        write(&dir, &format!("docs/f{i:03}.md"), b"a\n");
    }
    g(&dir, &["add", "-A"]);
    let from = commit(&dir);
    for i in 0..n {
        write(&dir, &format!("docs/f{i:03}.md"), b"b\n");
    }
    g(&dir, &["add", "-A"]);
    Case { t, dir: dir.clone(), from, to: commit(&dir) }
}

#[test]
fn too_many_files_is_refused() {
    many_files(201).refused("- too-many-files");
}

#[test]
fn exactly_200_files_are_not_refused_on_count() {
    many_files(200).pass(200);
}

#[test]
fn a_control_character_path_is_not_printed() {
    let o = case(&[("docs/a\nb.md", b"a\n")], &[("docs/a\nb.md", b"b\n")]).run();
    check_refused(&o, "<unprintable path> bad-path");
}

#[test]
fn not_an_ancestor_is_refused() {
    let (t, dir) = init();
    let base = commit_all(&dir, &[("docs/x.md", b"a\n")]);
    let from = commit_all(&dir, &[("docs/x.md", b"b\n")]);
    g(&dir, &["checkout", "-q", "--detach", &base]);
    let to = commit_all(&dir, &[("docs/x.md", b"c\n")]);
    Case { t, dir, from, to }.refused("- not-ancestor");
}

#[test]
fn the_same_commit_is_refused() {
    let c = rs_case("a\n", "b\n");
    check_refused(&run_with(&args_of(&c.dir, &c.to, &c.to), None, &[]), "- no-change");
}

#[test]
fn replace_refs_are_ignored() {
    let c = rs_case("fn f() { x(1); }\n", "fn f() { x(2); }\n");
    let old = g(&c.dir, &["rev-parse", &format!("{}:a.rs", c.from)]);
    let new = g(&c.dir, &["rev-parse", &format!("{}:a.rs", c.to)]);
    g(&c.dir, &["replace", &new, &old]);
    c.refused("a.rs code-changed");
}

#[test]
fn a_git_dir_in_the_environment_is_ignored() {
    let c = rs_case(FN_BODY, "fn f() {\n    // two\n    x();\n}\n");
    let other = c.t.0.join("other");
    fs::create_dir_all(&other).unwrap();
    g(&other, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(&other, &["config", k, v]);
    }
    let objs = c.t.0.join("emptyobjs");
    fs::create_dir_all(&objs).unwrap();
    let gd = other.join(".git");
    let o = run_with(&args_of(&c.dir, &c.from, &c.to), None, &[("GIT_DIR", gd.as_path()), ("GIT_OBJECT_DIRECTORY", objs.as_path())]);
    check_pass(&o, &c.from, &c.to, 1);
}

#[test]
fn a_gitattributes_filter_is_not_run() {
    // The attributes file is committed and the filter is required: a blob read through it would fail the call.
    let attrs: &[u8] = b"*.rs filter=boom diff=boom text eol=crlf\n";
    let c = case(&[("a.rs", b"fn f() { x(1); } // a\n"), (".gitattributes", attrs)], &[("a.rs", b"fn f() { x(1); } // b\n"), (".gitattributes", attrs)]);
    for (k, v) in [
        ("filter.boom.clean", "false"),
        ("filter.boom.smudge", "false"),
        ("filter.boom.required", "true"),
        ("diff.boom.textconv", "false"),
    ] {
        g(&c.dir, &["config", k, v]);
    }
    c.pass(1);
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(base: &Path, p: &Path, m: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            let rel = path.strip_prefix(base).unwrap().to_path_buf();
            if e.file_type().unwrap().is_dir() {
                m.insert(rel, Vec::new());
                walk(base, &path, m);
            } else {
                m.insert(rel, fs::read(&path).unwrap_or_default());
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

#[test]
fn is_read_only() {
    let c = rs_case(FN_BODY, "fn f() {\n    // two\n    x();\n}\n");
    let cwd = c.t.0.join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    let (before, before_cwd) = (snapshot(&c.dir), snapshot(&cwd));
    let o = run_with(&args_of(&c.dir, &c.from, &c.to), Some(&cwd), &[]);
    check_pass(&o, &c.from, &c.to, 1);
    assert!(before == snapshot(&c.dir), "the repo changed");
    assert!(before_cwd == snapshot(&cwd), "the working folder changed");
}

#[test]
fn a_blob_missing_from_a_partial_clone_is_not_fetched() {
    let c = rs_case(FN_BODY, "fn f() {\n    // two\n    x();\n}\n");
    g(&c.dir, &["config", "uploadpack.allowFilter", "true"]);
    let clone = c.t.0.join("clone");
    let url = format!("file://{}", c.dir.display());
    g(&c.t.0, &["clone", "-q", "--no-checkout", "--filter=blob:none", &url, clone.to_str().unwrap()]);
    let before = snapshot(&clone.join(".git/objects"));
    let o = run_with(&args_of(&clone, &c.from, &c.to), None, &[]);
    check_exit2(&o, "harness: error: a git call failed");
    assert!(before == snapshot(&clone.join(".git/objects")), "the object store changed");
}

// ---- usage and exit 2 ----

fn sv(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn usage_errors_exit_2_with_the_words_only_usage() {
    let sha = "a".repeat(40);
    let (d, f, t) = ("/x/repo", sha.as_str(), sha.as_str());
    let from_eq = format!("--from={f}");
    let cases: Vec<Vec<String>> = vec![
        vec![],
        sv(&["--from", f, "--to", t]),
        sv(&["--repo-dir", d, "--to", t]),
        sv(&["--repo-dir", d, "--from", f]),
        sv(&["--repo-dir", d, &from_eq, "--to", t]),
        sv(&["--repo-dir", d, "--from", f, "--to"]),
        sv(&["--repo-dir", d, "--from", f, "--to", t, "--to", t]),
        sv(&["--repo-dir", d, "--from", f, "--to", t, "extra"]),
        sv(&["--repo-dir", d, "--from", f, "--to", t, "--other", "x"]),
        sv(&["extra", "--repo-dir", d, "--from", f, "--to", t]),
    ];
    for a in cases {
        check_exit2(&run_with(&a, None, &[]), USAGE);
    }
}

#[test]
fn a_short_or_upper_case_sha_exits_2() {
    let c = rs_case("a\n", "b\n");
    let upper = c.to.to_uppercase();
    for bad in [&c.to[..39], &c.to[..7], upper.as_str(), &format!("{}0", c.to)] {
        check_exit2(&run_with(&args_of(&c.dir, &c.from, bad), None, &[]), USAGE);
        check_exit2(&run_with(&args_of(&c.dir, bad, &c.to), None, &[]), USAGE);
    }
}

#[test]
fn a_relative_or_dot_dot_repo_dir_exits_2() {
    let c = rs_case("a\n", "b\n");
    let dotted = format!("{}/../repo", c.dir.display());
    for bad in ["repo", "./repo", dotted.as_str()] {
        check_exit2(&run_with(&sv(&["--repo-dir", bad, "--from", &c.from, "--to", &c.to]), None, &[]), USAGE);
    }
}

#[test]
fn a_repo_dir_that_is_not_a_repo_exits_2() {
    let t = Tmp::new();
    let empty = t.0.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let sha = "a".repeat(40);
    check_exit2(&run_with(&args_of(&empty, &sha, &sha), None, &[]), "harness: error: the repo dir is not a git repo");
    check_exit2(&run_with(&args_of(&t.0.join("missing"), &sha, &sha), None, &[]), "harness: error: the repo dir is missing or not a folder");
    let file = t.0.join("file");
    fs::write(&file, b"x").unwrap();
    check_exit2(&run_with(&args_of(&file, &sha, &sha), None, &[]), "harness: error: the repo dir is missing or not a folder");
}

#[test]
fn an_unknown_commit_exits_2() {
    let c = rs_case("a\n", "b\n");
    let unknown = "b".repeat(40);
    check_exit2(&run_with(&args_of(&c.dir, &c.from, &unknown), None, &[]), "harness: error: --to is not a commit of the repo");
    check_exit2(&run_with(&args_of(&c.dir, &unknown, &c.to), None, &[]), "harness: error: --from is not a commit of the repo");
}

#[test]
fn a_tree_sha_exits_2() {
    let c = rs_case("a\n", "b\n");
    let tree = g(&c.dir, &["rev-parse", &format!("{}^{{tree}}", c.from)]);
    check_exit2(&run_with(&args_of(&c.dir, &tree, &c.to), None, &[]), "harness: error: --from is not a commit of the repo");
}
