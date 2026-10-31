#!/usr/bin/env bash
# PreToolUse hook (Bash): scan the words a `gh` call would publish before the call runs (T-4).
# Reads the tool call as JSON on stdin. Inputs by environment: HARNESS_BIN (the engine), HARNESS_PRIVACY_LIST
# (absolute path of the deny list; passed to the engine, never opened here), HARNESS_PUBLIC_REPO (owner/name).
# Exit 0 = allow (stdout and stderr empty). Exit 2 = refuse, `refused:` lines on stderr (surface and class id
# only, never the matched text), nothing on stdout. Any failure of the hook itself is converted to exit 2
# (only exit 2 blocks in Claude Code). Not a firewall for other repos: a write that provably targets another
# repo passes unscanned. If the hook is not sure it says no.
# Program-word rule: a command is waved through unread only when its text has no "gh" and every word is made of
# safe characters. Every other command is parsed, and the program word of every command (also inside $(..),
# backticks and <(..)) must be a plain literal from a small safe set; an allowed prefix (env, command, ...) followed
# by any option is refused. Unparseable text is refused. No pattern matcher is ever asked "could this be gh?".
# Test-only override named here (the smoke names its own in its header): HARNESS_TEST_SWAP_FILE, a path; when it
# equals the body file being read, the hook rewrites that file between its two stats, which can only force a
# refusal. It does nothing when unset.
# The `# G:<name>` tags mark each guard; the mutation proof disables one tag at a time.
set -u

here="${BASH_SOURCE[0]:-$0}"
case "$here" in /*) ;; *) here="$PWD/$here" ;; esac
here_dir=$(CDPATH= cd -- "${here%/*}" 2>/dev/null && pwd -P) || here_dir=""
here_abs="$here_dir/${here##*/}"

refuse() {
  printf '%s\n' "$1" >&2
  exit 2
}

if ! command -v python3 >/dev/null 2>&1; then refuse "refused: python3 is missing"; fi

PROG=''
read -r -d '' PROG <<'PYEOF'
import json, os, re, signal, stat, subprocess, sys, time

START = time.monotonic()
STDIN_CAP = 1024 * 1024
FIELD_CAP = 8
ENGINE_TIMEOUT = 3.0
TOTAL_BUDGET = 12.0
FILE_CAP = 64 * 1024
CLASSES = {"deny-token", "email", "home-path", "credential", "session-link", "coauthor-personal",
           "regulated-id", "baseline-number", "synthetic-test"}
MENTION = re.compile(r"(?<![A-Za-z0-9_.-])gh(?![A-Za-z0-9_.-])", re.I)
NAME_RE = r"[A-Za-z0-9_.-]+"


class Refuse(Exception):
    pass


def refuse(msg):
    raise Refuse(msg)


def has_control(s):
    return any((ord(c) < 32 and c not in "\t\n") or ord(c) == 127 for c in s)


# ------------------------------------------------------------------ tokenizer
class W(object):
    __slots__ = ("text", "raw", "dyn", "quoted")

    def __init__(self):
        self.text = ""
        self.raw = ""         # the source slice of the word, exactly as written (quotes and backslashes included)
        self.dyn = False      # any expansion, glob, brace, leading ~ or =, process substitution
        self.quoted = False   # a quote or backslash inside the word


class Parsed(object):
    def __init__(self):
        self.cmds = []
        self.heredoc = False
        self.paren = False
        self.subs = []        # (kind, text): "cmd" = the inside of $(..), backticks, <(..); "nest" = a group whose commands are not run
        self.pats = set()     # indexes into cmds of case-pattern words (the words before an unmatched ")")


def heredoc_start(s, i):
    # s[i:i+2] == "<<" and s[i+2] is not "<": return (delimiter, strip_tabs, index after the delimiter word), or None
    n = len(s)
    j = i + 2
    strip = False
    if j < n and s[j] == "-":
        strip = True
        j += 1
    while j < n and s[j] in " \t":
        j += 1
    d = []
    while j < n:
        c = s[j]
        if c in "'\"":
            k = s.find(c, j + 1)
            if k < 0:
                return None
            d.append(s[j + 1:k])
            j = k + 1
            continue
        if c == "\\" and j + 1 < n:
            d.append(s[j + 1])
            j += 2
            continue
        if c in " \t\n;&|()<>":
            break
        d.append(c)
        j += 1
    if not d:
        return None
    return "".join(d), strip, j


def heredoc_skip(s, i, delim, strip):
    # i is just after a newline: return the index after the line that closes the here-document (or the end)
    n = len(s)
    while i < n:
        j = s.find("\n", i)
        line = s[i:] if j < 0 else s[i:j]
        nxt = n if j < 0 else j + 1
        if (line.lstrip("\t") if strip else line) == delim:
            return nxt
        i = nxt
    return n


def skip_paren(s, i, st):
    # s[i] == "(": return the index after its matching ")"; a quote-aware scan, unbalanced is unparseable
    depth = 0
    pending = []
    n = len(s)
    while i < n:
        c = s[i]
        if c == "\\":
            i += 2
            continue
        if c == "'":
            j = s.find("'", i + 1)
            if j < 0:
                refuse("refused: gh text: unparseable command (quote)")
            i = j + 1
            continue
        if c == '"':
            i += 1
            while i < n and s[i] != '"':
                i += 2 if s[i] == "\\" else 1
            i += 1
            continue
        if c == "\n" and pending:  # G:heredoc_skip
            i += 1
            for d, sp in pending:
                i = heredoc_skip(s, i, d, sp)
            pending = []
            continue
        if c == "<" and s[i:i + 2] == "<<":
            st.heredoc = True
            if s[i:i + 3] == "<<<":
                i += 3
                continue
            hs = heredoc_start(s, i)
            if hs is not None:
                pending.append((hs[0], hs[1]))
                i = hs[2]
                continue
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    refuse("refused: gh text: unparseable command (paren)")


