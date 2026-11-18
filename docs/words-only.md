# harness ticket words-only

Reads two commits of one repo and exits 0 only when each change between them is plain comment text in a Rust file
(a doc comment is code) or text of a docs Markdown file. Else it refuses. A form the lexer cannot read with
certainty is refused as `unparsed`, not passed.

## Synopsis

```
harness ticket words-only
  --repo-dir <absolute-dir>
  --from <full-sha>
  --to <full-sha>
```

- `--repo-dir`: a git repo or a linked work tree. Absolute, with no `..` component. Git finds the repo upward
  from it, so a folder inside a repo is accepted.
- `--from`: 40 lowercase hex characters, the older commit.
- `--to`: 40 lowercase hex characters, the newer commit.

The three flags are required, each once. An unknown flag (also `--from=x`), a flag with no value, a flag
given twice or a word that is not a flag is exit 2 with the usage line on stderr and stdout empty.

## Words files

- a path ending in `.rs`
- a path under `docs/` ending in `.md`
- the root files `README.md`, `CONTRIBUTING.md` and `SECURITY.md`

Paths compare byte for byte (`Docs/x.md` is not under `docs/`). A shell file, a file with no name ending and
a Markdown file elsewhere are not words files.

## How git is read

Each git call runs on the repo dir, removes each inherited variable whose name starts with `GIT_`, gets no
stdin and runs with `--no-replace-objects` and `--no-lazy-fetch`. It reads only: no fetch, no ref, config,
index or work tree write. A blob that is missing from the object store (a partial clone) is not fetched: the
call fails when that blob is read, and the answer is exit 2 `harness: error: a git call failed` (an earlier
refused entry gives exit 1 first). Changed entries come from a raw tree
compare with renames off and `--ignore-submodules=none`, so a `submodule.<name>.ignore` setting of
`.gitmodules` does not hide a submodule entry. File contents come from the object store as raw bytes, so no text conversion,
filter or diff driver runs. The work tree is not read.

The verb needs a git that accepts `--no-lazy-fetch`. On an older git each call fails, and the verb fails
with exit 2 (`harness: error: the repo dir is not a git repo`).

## Order of steps

The first failing step gives one stderr line and stops.

1. Usage (exit 2).
2. Setup (exit 2, `harness: error: <what>`, no path printed): the repo dir is missing or not a folder, it is
   not a git repo, git cannot run or a call fails, `--from` or `--to` is not a commit of the repo.
3. `--from` equal to `--to`: `no-change`.
4. `--from` is not an ancestor of `--to`: `not-ancestor`.
5. More than 200 changed entries: `too-many-files`.
6. Each changed entry, in the order git lists them (see the classes).
7. A `.md` words file passes on any text change. A `.rs` file passes when the two versions are equal after the
   lexer removes plain comments (doc comments stay).
8. Done: exit 0.

A refusal is exit 1 with one stderr line `harness: words-only refused: <path or -> <class>` and stdout empty.

## Classes

- `no-change`: `--from` and `--to` are one commit (path `-`).
- `not-ancestor`: `--from` is not an ancestor of `--to` (path `-`).
- `too-many-files`: more than 200 changed entries (path `-`).
- `bad-path`: a path that is not UTF-8 or holds a control character. The line says `<unprintable path>`, not
  the name.
- `new-file`: an added path.
- `deleted-file`: a deleted path. A rename shows as a deleted and an added path, so the first one in git order
  is named.
- `mode`: a mode change or a type change.
- `link`: a symbolic link on either side.
- `submodule`: a submodule entry on either side.
- `not-words-file`: the path is not a words file.
- `too-large`: either blob is over 1 MiB. It is refused, not cut.
- `not-utf8`: either blob is not UTF-8.
- `unparsed`: the lexer cannot read a `.rs` version to its end (an unterminated string, char literal or
  block comment), or the version holds a form it does not read: a raw lifetime or a shebang
  first line (see the lexer part).
- `code-changed`: the two `.rs` versions differ after plain comments are removed.

## What the lexer removes and keeps

The lexer reads each `.rs` version and removes plain comments:

- a line comment `//` to its line end, also `////` (a `/*` inside it opens nothing)
- a block comment `/* */`, with nesting, also `/**/` and `/***`

It keeps each doc comment, whole, so a change there is a code change:

- a line doc comment `///` (not `////`) or `//!`
- a block doc comment `/** */` or `/*! */`

