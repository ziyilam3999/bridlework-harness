#!/bin/sh
# Stamped build. Usage: ci/stamped-build.sh
# Builds core/rust release with HARNESS_BUILD_COMMIT set to the commit of HEAD of the checkout this
# script lives in, so the stamp names the commit of HEAD (git-ignored files are not checked). The
# variable is read at build time by the engine itself (option_env!), so a rebuild takes a new stamp
# with no source change.
# Exit 0 built (prints stamped-build: built commit=<sha>); 1 refused (HEAD has no commit, or the work
# tree holds a tracked change or an untracked file: git status --porcelain --untracked-files=all is not
# empty, or git status fails, or an index entry is flagged assume-unchanged or skip-worktree, or
# git ls-files fails) and then no build runs, or the cargo build failed; nothing is printed
# on stdout. GIT_DIR, GIT_WORK_TREE, GIT_INDEX_FILE, GIT_COMMON_DIR and GIT_OBJECT_DIRECTORY are
# cleared before the git calls. The release build, the release smoke and the clean-install scripts
# set no stamp, so their binaries hold none unless the variable is set in their environment.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 1
root="$here/.."
# The git calls read this checkout, not a repo named by the caller's environment.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY
commit=$(git -C "$root" rev-parse --verify HEAD 2>/dev/null) || exit 1
# A git status that fails is a refusal; untracked files are listed whatever status.showUntrackedFiles
# says (files ignored by .gitignore, .git/info/exclude or core.excludesFile are not checked).
st=$(git -C "$root" status --porcelain --untracked-files=all) || exit 1
[ -z "$st" ] || exit 1
# An index entry flagged assume-unchanged or skip-worktree hides an edit from git status: ls-files -v
# tags such an entry with a letter other than H. A failing ls-files is a refusal.
tags=$(git -C "$root" ls-files -v) || exit 1
case "$tags" in
  *'
'[!H]' '*|[!H]' '*) exit 1 ;;
esac
cd "$root" || exit 1
HARNESS_BUILD_COMMIT="$commit" cargo build --release --locked --offline --manifest-path core/rust/Cargo.toml || exit 1
echo "stamped-build: built commit=$commit"
