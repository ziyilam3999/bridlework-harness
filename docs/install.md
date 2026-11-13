# Install and upgrade

This guide installs a released bridlework-harness on **Linux x86_64** from the release assets, then runs the example ticket
to prove the install works. Every step is one shell block. The line under each block starts with `Expected:` and says what
you should see. `ci/install-guide-smoke.sh` runs these same blocks against a locally built fake release, so the guide cannot
drift from the code.

Conventions in the `Expected:` lines: `exit N` is the exit code of the block (0 when absent), a `code` token must appear in
the output, a digits-only token must be a whole output line, and `×N` after a token means exactly N lines contain it.

A release has exactly three assets: the plugin tarball, the engine tarball, and `SHA256SUMS`. The example config and the
example project are not in those assets; the guide takes them from the tag's source archive.

## Install

Check that the tools the guide uses are present.

<!-- step: prereqs -->
```sh
command -v curl sha256sum tar git bash python3
```
Expected: exit 0; six lines, one per tool, each containing `curl`, `sha256sum`, `tar`, `git`, `bash`, `python3` (one line each).

Set the variables. Change `VERSION` to the release you want and `REPO` to the GitHub owner and name of the repository.
`TRIPLE` is the only engine build the project publishes.

<!-- step: vars -->
```sh
VERSION=0.1.0
REPO='<owner>/bridlework-harness'
TRIPLE=x86_64-unknown-linux-gnu
BASE=https://github.com/$REPO/releases/download/v$VERSION
SRC_URL=https://github.com/$REPO/archive/refs/tags/v$VERSION.tar.gz
PREFIX=$HOME/bridlework
echo "$VERSION $TRIPLE"
```
Expected: `0.1.0 x86_64-unknown-linux-gnu`

Download the three release assets, plus the tag's source archive, into a new empty folder.

<!-- step: download -->
```sh
DL="$HOME/downloads/$VERSION"
mkdir -p "$DL" && cd "$DL" &&
curl -fLO "$BASE/bridlework-harness-claude-plugin-$VERSION.tar.gz" &&
curl -fLO "$BASE/bridlework-harness-engine-$VERSION-$TRIPLE.tar.gz" &&
curl -fLO "$BASE/SHA256SUMS" &&
curl -fL "$SRC_URL" -o src.tar.gz
ls | wc -l | tr -d ' '
```
Expected: `4`

Verify the downloads. `SHA256SUMS` also lists `release-notes.md`, which is the release's description and not a download,
so use `--ignore-missing`. If any line says `FAILED` the command exits non-zero: stop, delete the folder and download again.
`src.tar.gz` is **not** covered by `SHA256SUMS`: it is GitHub's own archive of the tag.

<!-- step: verify -->
```sh
cd "$DL" && sha256sum -c --ignore-missing SHA256SUMS
```
Expected: exit 0; `: OK` ×2

Install the engine to `$PREFIX/bin/harness`, the plugin to `$PREFIX/plugins/claude`, and the source archive to `$PREFIX/src`.

<!-- step: install -->
```sh
cd "$DL" &&
mkdir -p "$PREFIX/bin" "$PREFIX/plugins" "$PREFIX/src" &&
tar -xzf "bridlework-harness-engine-$VERSION-$TRIPLE.tar.gz" --strip-components=1 -C "$PREFIX/bin" "bridlework-harness-engine-$VERSION-$TRIPLE/harness" &&
tar -xzf "bridlework-harness-claude-plugin-$VERSION.tar.gz" --strip-components=2 -C "$PREFIX/plugins" &&
tar -xzf src.tar.gz --strip-components=1 -C "$PREFIX/src" &&
"$PREFIX/bin/harness" --version &&
python3 -c 'import json,sys; print("plugin " + json.load(open(sys.argv[1]))["version"])' "$PREFIX/plugins/claude/.claude-plugin/plugin.json"
```
Expected: exit 0; `harness 0.1.0` and `plugin 0.1.0`

Check that no private settings are present. No `HARNESS_` variable may be set, and the example config must report no repos.

<!-- step: overlay -->
```sh
env | grep '^HARNESS_' || echo none
"$PREFIX/bin/harness" config check --config "$PREFIX/src/config/harness.example.yaml"
```
Expected: exit 0; the first output line is `none`, and the output contains `repos=0`