def skip_backtick(s, i):
    j = i + 1
    n = len(s)
    while j < n:
        if s[j] == "\\":
            j += 2
            continue
        if s[j] == "`":
            return j + 1
        j += 1
    refuse("refused: gh text: unparseable command (backtick)")


def note_paren(st, s, p, k):
    # s[p] == "(" opened a substitution that ends at k (after its ")"). $((..)) is arithmetic: only what is nested in it runs.
    if s[p + 1:p + 2] == "(":
        k2 = skip_paren(s, p + 1, st)
        if k2 == k - 1:
            st.subs.append(("nest", s[p + 2:k2 - 1]))
            return
    st.subs.append(("cmd", s[p + 1:k - 1]))


def tokenize(s):
    st = Parsed()
    cur = []
    chain = []
    w = None
    wstart = 0
    redir = False
    depth = 0
    pending = []
    i = 0
    n = len(s)

    def endword(end):
        nonlocal w, redir
        if w is not None:
            w.raw = s[wstart:end]
            if redir:
                redir = False
            elif w.raw.isdigit() and end < n and s[end] in "<>":
                pass  # a file descriptor prefix such as the 2 of 2>&1
            else:
                cur.append(w)
            w = None

    def endcmd():
        nonlocal cur, redir
        redir = False
        added = bool(cur)
        if cur:
            st.cmds.append(cur)
        cur = []
        return added

    def begin(pos):
        nonlocal w, wstart
        if w is None:
            w = W()
            wstart = pos
        return w

    while i < n:
        c = s[i]
        if c in " \t":
            endword(i)
            i += 1
            continue
        if c == "\n":  # G:chain_split
            endword(i)
            endcmd()
            chain.clear()
            i += 1
            if pending:  # G:heredoc_skip  (a here-document body is data, not commands)
                for d, sp in pending:
                    i = heredoc_skip(s, i, d, sp)
                pending = []
            continue
        if c == "\\" and i + 1 < n and s[i + 1] == "\n":
            i += 2
            continue
        if c == "#" and w is None:
            while i < n and s[i] != "\n":
                i += 1
            continue
        if c in ";|":  # G:chain_split
            endword(i)
            single_pipe = c == "|" and s[i + 1:i + 2] not in ("|", "&")
            added = endcmd()
            if single_pipe and added:
                chain.append(len(st.cmds) - 1)   # a case pattern list is a | b | c)
            else:
                chain.clear()
            i += 1
            while i < n and s[i] in ";|&":
                i += 1
            continue
        if c == "&":
            if i + 1 < n and s[i + 1] == ">":
                endword(i)
                redir = True
                i += 2
                while i < n and s[i] in ">&|":
                    i += 1
                continue
            if True:  # G:chain_split
                endword(i)
                endcmd()
                chain.clear()
                i += 1
                while i < n and s[i] in ";|&":
                    i += 1
                continue
        if c in "<>":
            if i + 1 < n and s[i + 1] == "(":
                x = begin(i)
                x.dyn = True
                k = skip_paren(s, i + 1, st)
                note_paren(st, s, i + 1, k)
                i = k
                continue
            endword(i)
            if s[i:i + 2] == "<<":
                st.heredoc = True
                if s[i:i + 3] != "<<<":
                    hs = heredoc_start(s, i)
                    if hs is not None:
                        pending.append((hs[0], hs[1]))
            redir = True
            i += 1
            while i < n and s[i] in "<>&|-":
                if s[i] == "-" and not (i > 0 and s[i - 1] == "<"):
                    break
                i += 1
            continue
        if c == "(" and w is not None and s[i + 1:i + 2] != ")":  # G:glued_paren
            # a "(" touching a word is part of that word: a zsh glob group (g(h|x), /abs/(g)h) or an array value (a=(b c))
            x = w
            x.dyn = True
            k = skip_paren(s, i, st)
            x.text += s[i:k]
            st.subs.append(("nest", s[i + 1:k - 1]))
            i = k
            continue
        if c in "()":
            endword(i)
            if c == "(":
                depth += 1
                endcmd()
                chain.clear()
            elif depth > 0:
                depth -= 1
                endcmd()
                chain.clear()
            else:  # an unmatched ")" ends a case pattern list: those words are patterns, not program words
                if endcmd():
                    chain.append(len(st.cmds) - 1)
                st.pats.update(chain)
                chain.clear()
            st.paren = True
            i += 1
            continue
        x = begin(i)
        if c == "'":
            j = s.find("'", i + 1)
            if j < 0:
                refuse("refused: gh text: unparseable command (quote)")
            x.text += s[i + 1:j]
            x.quoted = True
            i = j + 1
            continue
        if c == '"':
            x.quoted = True
            i += 1
            while True:
                if i >= n:
                    refuse("refused: gh text: unparseable command (quote)")
                d = s[i]
                if d == '"':
                    i += 1
                    break
                if d == "\\":
                    if i + 1 >= n:
                        refuse("refused: gh text: unparseable command (quote)")
                    e = s[i + 1]
                    if e == "\n":
                        i += 2
                        continue
                    if e in '$\\"`':
                        x.text += e
                    else:
                        x.text += "\\" + e
                    i += 2
                    continue
                if d == "`":
                    x.dyn = True
                    k = skip_backtick(s, i)
                    x.text += s[i:k]
                    st.subs.append(("cmd", s[i + 1:k - 1]))
                    i = k
                    continue
                if d == "$":
                    nx = s[i + 1:i + 2]
                    if nx == "(":
                        x.dyn = True
                        k = skip_paren(s, i + 1, st)
                        x.text += s[i:k]
                        note_paren(st, s, i + 1, k)
                        i = k
                        continue
                    if nx and (nx.isalnum() or nx in "_@*#?!$-{["):
                        x.dyn = True
                    x.text += d
                    i += 1
                    continue
                x.text += d
                i += 1
            continue
        if c == "\\":
            if i + 1 >= n:
                refuse("refused: gh text: unparseable command (backslash)")
            x.text += s[i + 1]
            x.quoted = True
            i += 2
            continue
        if c == "`":
            x.dyn = True
            k = skip_backtick(s, i)
            x.text += s[i:k]
            st.subs.append(("cmd", s[i + 1:k - 1]))
            i = k
            continue
        if c == "$":
            nx = s[i + 1:i + 2]
            if nx == "(":
                x.dyn = True
                k = skip_paren(s, i + 1, st)
                x.text += s[i:k]
                note_paren(st, s, i + 1, k)
                i = k
                continue
            if nx == "'":
                x.dyn = True
                x.quoted = True
                j = i + 2
                while j < n and s[j] != "'":
                    j += 2 if s[j] == "\\" else 1
                if j >= n:
                    refuse("refused: gh text: unparseable command (quote)")
                x.text += s[i:j + 1]
                i = j + 1
                continue
            if nx and (nx.isalnum() or nx in "_@*#?!$-{[\"'"):
                x.dyn = True
            x.text += c
            i += 1
            continue
        if c in "*?[{":
            x.dyn = True
            x.text += c
            i += 1
            continue
        if c == "~" and x.text == "" and not x.quoted:
            x.dyn = True
        if c == "=" and x.text == "" and not x.quoted and s[i + 1:i + 2] == "(":
            x.dyn = True
            k = skip_paren(s, i + 1, st)
            x.text += s[i:k]
            note_paren(st, s, i + 1, k)
            i = k
            continue
        x.text += c
        i += 1
    endword(n)
    endcmd()
    return st