Why: rustdoc may run the text of a doc comment as a doctest, and a doctest can be written in many Markdown
shapes (a fence, an indented block, a block after a list or quote marker). Telling those shapes apart is not
attempted, so a doc comment wording fix is not words only. A doc comment in any file is kept, with or
without a fence.

Removing a plain comment also removes the blanks before it on its line. A block comment with a non-blank byte
before it (ahead of those blanks) and a non-blank byte right after it is replaced by one blank, so two
tokens are not joined (a CR or LF right after the comment counts as no byte). A line left with only blanks is dropped with its newline when it held comment text.
A line that held no comment text is kept as it is, blank or not. A CR before an LF stays in the code, so a
line end change on a code line is a code change.

The lexer reads string literals with escapes, byte strings, raw strings (`r"..."`, `r#"..."#` with any
number of `#`), byte and C string forms of both, raw identifiers such as `r#type` (not a raw string), char
and byte literals (with escapes such as a quote, `\n` and `\u{...}`), and lifetimes and loop labels. A quote
starts a char literal when the next character is a backslash, or when the character after the next one is
a quote. Else it is a lifetime or label only when the next character is an ASCII letter, an underscore or a
non-ASCII character; any other quote (for example `'1`) is `unparsed`.

Some tails are read as code with their token, as rustc reads them, so a following `r"` is a plain string with
escapes and not a raw string: the whole name of a lifetime or label (`'r`), the identifier suffix right after
the end of a string, byte string, C string, raw string, char or byte literal (`'a'r`, `"z"r`), and the name
after `r#` in a raw identifier (`r#r`).

Two forms are refused as `unparsed`, because the lexer cannot tell how rustc reads what follows:

- a raw lifetime `'r#name` (the lifetime `'r` followed by `#`), which for a name `r`, `br` or `cr` would
  let a following string be read as a raw string
- a shebang first line: the file starts with `#!` (after an optional BOM) and the next non-blank byte is not
  `[`. rustc drops that line, so a `/*` or a quote in it must not open a comment or string here. A file that
  starts with an inner attribute such as `#![allow(dead_code)]` is not a shebang file.

## The ok line

Exit 0 with stderr empty and one stdout line:

```
words-only yes from=<from> to=<to> files=<count of changed entries>
```

## What it does not do

- It decides no review and marks no finding. A reviewer marks a finding as wording only. A caller uses the
  answer as one input beside the reviewer mark.
- It does not replace the gate, because some checks read comment and docs text. As of 1ef1556,
  `ci/lint-rust-tests.sh` and `ci/lint-fixtures.sh` read a code line with a trailing comment and block
  comment lines, and `tests/intake.rs` runs that lint on its own file. `tests/manifest.rs` reads
  `docs/resource-manifest.md`. `ci/install-guide-smoke.sh` reads `README.md` and `docs/install.md`. A comment
  edit can fail a lint.
- A doc comment wording fix (`///`, `//!`, `/** */`, `/*! */`) is not words only: it is `code-changed`.
- A words-only answer can still go with a failing build or test: some checks read comment or docs text (see
  above), and the gate catches those. A check that reads a plain comment can fail after a plain comment edit.
- An added or removed comment line moves `line!()` values and panic locations, and a plain comment edit
  that changes the line count can change them. `column!()` can move with an edit of a block comment inside a
  line, as `line!()` can with a line change.
- Two commits with equal trees (for example an empty commit) pass with `files=0`.
- It says nothing about whether a sentence is true.
- A `.md` words file passes on any change, so a `.md` file pulled into Rust docs with `include_str!` would let
  a doctest edit pass. No `.rs` file does that today.
- It reads shell files as `not-words-file` (a later change).
- It reads two commits, not the work tree.
- A fix round that was rebased onto a new base is refused as `not-ancestor`.

## Seeded tests

`core/rust/harness/src/seeded.rs` feeds the reader random inputs from a fixed seed list (a std-only splitmix64
generator, no clock and no environment value, so two runs see the same inputs). W1: raw bytes do not panic and
give the same answer twice. W2: text with no plain comment is unchanged. W3: a plain `//` comment added at a line
end is removed, and a line of only blanks and a plain comment is dropped. A failing case panics with its seed, its
case number and the input as escaped bytes; to replay it, add a temporary test that calls `replay(w2, <seed>,
<case>)` (use the property that failed) and run `cargo test --lib seeded`. Run the whole set with
`cargo test --locked --offline --manifest-path core/rust/Cargo.toml --lib seeded`.