## Use the plugin

The hooks read five variables. All five must be absolute paths, and the ticket file, result folder, caps folder and engine must
sit outside the repository the hooks guard. `HARNESS_REPO_MAP` is one `id=folder` entry.

<!-- step: plugin -->
```sh
W="$HOME/work"
export HARNESS_BIN="$PREFIX/bin/harness"
export HARNESS_TICKET_FILE="$W/tickets/ex-1.json"
export HARNESS_REPO_MAP="example=$W/project"
export HARNESS_RESULT_DIR="$W/res"
export HARNESS_CAPS_DIR="$W/caps"
# claude --plugin-dir "$PREFIX/plugins/claude"
test -f "$PREFIX/plugins/claude/hooks/hooks.json" && env | grep -c '^HARNESS_'
```
Expected: exit 0; `5`

To load the plugin in Claude Code, start it with `claude --plugin-dir "$PREFIX/plugins/claude"` (the commented line above;
there is no marketplace manifest yet). That line is **not run by the guide test**, because Claude Code is interactive. The
next steps run the hook the way `hooks.json` registers it, so you see what Claude Code would see.

## First run on the example ticket

Copy the example project and ticket, and make the project a git repository with one commit.

<!-- step: setup -->
```sh
R="$W/project"
T="$W/ws/ex-1/tree"
mkdir -p "$W" && cp -R "$PREFIX/src/examples/project-template/." "$W/" &&
mkdir -p "$W/ws" "$W/man" "$W/res" "$W/caps" &&
git -C "$R" init -q -b main &&
git -C "$R" add -A &&
git -C "$R" config user.name guide &&
git -C "$R" config user.email guide &&
git -C "$R" commit -q -m base &&
BASE_REV=$(git -C "$R" rev-parse HEAD)
git -C "$R" log --oneline | wc -l | tr -d ' '
```
Expected: `1`

Take the ticket in.

<!-- step: intake -->
```sh
"$PREFIX/bin/harness" ticket intake "$HARNESS_TICKET_FILE" --repo-map "$HARNESS_REPO_MAP"
```
Expected: exit 0

Create the ticket's workspace, a separate working tree on branch `ws/ex-1`.

<!-- step: workspace-create -->
```sh
"$PREFIX/bin/harness" workspace create "$HARNESS_TICKET_FILE" --repo-map "$HARNESS_REPO_MAP" --base "$BASE_REV" --workspace-root "$W/ws" --manifest-dir "$W/man"
```
Expected: exit 0; `branch=ws/ex-1`

Ask the completion hook before any gate has run. This is the command `hooks.json` registers for `TaskUpdate`, with the plugin
root filled in. While you work in the workspace tree, the hook is pointed at that tree with a one-shot repo map.

<!-- step: hook-before -->
```sh
printf '%s' '{"tool_name":"TaskUpdate","tool_input":{"status":"completed"}}' |
  CLAUDE_PLUGIN_ROOT="$PREFIX/plugins/claude" HARNESS_REPO_MAP="example=$T" bash "$PREFIX/plugins/claude/pre-completion-hook.sh"
```
Expected: exit 2; `the gate has not run`

Make a wrong change, commit it in the workspace tree, fast-forward the project to it, and run the gate. The gate fails.

<!-- step: gate-wrong -->
```sh
printf 'hullo\n' > "$T/src/greeting.txt" &&
git -C "$T" commit -q -am "wrong greeting" &&
git -C "$R" merge -q --ff-only ws/ex-1
"$PREFIX/bin/harness" ticket gate "$HARNESS_TICKET_FILE" --repo-map "$HARNESS_REPO_MAP" --base "$BASE_REV" --result-dir "$HARNESS_RESULT_DIR" --caps-dir "$HARNESS_CAPS_DIR"
```
Expected: exit 1; `result=fail`

The hook now refuses because the last gate result is a failure.

<!-- step: hook-wrong -->
```sh
printf '%s' '{"tool_name":"TaskUpdate","tool_input":{"status":"completed"}}' |
  CLAUDE_PLUGIN_ROOT="$PREFIX/plugins/claude" bash "$PREFIX/plugins/claude/pre-completion-hook.sh"
```
Expected: exit 2; `last gate result is fail`