# ------------------------------------------------------------------ flag tables
# kind: bool | val | text:<surface> | file:<surface> | repo | refuse
def flags(**kw):
    return kw


COMMON = {"-h": "bool", "--help": "bool"}


def table(*dicts):
    t = dict(COMMON)
    for d in dicts:
        t.update(d)
    return t


PR_CREATE = table({
    "-a": "val", "--assignee": "val", "-B": "val", "--base": "val",
    "-b": "text:pr_body", "--body": "text:pr_body", "-F": "file:pr_body", "--body-file": "file:pr_body",
    "-d": "bool", "--draft": "bool", "--dry-run": "bool", "-e": "refuse", "--editor": "refuse",
    "-f": "refuse", "--fill": "refuse", "--fill-first": "refuse", "--fill-verbose": "refuse",  # G:body_source
    "-H": "val", "--head": "val", "-l": "val", "--label": "val", "-m": "val", "--milestone": "val",
    "--no-maintainer-edit": "bool", "-p": "val", "--project": "val", "--recover": "refuse",  # G:body_source
    "-r": "val", "--reviewer": "val", "-T": "refuse", "--template": "refuse",  # G:body_source
    "-t": "text:pr_title", "--title": "text:pr_title", "-w": "refuse", "--web": "refuse", "-R": "repo", "--repo": "repo",
})
PR_EDIT = table({
    "--add-assignee": "val", "--add-label": "val", "--add-project": "val", "--add-reviewer": "val",
    "-B": "val", "--base": "val",
    "-b": "text:pr_body", "--body": "text:pr_body", "-F": "file:pr_body", "--body-file": "file:pr_body",
    "-m": "val", "--milestone": "val", "--remove-assignee": "val", "--remove-label": "val",
    "--remove-milestone": "bool", "--remove-project": "val", "--remove-reviewer": "val",
    "-t": "text:pr_title", "--title": "text:pr_title", "-R": "repo", "--repo": "repo",
})
COMMENT = table({
    "-b": "text:comment_body", "--body": "text:comment_body", "-F": "file:comment_body", "--body-file": "file:comment_body",
    "--edit-last": "bool", "--delete-last": "bool", "--create-if-none": "bool", "--yes": "bool",
    "-e": "refuse", "--editor": "refuse", "-w": "refuse", "--web": "refuse", "-R": "repo", "--repo": "repo",
})
PR_REVIEW = table({
    "-a": "bool", "--approve": "bool", "-c": "bool", "--comment": "bool", "-r": "bool", "--request-changes": "bool",
    "-b": "text:review_body", "--body": "text:review_body", "-F": "file:review_body", "--body-file": "file:review_body",
    "-R": "repo", "--repo": "repo",
})
PR_MERGE = table({
    "--admin": "bool",
    "-A": "text:merge_author_email", "--author-email": "text:merge_author_email",  # G:author_email
    "--auto": "bool",
    "-b": "text:merge_body", "--body": "text:merge_body", "-F": "file:merge_body", "--body-file": "file:merge_body",
    "-d": "bool", "--delete-branch": "bool", "--disable-auto": "bool", "--match-head-commit": "val",
    "-m": "bool", "--merge": "bool", "-r": "bool", "--rebase": "bool", "-s": "bool", "--squash": "bool",
    "-t": "text:merge_subject", "--subject": "text:merge_subject", "-R": "repo", "--repo": "repo",
})
PR_CLOSE = table({
    "-c": "text:close_comment", "--comment": "text:close_comment", "-d": "bool", "--delete-branch": "bool",
    "-R": "repo", "--repo": "repo",
})
ISSUE_CREATE = table({
    "-a": "val", "--assignee": "val",
    "-b": "text:issue_body", "--body": "text:issue_body", "-F": "file:issue_body", "--body-file": "file:issue_body",
    "-e": "refuse", "--editor": "refuse", "-l": "val", "--label": "val", "-m": "val", "--milestone": "val",
    "-p": "val", "--project": "val", "--recover": "refuse", "-T": "refuse", "--template": "refuse",  # G:body_source
    "-t": "text:issue_title", "--title": "text:issue_title", "-w": "refuse", "--web": "refuse",
    "-R": "repo", "--repo": "repo",
})
ISSUE_EDIT = table({
    "--add-assignee": "val", "--add-label": "val", "--add-project": "val",
    "-b": "text:issue_body", "--body": "text:issue_body", "-F": "file:issue_body", "--body-file": "file:issue_body",
    "-m": "val", "--milestone": "val", "--remove-assignee": "val", "--remove-label": "val",
    "--remove-milestone": "bool", "--remove-project": "val",
    "-t": "text:issue_title", "--title": "text:issue_title", "-R": "repo", "--repo": "repo",
})
ISSUE_CLOSE = table({
    "-c": "text:close_comment", "--comment": "text:close_comment", "-r": "val", "--reason": "val",
    "-R": "repo", "--repo": "repo",
})
REL_COMMON = {
    "--discussion-category": "val", "-d": "bool", "--draft": "bool", "--latest": "bool",
    "-n": "text:release_notes", "--notes": "text:release_notes",
    "-F": "file:release_notes", "--notes-file": "file:release_notes",
    "-p": "bool", "--prerelease": "bool", "--target": "val",
    "-t": "text:release_title", "--title": "text:release_title", "--verify-tag": "bool",
    "-R": "repo", "--repo": "repo",
}
REL_CREATE = table(REL_COMMON, {
    "--generate-notes": "refuse", "--notes-from-tag": "refuse", "--notes-start-tag": "refuse",  # G:body_source
})
REL_EDIT = table(REL_COMMON, {"--tag": "text:release_tag"})  # G:release_tag

