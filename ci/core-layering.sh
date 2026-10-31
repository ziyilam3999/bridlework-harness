#!/bin/sh
# Core-layering gate check (G4a). Usage: ci/core-layering.sh
# Runs the planted-fixture smoke, whose live arm reads the real dependency graph from cargo and fails
# naming the first core -> providers/plugins/mcp edge. An unreadable graph also fails (fail closed).
# Exit 0 clean, non-zero on a violation, an unreadable graph or any build failure.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
cd "$here/../core/rust" || exit 1
exec cargo test --locked -p harness --test core_layering
