# Cutting a release

A release is cut by CI when a version tag is pushed. Nothing is built or published from a laptop.

Installing a release: see `docs/install.md`.

## What is published

The `plugins/claude` bundle at the tag, as one tarball (`bridlework-harness-claude-plugin-X.Y.Z.tar.gz`), the engine as a
second tarball (`bridlework-harness-engine-X.Y.Z-<triple>.tar.gz`), plus `SHA256SUMS`.
The release notes are a fixed template. Both tarballs are reproducible: the same tag and the same engine bytes give the same bytes.

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

The `harness` engine ships as `bridlework-harness-engine-<version>-<triple>.tar.gz` (the release job runs on linux x86_64, so
the published asset is `x86_64-unknown-linux-gnu`). It holds `harness` and `LICENSE`. It is the same binary that ran the scan,
and the scan reads the tarball member like every other file. The fixture generators (`harness fixtures ...`) are compiled only
into debug builds, and the detector's search strings are stored encoded, so a release build holds no test marker. Point
`HARNESS_BIN` at the unpacked `harness`, or build it from source (`cargo build --release --locked` in `core/rust`).

## Tests

`sh ci/release-smoke.sh all` runs the workflow's own steps in scratch repos with a dummy deny list (also run by `rust-test`).
