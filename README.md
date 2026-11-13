# harness

bridlework-harness is a Rust engine (`harness`) plus a Claude Code plugin that runs fail-closed gates around a ticket: hooks
that restrict paths, check text before it reaches GitHub, and refuse to mark a ticket done until its gate has passed. It also
keeps the original privacy gate, which scans what is about to be pushed and refuses the push when a value of a protected
class is found.

## Install

Releases ship an engine tarball, a plugin tarball and `SHA256SUMS` (Linux x86_64). Download, verify, unpack, then run the
example ticket to check the install: see [docs/install.md](docs/install.md).

## Upgrade

Install the new version next to the old one and switch; rollback is the reverse move. See
[docs/install.md#upgrade](docs/install.md#upgrade).

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
- `plugins/claude/` : the Claude Code plugin (hooks that call the engine).
- `plugins/openai/` : the OpenAI host plugin (placeholder).
- `mcp/` : MCP servers (placeholder).
- `evals/` : evaluations (placeholder).
- `examples/` : the example project and ticket used by the install guide.
- `config/` : example configuration files.
- `docs/` : the install guide and the release notes for maintainers.

## Build from source

    cd core/rust && cargo build --release

The hook looks for the binary at `core/rust/target/release/harness`.

Turn the hooks on in a clone: `git config core.hooksPath .githooks`

## Cutting a release

A version tag starts the release job; see [docs/release.md](docs/release.md).
