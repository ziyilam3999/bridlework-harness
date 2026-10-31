# harness

A small privacy gate for this repository. It scans what is about to be pushed
and refuses the push when a value of a protected class is found.

## Map

- `core/rust/` : Rust workspace. The `harness` crate holds the scan engine
  (std only, no external crates).
- `.githooks/` : the pre-push hook and the `.githooks/pre-commit` hook; both call the engine.
- `ci/` : entry scripts and lints that CI runs.
- `.github/workflows/` : the workflows that call those scripts.
- `dashboard/` : the dashboard (placeholder).
- `schemas/` : data schemas (placeholder).
- `skills/` : skills (placeholder).
- `hooks/` : hooks (placeholder).
- `gates/` : verification, architecture and security gates, and the path-to-check map (placeholder).
- `wiki/hub/` : the wiki hub (placeholder).
- `wiki/lesson-store/` : the lesson store (placeholder).
- `providers/anthropic/`, `providers/zai/`, `providers/openai/`, `providers/jev/` : model provider adapters (placeholders).
- `plugins/claude/`, `plugins/openai/` : thin host plugin packages (placeholders).
- `mcp/` : MCP servers (placeholder).
- `evals/` : evaluations (placeholder).
- `examples/` : examples (placeholder).
- `config/` : example configuration files.
- `docs/` : documentation (placeholder).

## Build

    cd core/rust && cargo build --release

The hook looks for the binary at `core/rust/target/release/harness`.

Turn the hooks on in a clone: `git config core.hooksPath .githooks`