# shape -> (table, max positionals, needs_text). needs_text: refuse when no text source is given (interactive prompt)
SHAPES = {
    ("pr", "create"): (PR_CREATE, 0, "title+body"),
    ("pr", "edit"): (PR_EDIT, 1, None),
    ("pr", "comment"): (COMMENT, 1, "body"),
    ("issue", "comment"): (COMMENT, 1, "body"),
    ("pr", "review"): (PR_REVIEW, 1, "review"),
    ("pr", "merge"): (PR_MERGE, 1, None),
    ("pr", "close"): (PR_CLOSE, 1, None),
    ("issue", "close"): (ISSUE_CLOSE, 1, None),
    ("issue", "create"): (ISSUE_CREATE, 0, "title+body"),
    ("issue", "edit"): (ISSUE_EDIT, 1, None),
    ("release", "create"): (REL_CREATE, 1, "tag"),
    ("release", "edit"): (REL_EDIT, 1, "tag"),
}
RO_GROUPS = {"pr", "issue", "run", "release", "workflow", "repo", "gist", "label", "cache", "ruleset"}
RO_SUBS = {"list", "view", "status", "checks", "diff", "watch", "download", "checkout", "ready"}
RO_TOP = {"browse", "search", "status"}
PREFIXES = {"command", "env", "exec", "time", "nohup", "builtin", "sudo"}
# Words after which the next word is a command again (if/while conditions, then/else/do bodies, group and negation).
KEYWORDS = {"if", "elif", "while", "until", "then", "else", "do", "!", "{", "coproc"}
# A program word is a plain literal only if every character is in this set (no glob, bracket, brace, group, quote,
# dollar, backtick, backslash, tilde, equals, control or non-ASCII character).
SAFE_WORD = re.compile(r"[A-Za-z0-9_./+@:%,-]+")
SAFE_TOKEN = re.compile(r"[A-Za-z0-9_./+@:%,-][A-Za-z0-9_./+@:%,=-]*")
PROGRAM_OK = {"[", "[[", "{", "}", "!"}   # one-character words that no pattern can turn into a two-letter name
MAX_DEPTH = 8
API_BAD_LONG = {"--method", "--field", "--raw-field", "--input"}

# ------------------------------------------------------------------ repo grammar
_N = NAME_RE
_PATTERNS = [
    re.compile(r"^(" + _N + r")/(" + _N + r")$"),
    re.compile(r"^" + _N + r"/(" + _N + r")/(" + _N + r")$"),
    re.compile(r"^https?://(?:www\.)?" + _N + r"/(" + _N + r")/(" + _N + r")/?$"),
    re.compile(r"^ssh://(?:[A-Za-z0-9_.-]+@)?" + _N + r"/(" + _N + r")/(" + _N + r")$"),
    re.compile(r"^[A-Za-z0-9_.-]+@" + _N + r":(" + _N + r")/(" + _N + r")$"),
]


