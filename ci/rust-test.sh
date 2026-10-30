#!/bin/sh
# CI Rust build and test entry. Usage: ci/rust-test.sh
# Builds the workspace, runs its tests, then runs the built engine's --version.
# The deny list is never set here: the tests must not need it (see ci/lint-rust-tests.sh).
# Exit 0 clean, non-zero on any build, test or version failure.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
cd "$here/../core/rust" || exit 1
unset HARNESS_PRIVACY_LIST HARNESS_PRIVACY_OLD_LIST HARNESS_PRIVACY_LIST_SECRET
cargo build --locked || exit 1
cargo test --locked || exit 1
./target/debug/harness --version </dev/null || exit 1
