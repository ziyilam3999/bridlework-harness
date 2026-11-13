# Cutting a release

A release is cut by CI when a version tag is pushed. Nothing is built or published from a laptop.

## What is published

The `plugins/claude` bundle at the tag, as one tarball (`bridlework-harness-claude-plugin-X.Y.Z.tar.gz`), plus `SHA256SUMS`.
The release notes are a fixed template. The tarball is reproducible: the same tag gives the same bytes.

## Steps

1. In one PR, bump `version` in `core/rust/harness/Cargo.toml` and regenerate `plugins/claude/.claude-plugin/plugin.json`
   with `harness plugin plugin-json`. Merge it.
2. From a worktree at `origin/master` (never the primary clone), tag and push:

       git tag -a vX.Y.Z -m vX.Y.Z
       git push origin vX.Y.Z

Only strict `vX.Y.Z` tags start the job (no pre-release suffix).

## What the job checks

The `scan` job refuses (and nothing is published) when any of these holds:

- the tag is not strict semver, or differs from the Cargo package version or the `plugin.json` version;
- `harness plugin check` fails;
- the privacy scan (`ci/privacy-release-artifacts.sh`) finds anything in a file name or in the content of any dist file or
  tarball member, or the list secret is missing or blank.

Only after `scan` passes does the `publish` job start. It is the only job with write access. It runs no code from the tagged
tree: it checks `SHA256SUMS` and the file set, then calls `gh release create`. Publishing a tag that already has a release
fails; it never overwrites.

A refused run shows `surface:class-id` lines and the item index only, never the matched content.

## Engine binary

The `harness` engine binary is not shipped yet (it carries the scanner's own test strings, so it cannot pass the scan).
Build it from source at the tag (`cargo build --release --locked` in `core/rust`) and point `HARNESS_BIN` at it.

## Tests

`sh ci/release-smoke.sh all` runs the workflow's own steps in scratch repos with a dummy deny list (also run by `rust-test`).