def reduce_repo(v):
    """Reduce a repo spelling to lower-cased owner/name, or None when outside the strict grammar."""
    if has_control(v) or " " in v or "\t" in v:
        return None
    for p in _PATTERNS:
        m = p.match(v)
        if m:
            o, nm = m.group(1), m.group(2)
            if nm.lower().endswith(".git"):  # G:target_normalise
                nm = nm[:-4]
            if o in (".", "..") or nm in ("", ".", ".."):
                return None
            return (o + "/" + nm).lower()  # G:target_normalise
    return None


def public_repo():
    v = os.environ.get("HARNESS_PUBLIC_REPO", "")
    if re.fullmatch(_N + "/" + _N, v) is None:
        return None
    return v.lower()


# ------------------------------------------------------------------ gh call analysis
class Call(object):
    def __init__(self):
        self.fields = []     # (surface, text)
        self.files = []      # (surface, path)
        self.repos = []      # raw -R values
        self.words = []      # every word of the call (decoded text)
        self.positionals = []


def is_assign(w):
    # the name is literal by the pattern on the source slice; the value may carry quotes (FOO="a b" cmd)
    return re.match(r"^[A-Za-z_][A-Za-z0-9_]*(\+)?=", w.raw) is not None


def is_prefix(w):
    if w.quoted or w.dyn:
        return False
    base = w.text.rsplit("/", 1)[-1] if w.text.startswith("/") else w.text
    return base in PREFIXES


def lead_index(ws, skip_keywords):
    """Index of the program word of one command: after assignments, allowed prefixes and (when asked) shell keywords.
    An allowed prefix followed by any option is refused: its option can change which word is the program."""
    i = 0
    n = len(ws)
    while i < n:
        w = ws[i]
        if is_assign(w):
            i += 1
            continue
        if skip_keywords and (not w.quoted) and w.raw in KEYWORDS:  # G:keyword_skip
            i += 1
            continue
        if skip_keywords and (not w.quoted) and w.raw == "function":  # G:keyword_skip
            i += 2  # function NAME
            continue
        if is_prefix(w):  # G:wrapper
            i += 1
            if i < n and (ws[i].raw.startswith("-") or ws[i].text.startswith("-")):  # G:prefix_option
                refuse("refused: gh text: an option after a command prefix")
            continue
        break
    return i if i < n else None


def program_ok(p):
    """A program word is accepted only as a plain literal: every character in the safe set, nothing quoted or expanded.
    Anything else could be spelling the GitHub CLI by glob, bracket, brace, group, quote, expansion or escape."""
    if p.raw in PROGRAM_OK:
        return True
    return (not p.quoted) and (not p.dyn) and SAFE_WORD.fullmatch(p.raw) is not None


def check_program_words(st, depth, nest=False):
    """Default deny: every command, including those inside $(..), backticks and <(..), must run a plain literal program."""
    if depth > MAX_DEPTH:  # G:sub_depth
        refuse("refused: gh text: command substitution nested too deeply")
    if not nest:
        for idx, ws in enumerate(st.cmds):
            if idx in st.pats:  # G:case_pattern
                continue
            i = lead_index(ws, True)
            if i is None:
                continue
            if not program_ok(ws[i]):  # G:progword_literal
                refuse("refused: gh text: a program word that is not a plain literal")
    for kind, text in st.subs:  # G:sub_recurse
        check_program_words(tokenize(text), depth + 1, kind == "nest")


def plain_command(cmd):
    """True when nothing in the text can spell the GitHub CLI: no 'gh' anywhere and every word made of safe characters."""
    if "gh" in cmd.lower():
        return False
    for tok in re.split(r"[ \t\n;&|<>]+", cmd):
        if tok != "" and SAFE_TOKEN.fullmatch(tok) is None:
            return False
    return True


def analyse_api(ws):
    for w in ws:
        t = w.text
        low = t.lower()
        if re.match(r"^/?graphql(\?.*)?$", low):  # G:api_write
            refuse("refused: gh text: gh api graphql")
        if t.startswith("--"):
            nm = t.split("=", 1)[0]
            if nm in API_BAD_LONG or nm.startswith("--method"):  # G:api_write
                refuse("refused: gh text: gh api write flag")
        elif t.startswith("-") and len(t) > 1:
            if any(ch in "XfF" for ch in t[1:2]) or re.match(r"^-[A-Za-z]*[XfF]", t):  # G:api_write
                refuse("refused: gh text: gh api write flag")
    for w in ws:
        if w.dyn:  # G:parse_before_target
            refuse("refused: gh text: expansion in a gh call")


