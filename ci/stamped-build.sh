#!/bin/sh
# Stamped build. Usage: ci/stamped-build.sh
# Builds core/rust release with HARNESS_BUILD_COMMIT set to the commit of HEAD of the checkout this
# script lives in, so the stamp names the code that was built. The variable is read at build time by
# the engine itself (option_env!), so a rebuild takes a new stamp with no source change.
# Exit 0 built (prints stamped-build: built commit=<sha>); 1 refused (HEAD has no commit, or the work
# tree holds a tracked change or an untracked file: git status --porcelain is not empty) and then no
# build runs; nothing is printed on stdout. The release build, the release smoke and the clean-install
# scripts set no stamp, so their binaries hold none.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
root="$here/.."
commit=$(git -C "$root" rev-parse --verify HEAD 2>/dev/null) || exit 1
[ -n "$(git -C "$root" status --porcelain)" ] && exit 1
cd "$root" || exit 1
HARNESS_BUILD_COMMIT="$commit" cargo build --release --locked --offline --manifest-path core/rust/Cargo.toml || exit 1
echo "stamped-build: built commit=$commit"
