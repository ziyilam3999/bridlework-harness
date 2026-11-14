#!/bin/sh
# CI Rust clippy gate. Usage: ci/rust-clippy.sh
# Lints every target of the workspace with warnings denied, so a new lint fails the build.
# The deny list is never set here: the lint must not need it (see ci/lint-rust-tests.sh).
# Exit 0 clean, non-zero on any lint failure.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
cd "$here/../core/rust" || exit 1
unset HARNESS_PRIVACY_LIST HARNESS_PRIVACY_OLD_LIST HARNESS_PRIVACY_LIST_SECRET
cargo clippy --locked --all-targets -- -D warnings || exit 1