def parse_gh_call(ws):
    """ws: the words after the program word. Returns a Call, or None for a shape that publishes nothing."""
    call = Call()
    call.words = [w.text for w in ws]
    if not ws:
        return None
    first = ws[0]
    if first.text.startswith("-"):
        if first.text in ("--version", "--help", "-h") and len(ws) == 1:
            return None
        refuse("refused: gh text: unknown gh flag")
    group = first.text
    if first.dyn:  # G:parse_before_target
        refuse("refused: gh text: expansion in a gh call")
    if group == "api":
        analyse_api(ws[1:])
        return None
    if group in ("alias", "extension", "config"):
        refuse("refused: gh text: gh " + group + " is not supported")
    sub = ws[1].text if len(ws) > 1 else ""
    if len(ws) > 1 and ws[1].dyn:  # G:parse_before_target
        refuse("refused: gh text: expansion in a gh call")
    if (group == "gist" and sub in ("create", "edit")) or (group == "repo" and sub in ("create", "edit")) \
            or (group == "release" and sub == "upload"):  # G:content_shapes
        refuse("refused: gh text: content shape is not supported")
    if (group in RO_GROUPS and sub in RO_SUBS) or (group == "auth" and sub == "status") or group in RO_TOP:  # G:readonly_allowlist
        for w in ws[1:]:
            if w.dyn:  # G:parse_before_target
                refuse("refused: gh text: expansion in a gh call")
        return None
    shape = SHAPES.get((group, sub))
    if shape is None:
        refuse("refused: gh text: unsupported gh subcommand")
    tbl, maxpos, needs = shape
    args = ws[2:]
    i = 0
    have = set()
    while i < len(args):
        w = args[i]
        t = w.text
        if t == "--":
            if True:  # G:dashdash
                refuse("refused: gh text: a lone -- on a text shape")
            i += 1
            continue
        if t == "-" or not t.startswith("-"):
            if t == "-":
                refuse("refused: gh text: stdin is not readable")
            if w.dyn:  # G:parse_before_target
                refuse("refused: gh text: expansion in a gh call")
            call.positionals.append(w)
            i += 1
            continue
        attached = None
        if t.startswith("--"):
            name = t.split("=", 1)[0]
            if "=" in t:
                attached = t.split("=", 1)[1]
        else:
            name = t[:2]
            if len(t) > 2:
                attached = t[2:]
        kind = tbl.get(name)
        if kind is None:  # G:flag_table
            refuse("refused: gh text: unknown flag")
        if kind == "refuse":  # G:flag_table
            refuse("refused: gh text: flag is not readable")
        if kind == "bool":
            if w.dyn:  # G:parse_before_target
                refuse("refused: gh text: expansion in a gh call")
            if t.startswith("--"):
                if attached is not None and attached not in ("true", "false"):  # G:flag_table
                    refuse("refused: gh text: ambiguous flag value")
            elif attached:
                for ch in attached:
                    if tbl.get("-" + ch) != "bool":  # G:flag_table
                        refuse("refused: gh text: unknown short flag")
            have.add(name)
            i += 1
            continue
        if attached is not None:
            if attached.startswith("=") and not t.startswith("--"):  # G:flag_table
                refuse("refused: gh text: ambiguous flag value")
            vword, vtext = w, attached
            i += 1
        else:
            if i + 1 >= len(args):  # G:flag_table
                refuse("refused: gh text: a flag has no value")
            vword = args[i + 1]
            vtext = vword.text
            i += 2
        base, _, surface = kind.partition(":")
        if base == "text":
            if vword.dyn:  # G:expansion
                refuse("refused: gh text: expansion in a text argument")
            call.fields.append((surface, vtext))
        elif base == "file":
            if vword.dyn:  # G:expansion
                refuse("refused: gh text: expansion in a text argument")
            if vtext == "-":  # G:flag_table
                refuse("refused: gh text: stdin is not readable")
            call.files.append((surface, vtext))
        elif base == "repo":
            if vword.dyn:  # G:parse_before_target
                refuse("refused: gh text: expansion in a gh call")
            call.repos.append(vtext)
        else:
            if vword.dyn:  # G:parse_before_target
                refuse("refused: gh text: expansion in a gh call")
        have.add(name)
    if len(call.positionals) > maxpos:  # G:content_shapes
        refuse("refused: gh text: unexpected positional words")
    if (group, sub) == ("release", "create"):
        if len(call.positionals) < 1:
            refuse("refused: gh text: release tag is missing")
        if True:  # G:release_tag
            call.fields.append(("release_tag", call.positionals[0].text))
    srcs = set(s for s, _ in call.fields) | set(s for s, _ in call.files)
    if needs == "title+body":
        if not any(s.endswith("_title") for s in srcs) or not any(s.endswith("_body") for s in srcs):
            refuse("refused: gh text: interactive prompt")
    elif needs == "body":
        if not any(s.endswith("_body") for s in srcs) and "--delete-last" not in have:
            refuse("refused: gh text: interactive prompt")
    elif needs == "review":
        if not any(s.endswith("_body") for s in srcs) and not (have & {"-a", "--approve"}):
            refuse("refused: gh text: interactive prompt")
    return call


