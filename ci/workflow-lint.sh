#!/bin/sh
# Workflow lint. Usage: ci/workflow-lint.sh [workflow file ...]   (default: every workflow in .github/workflows)
# Layout assumed: jobs at 2 spaces, job keys at 4, steps ("- ") at 6.
# Exit 0 clean, 1 at least one refusal (file:line:rule printed, never content), 2 usage or unreadable file.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
if [ "$#" -eq 0 ]; then
  set -- "$here"/../.github/workflows/*.yml
fi
exec python3 -I - "$@" <<'PY'
import os, re, sys

ALLOWED_PERMS = {"contents", "issues", "pull-requests"}


def code(line):
    s = line.strip()
    if s.startswith("#"):
        return ""
    return re.sub(r"\s+#.*$", "", line.rstrip())


def indent(line):
    n = len(line) - len(line.lstrip())
    if line.lstrip().startswith("- "):
        n += 2
    return n


def lint(path):
    bad = []
    lines = open(path, encoding="utf-8").read().split("\n")
    cl = [code(x) for x in lines]
    has_perm = False
    for i, c in enumerate(cl):
        if not c.strip():
            continue
        n = i + 1
        if "continue-on-error" in c:
            bad.append((n, "continue-on-error"))
        if re.match(r"^    if:", c):
            bad.append((n, "job-level-if"))
        if re.search(r"github\.event\.pull_request\.head|github\.head_ref", c):
            bad.append((n, "checkout-pr-head"))
        if re.search(r"toJSON\(\s*secrets|secrets:\s*inherit", c):
            bad.append((n, "secret-wide"))
        if re.search(r":\s*write\b|write-all", c):
            bad.append((n, "permission-write"))
        if re.match(r"^permissions:", c):
            has_perm = True
    if not has_perm:
        bad.append((1, "no-permissions"))
    # permission grants: only the allowed keys, read only
    in_perm = False
    pind = 0
    for i, c in enumerate(cl):
        if not c.strip():
            continue
        m = re.match(r"^(\s*)permissions:\s*(\S*)$", c)
        if m:
            if m.group(2):
                bad.append((i + 1, "permission-inline"))
            in_perm = True
            pind = len(m.group(1))
            continue
        if in_perm:
            if indent(c) <= pind:
                in_perm = False
            else:
                g = re.match(r"\s*([\w-]+):\s*(\S+)$", c)
                if not g or g.group(1) not in ALLOWED_PERMS or g.group(2) != "read":
                    bad.append((i + 1, "permission-grant"))
    # steps
    starts = [i for i, c in enumerate(cl) if re.match(r"^      - ", c)]
    covered = set()
    for k, s in enumerate(starts):
        e = starts[k + 1] if k + 1 < len(starts) else len(cl)
        for j in range(s + 1, e):
            if cl[j].strip() and indent(cl[j]) <= 4:
                e = j
                break
        covered.update(range(s, e))
        body = cl[s:e]
        text = "\n".join(body)
        if re.search(r"uses:\s*actions/checkout", text):
            if not re.search(r"persist-credentials:\s*false", text):
                bad.append((s + 1, "checkout-persist-credentials"))
            if re.search(r"^\s*(-\s+)?(ref|repository):", text, re.M):
                bad.append((s + 1, "checkout-ref"))
        if "secrets." in text and not re.search(r"ci/privacy-(gate|text)\.sh", text):
            bad.append((s + 1, "secret-outside-scan-step"))
        # ${{ github.event.* }} inside a run: script
        for j, line in enumerate(body):
            m = re.match(r"^(\s*(?:-\s+)?)run:\s*(.*)$", line)
            if not m:
                continue
            rind = indent(line)
            chunk = [m.group(2)]
            for r in body[j + 1:]:
                if r.strip() and indent(r) <= rind:
                    break
                chunk.append(r)
            if re.search(r"\$\{\{[^}]*github\.event", "\n".join(chunk)):
                bad.append((s + j + 1, "event-text-in-run"))
    for i, c in enumerate(cl):
        if "secrets." in c and i not in covered:
            bad.append((i + 1, "secret-outside-scan-step"))
    return bad


rc = 0
for f in sys.argv[1:]:
    if not os.path.isfile(f):
        sys.stderr.write("workflow-lint: unreadable file\n")
        sys.exit(2)
    for n, rule in sorted(set(lint(f))):
        print("%s:%d:%s" % (os.path.basename(f), n, rule))
        rc = 1
sys.exit(rc)
PY
