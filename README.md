# harness

A small privacy gate for this repository. It scans what is about to be pushed
and refuses the push when a value of a protected class is found.

## Map

- `core/rust/` : Rust workspace. The `harness` crate holds the scan engine
  (std only, no external crates).
- `.githooks/` : the pre-push hook that calls the engine.
- `ci/` : entry scripts and lints that CI runs.
- `.github/workflows/` : the workflows that call those scripts.

## Build

    cd core/rust && cargo build --release

The hook looks for the binary at `core/rust/target/release/harness`.