# ------------------------------------------------------------------ body files
def read_body_file(path, surface, chain_has_cd):
    if has_control(path) or path == "-" or path == "":
        refuse("refused: gh text: body file is not readable")
    if not path.startswith("/"):  # G:bodyfile_abs
        refuse("refused: gh text: body file path is not absolute")
    if chain_has_cd:  # G:bodyfile_abs
        refuse("refused: gh text: body file with a folder change in the chain")
    if surface == "merge_body":  # G:bodyfile_surface
        refuse("refused: gh text: body file on a surface with no CI backstop")
    try:
        st1 = os.lstat(path)
    except OSError:
        refuse("refused: gh text: body file is missing")
    if not stat.S_ISREG(st1.st_mode):  # G:bodyfile_kind
        refuse("refused: gh text: body file is not a plain file")
    if st1.st_mode & 0o022:  # G:bodyfile_kind
        refuse("refused: gh text: body file is group or world writable")
    if st1.st_size > FILE_CAP:  # G:bodyfile_kind
        refuse("refused: gh text: body file is too large")
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except OSError:
        refuse("refused: gh text: body file is not readable")
    try:
        fst = os.fstat(fd)
        if (fst.st_dev, fst.st_ino) != (st1.st_dev, st1.st_ino):  # G:bodyfile_stat
            refuse("refused: gh text: body file changed")
        data = b""
        while True:
            chunk = os.read(fd, 65536)
            if not chunk:
                break
            data += chunk
            if len(data) > FILE_CAP:  # G:bodyfile_kind
                refuse("refused: gh text: body file is too large")
    except OSError:
        refuse("refused: gh text: body file is not readable")
    finally:
        os.close(fd)
    swap = os.environ.get("HARNESS_TEST_SWAP_FILE", "")
    if swap != "" and swap == path:
        try:
            with open(path, "ab") as f:
                f.write(b"x")
        except OSError:
            pass
    try:
        st2 = os.lstat(path)
    except OSError:
        refuse("refused: gh text: body file changed")
    same = (st1.st_dev, st1.st_ino, st1.st_size, st1.st_mtime_ns) == (st2.st_dev, st2.st_ino, st2.st_size, st2.st_mtime_ns)
    if not same:  # G:bodyfile_stat
        refuse("refused: gh text: body file changed")
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        refuse("refused: gh text: body file is not valid UTF-8")


# ------------------------------------------------------------------ engine
def anchored(path):
    real = os.path.realpath(path)
    if any(c.lower() == ".git" for c in real.split("/")):  # G:anchor_dotgit
        return False
    p = real
    while p != "/":
        p = os.path.dirname(p)
        if os.path.lexists(os.path.join(p, ".git")):
            return False
    return True