Make the right change and run the gate again. It passes.

<!-- step: gate-right -->
```sh
printf 'hello\n' > "$T/src/greeting.txt" &&
git -C "$T" commit -q -am "right greeting" &&
git -C "$R" merge -q --ff-only ws/ex-1
"$PREFIX/bin/harness" ticket gate "$HARNESS_TICKET_FILE" --repo-map "$HARNESS_REPO_MAP" --base "$BASE_REV" --result-dir "$HARNESS_RESULT_DIR" --caps-dir "$HARNESS_CAPS_DIR"
```
Expected: exit 0; `done=allowed`

<!-- step: hook-right -->
```sh
printf '%s' '{"tool_name":"TaskUpdate","tool_input":{"status":"completed"}}' |
  CLAUDE_PLUGIN_ROOT="$PREFIX/plugins/claude" bash "$PREFIX/plugins/claude/pre-completion-hook.sh"
```
Expected: exit 0

<!-- step: status -->
```sh
"$PREFIX/bin/harness" status "$HARNESS_TICKET_FILE" --repo-map "$HARNESS_REPO_MAP" --result-dir "$HARNESS_RESULT_DIR" --caps-dir "$HARNESS_CAPS_DIR"
```
Expected: exit 0; `last_gate=pass` and `gate_current=yes`

## Upgrade

Install the new version next to the old one, then switch. Your work folder is untouched. Set `VERSION` to the newer tag.

<!-- step: upgrade-download -->
```sh
VERSION=0.1.1
BASE=https://github.com/$REPO/releases/download/v$VERSION
SRC_URL=https://github.com/$REPO/archive/refs/tags/v$VERSION.tar.gz
DL="$HOME/downloads/$VERSION"
mkdir -p "$DL" && cd "$DL" &&
curl -fLO "$BASE/bridlework-harness-claude-plugin-$VERSION.tar.gz" &&
curl -fLO "$BASE/bridlework-harness-engine-$VERSION-$TRIPLE.tar.gz" &&
curl -fLO "$BASE/SHA256SUMS" &&
curl -fL "$SRC_URL" -o src.tar.gz
ls | wc -l | tr -d ' '
```
Expected: `4`

<!-- step: upgrade-verify -->
```sh
cd "$DL" && sha256sum -c --ignore-missing SHA256SUMS
```
Expected: exit 0; `: OK` ×2

<!-- step: upgrade-install -->
```sh
NEW="$PREFIX.new"
cd "$DL" &&
mkdir -p "$NEW/bin" "$NEW/plugins" "$NEW/src" &&
tar -xzf "bridlework-harness-engine-$VERSION-$TRIPLE.tar.gz" --strip-components=1 -C "$NEW/bin" "bridlework-harness-engine-$VERSION-$TRIPLE/harness" &&
tar -xzf "bridlework-harness-claude-plugin-$VERSION.tar.gz" --strip-components=2 -C "$NEW/plugins" &&
tar -xzf src.tar.gz --strip-components=1 -C "$NEW/src" &&
"$NEW/bin/harness" --version
```
Expected: exit 0; `harness 0.1.1`

Switch. `HARNESS_BIN` still points at `$PREFIX/bin/harness`, which is now the new engine. To roll back, run the two `mv`
commands in reverse (`mv "$PREFIX" "$PREFIX.new" && mv "$PREFIX.old" "$PREFIX"`).

<!-- step: upgrade-switch -->
```sh
mv "$PREFIX" "$PREFIX.old" && mv "$PREFIX.new" "$PREFIX" &&
"$HARNESS_BIN" --version
```
Expected: exit 0; `harness 0.1.1`

Check the existing work folder with the new engine.

<!-- step: upgrade-status -->
```sh
"$PREFIX/bin/harness" status "$HARNESS_TICKET_FILE" --repo-map "$HARNESS_REPO_MAP" --result-dir "$HARNESS_RESULT_DIR" --caps-dir "$HARNESS_CAPS_DIR"
```
Expected: exit 0; `last_gate=pass`

## Other platforms

Only `x86_64-unknown-linux-gnu` is published. On any other system, build from source (`cargo build --release --locked` in
`core/rust`, as in `docs/release.md`) and point `HARNESS_BIN` at the result. That path is **not covered by the guide test**.
