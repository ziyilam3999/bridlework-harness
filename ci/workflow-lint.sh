#!/bin/sh
# Workflow lint. Usage: ci/workflow-lint.sh [workflow file ...]   (default: every workflow in .github/workflows)
# Layout assumed: jobs at 2 spaces, job keys at 4, steps ("- ") at 6.
# Exit 0 clean, 1 at least one refusal (file:line:rule printed, never content), 2 usage or unreadable file.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
if [ "$#" -eq 0 ]; then
  # GitHub runs both spellings, so the default list covers both (a missing pattern is skipped, not an error).
  for f in "$here"/../.github/workflows/*.yml "$here"/../.github/workflows/*.yaml; do
    [ -f "$f" ] && set -- "$@" "$f"
  done
  if [ "$#" -eq 0 ]; then
    echo "workflow-lint: no workflow files" >&2
    exit 2
  fi
fi
exec python3 -I - "$@" <<'PY'
import os, re, sys

ALLOWED_PERMS = {"contents", "issues", "pull-requests"}
FILTERS = {"paths", "paths-ignore", "branches", "branches-ignore", "types"}
# A YAML mapping key, with optional quotes and optional space before the colon: `if: x`, `"if": x`, `'if' : x`.
KEY_RE = re.compile(r"""^(\s*)(-\s+)?(?:"([^"]*)"|'([^']*)'|([A-Za-z0-9_.-]+))\s*:(?:\s+(.*))?$""")
PR_FLOW = re.compile(r"pull_request[\"']?\s*:\s*\{[^}]*\b(paths|paths-ignore|branches|branches-ignore|types)\b")


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


def kv(c):
    """(indent, key, quoted, value, dash) of a mapping-key line, else None."""
    m = KEY_RE.match(c)
    if not m:
        return None
    quoted = m.group(3) is not None or m.group(4) is not None
    key = m.group(3) if m.group(3) is not None else (m.group(4) if m.group(4) is not None else m.group(5))
    dash = m.group(2) is not None
    return len(m.group(1)) + (2 if dash else 0), key, quoted, (m.group(6) or "").strip(), dash


def section(cl, key):
    """(start, end) line span of the top-level key, else None."""
    start = None
    for i, c in enumerate(cl):
        if not c.strip():
            continue
        top = not c[0].isspace()
        if start is None:
            k = kv(c) if top else None
            if k and k[1] == key:
                start = i
        elif top:
            return start, i
    return (start, len(cl)) if start is not None else None


def lint_14c(path, cl):
    """Rules that stop a gate being switched off. All file-local."""
    bad = []
    name = os.path.basename(path)
    fg = name == "final-gate.yml"
    # trigger filters under pull_request (not pull_request_target / pull_request_review)
    sec = section(cl, "on")
    if sec:
        in_pr = False
        pr_ind = 0
        for i in range(sec[0], sec[1]):
            c = cl[i]
            if not c.strip():
                continue
            if PR_FLOW.search(c):
                bad.append((i + 1, "pull-request-filter"))
            k = kv(c)
            if in_pr:
                if indent(c) <= pr_ind:
                    in_pr = False
                elif k and k[1] in FILTERS:
                    bad.append((i + 1, "pull-request-filter"))
            if not in_pr and k and k[1] == "pull_request":
                in_pr = True
                pr_ind = k[0]
    for i, c in enumerate(cl):
        if not c.strip():
            continue
        n = i + 1
        k = kv(c)
        # fail-swallow, every workflow
        if re.search(r"\|\|\s*(true|exit\s+0)\s*[\"']?\s*$", c):
            bad.append((n, "fail-swallow"))
        # caches
        if re.search(r"uses[\"']?\s*:\s*[\"']?[^\s\"']*cache", c, re.I):
            bad.append((n, "cache-action"))
        if re.search(r"(^|[\s{,\"'])cache(-[\w-]+)?[\"']?\s*:", c):
            bad.append((n, "cache-input"))
        if not fg:
            continue
        # final-gate.yml only: the exit code cannot be swallowed or the gate step skipped by any route
        if k and k[1] == "if":
            bad.append((n, "if-in-final-gate"))
        if "||" in c:
            bad.append((n, "final-gate-or"))
        if re.search(r"set\s+\+\w*e|set\s+\+o\s+errexit", c):
            bad.append((n, "final-gate-set-plus-e"))
        if k and k[1] == "shell":
            bad.append((n, "final-gate-shell"))
        if k and k[1] == "run" and not re.fullmatch(r"sh ci/[a-z0-9_-]+\.sh", k[3]):
            bad.append((n, "final-gate-run-allowlist"))
        if k and k[2]:
            bad.append((n, "final-gate-quoted-key"))
        if k and re.fullmatch(r"[|>][-+0-9]*", k[3]):
            bad.append((n, "final-gate-block-scalar"))
        if re.search(r"\b(BASH_ENV|ENV)\b", c):
            bad.append((n, "final-gate-shell-env"))
        if "{" in re.sub(r"\$\{\{.*?\}\}", "", c):
            bad.append((n, "final-gate-flow-style"))
    # job reported names (the name: when set, else the job id) that collide with a required check
    sec = section(cl, "jobs")
    if sec:
        jind = None
        bind = None
        jobs = []
        for i in range(sec[0] + 1, sec[1]):
            c = cl[i]
            if not c.strip():
                continue
            k = kv(c)
            if jind is None:
                jind = indent(c)
            if k and not k[4] and k[0] == jind:
                jobs.append([i + 1, k[1], None, None])
                bind = None
                continue
            if not jobs:
                continue
            if bind is None:
                bind = indent(c)
            if k and not k[4] and k[0] == bind and k[1] == "name":
                jobs[-1][2] = k[3].strip("\"'")
                jobs[-1][3] = i + 1
        for line, jid, nm, nline in jobs:
            rep = nm if nm else jid
            at = nline if nm else line
            if rep == "gate" and name != "privacy-gate.yml":
                bad.append((at, "required-name-gate"))
            if rep == "final-gate" and not fg:
                bad.append((at, "required-name-final-gate"))
            if fg and rep != "final-gate":
                bad.append((at, "final-gate-job-name"))
    return bad


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
        k = kv(c)
        if k and k[1] == "if" and k[0] == 4:
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
    bad.extend(lint_14c(path, cl))
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