def run_engine(bin_path, surface, text, timeout):
    env = {"PATH": "/usr/bin:/bin", "HOME": os.environ.get("HOME", "/"), "HARNESS_PRIVACY_LIST": os.environ["HARNESS_PRIVACY_LIST"]}
    env = scrubbed(env)
    argv = [bin_path, "privacy", "scan", "--text", "--surface", surface]
    try:
        p = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             env=env, start_new_session=True)
    except OSError:  # G:engine_missing
        refuse("refused: gh text: the scan engine could not run")
    try:
        out, _err = p.communicate(text.encode("utf-8", "strict"), timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except OSError:
            pass
        try:
            p.communicate(timeout=1)
        except Exception:
            pass
        refuse("refused: gh text: the scan engine timed out")
    except (OSError, UnicodeError, ValueError):
        refuse("refused: gh text: the scan engine failed")
    return p.returncode, out


def scrubbed(env):
    return env  # G:env_scrub


def scan_fields(fields):
    if len(fields) > FIELD_CAP:  # G:field_cap
        refuse("refused: gh text: too many text fields")
    v = os.environ.get("HARNESS_PRIVACY_LIST", "")
    if v == "" or has_control(v) or not v.startswith("/"):  # G:list_env
        refuse("refused: HARNESS_PRIVACY_LIST must be set to an absolute path")
    if not (os.path.isfile(v) and os.access(v, os.R_OK)):  # G:list_env
        refuse("refused: HARNESS_PRIVACY_LIST is not a readable file")
    bin_path = os.environ.get("HARNESS_BIN", "")
    if bin_path == "" or has_control(bin_path) or not bin_path.startswith("/"):  # G:engine_missing
        refuse("refused: HARNESS_BIN must be set to an absolute path")
    if not (os.path.isfile(bin_path) and os.access(bin_path, os.X_OK)):  # G:engine_missing
        refuse("refused: gh text: the scan engine is missing")
    if not anchored(bin_path):  # G:anchor_engine
        refuse("refused: gh text: the scan engine is inside a git work tree")
    if not anchored(v):  # G:anchor_list
        refuse("refused: gh text: the deny list is inside a git work tree")
    if not anchored(sys.argv[1]):  # G:anchor_hook
        refuse("refused: gh text: the hook script is inside a git work tree")
    deadline = START + TOTAL_BUDGET
    lines = []
    for surface, text in fields:
        rem = deadline - time.monotonic()
        if rem <= 0:  # G:deadline
            refuse("refused: gh text: the scan deadline passed")
        t = min(ENGINE_TIMEOUT, rem)  # G:deadline
        rc, out = run_engine(bin_path, surface, text, t)
        if rc not in (0, 1):  # G:engine_exit
            refuse("refused: gh text: the scan engine failed")
        if rc == 1:  # G:engine_verdict
            raw = out.decode("utf-8", "replace").split("\n")
            if raw and raw[-1] == "":
                raw.pop()
            if not raw:
                refuse("refused: gh text: the scan engine output is malformed")
            kept = []
            for ln in raw:
                m = re.match(r"^([a-z_.]+):([a-z0-9-]+)$", ln)
                if m is None or m.group(1) != surface or m.group(2) not in CLASSES:
                    refuse("refused: gh text: the scan engine output is malformed")  # G:no_echo
                if m.group(2) == "session-link":
                    if True:  # G:session_link
                        kept.append(surface + ":" + m.group(2))
                else:
                    kept.append(surface + ":" + m.group(2))
            lines.extend(kept)
        if rc == 0 and out:
            refuse("refused: gh text: the scan engine output is unexpected")
    if lines:
        seen = []
        for ln in lines:
            if ln not in seen:
                seen.append(ln)
        raise Refuse("\n".join("refused: gh text: " + x for x in seen[:8]))


# ------------------------------------------------------------------ main
def main():
    data = sys.stdin.buffer.read(STDIN_CAP + 1)
    if len(data) > STDIN_CAP:  # G:stdin_cap
        refuse("refused: the tool call is too large")
    try:
        call = json.loads(data.decode("utf-8"))
    except (ValueError, UnicodeDecodeError):  # G:stdin_json
        refuse("refused: the tool call is not valid JSON")
    if not isinstance(call, dict):
        refuse("refused: the tool call is not an object")
    if call.get("tool_name") != "Bash":
        refuse("refused: the tool is not Bash")
    tin = call.get("tool_input")
    cmd = tin.get("command") if isinstance(tin, dict) else None
    if not isinstance(cmd, str) or cmd == "":  # G:command_missing
        refuse("refused: the tool input has no command")

    # Is the GitHub CLI possibly in play? A bare mention of it, or any command this hook cannot read as plain words.
    # Nothing is let through on a "no" from pattern matching: only a text with no "gh" in it and nothing but safe
    # characters in every word is waved through unread; every other command is parsed and every program word must be
    # a plain literal (unparseable means cannot decide, and cannot decide means refuse).
    mention = MENTION.search(cmd) is not None or MENTION.search(re.sub(r"['\"\\]", "", cmd)) is not None
    if not mention and plain_command(cmd):  # G:plain_fast
        return
    if mention and has_control(cmd):
        refuse("refused: gh text: control characters in the command")

    try:
        st = tokenize(cmd)
    except Refuse:
        if True:  # G:cannot_decide
            raise
        return
    check_program_words(st, 0)
    if not mention:
        return
    if st.heredoc:  # G:heredoc
        refuse("refused: gh text: heredoc in a gh command")
    if st.paren:  # G:wrapper
        refuse("refused: gh text: grouping in a gh command")

    calls = []
    analysed = 0
    stray = False
    chain_has_cd = False
    for ws in st.cmds:
        i = lead_index(ws, False)
        if i is None:
            continue
        p = ws[i]
        if p.text in ("cd", "pushd", "popd"):
            chain_has_cd = True
        for w in ws:
            if w.raw.startswith("=") and not w.quoted:  # G:zsh_equals
                refuse("refused: gh text: zsh equals expansion")
        is_gh = p.text == "gh" or (p.text.endswith("/gh") and p.text.startswith("/") and re.fullmatch(r"[A-Za-z0-9_./+@-]+", p.text) is not None)
        if is_gh:
            analysed += 1
            c = parse_gh_call(ws[i + 1:])
            if c is not None:
                c.prog = p
                calls.append(c)
            # the words of this call are mention-tested below like every other word
            for w in ws[i + 1:]:
                if MENTION.search(w.text):  # G:mention_contain
                    refuse("refused: gh text: a gh mention that is not a program word")
            continue
        for w in ws:
            if MENTION.search(w.text):  # G:mention_contain
                refuse("refused: gh text: a gh mention that is not a program word")
    if len(MENTION.findall(cmd)) > analysed:  # G:mention_contain
        refuse("refused: gh text: a gh mention that is not a program word")

    # Which calls need a scan? Target rule T decides, after every refusal above.
    scan = []
    pub = public_repo()
    gh_env = re.search(r"(?<![A-Za-z0-9_])GH_[A-Za-z0-9_]*", cmd, re.I) is not None
    for c in calls:
        has_text = bool(c.fields or c.files)
        if not has_text:
            continue
        if pub is None:  # G:public_repo_env
            refuse("refused: HARNESS_PUBLIC_REPO must be set to owner/name")
        if len(c.repos) > 1:  # G:repo_flag_once
            refuse("refused: gh text: more than one repo flag")
        unscanned = False
        if c.repos:
            red = reduce_repo(c.repos[0])
            if red is None:  # G:target_grammar
                refuse("refused: gh text: the repo value is outside the strict grammar")
            if red != pub:
                unscanned = True
        if unscanned:
            for wtxt in c.words:
                if pub in wtxt.lower():  # G:target_repo
                    unscanned = False
                    break
            for pw in c.positionals:
                if "/" in pw.text or "@" in pw.text:
                    r2 = reduce_repo(pw.text)
                    if r2 is None or r2 == pub:  # G:target_repo
                        unscanned = False
        if unscanned and gh_env:  # G:target_env
            unscanned = False
        if not unscanned:
            scan.append(c)
    if not scan:
        return
    fields = []
    for c in scan:
        fields.extend(c.fields)
    for c in scan:
        for surface, path in c.files:
            txt = read_body_file(path, surface, chain_has_cd)
            fields.append((surface, txt))  # G:bodyfile_scan
    scan_fields(fields)


try:
    main()
except Refuse as e:
    sys.stderr.write(str(e) + "\n")
    sys.exit(2)
sys.exit(0)
PYEOF

# G:catch_all  (any python failure other than a clean refusal becomes exit 2)
err=$(python3 -I -c "$PROG" "$here_abs" 2>&1 >/dev/null)
rc=$?
if [ "$rc" -eq 0 ]; then
  exit 0
fi
if [ "$rc" -eq 2 ] && [ -n "$err" ]; then
  ok=1
  n=0
  out=""
  while IFS= read -r l; do
    case "$l" in refused:*) ;; *) ok=0 ;; esac
    n=$((n + 1))
    if [ "$n" -le 8 ]; then out="$out${l:0:300}"$'\n'; fi
  done <<< "$err"
  if [ "$ok" -eq 1 ]; then printf '%s' "$out" >&2; exit 2; fi
fi
refuse "refused: internal error in the gh text hook"  # G:catch_all
