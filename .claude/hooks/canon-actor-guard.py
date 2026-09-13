#!/usr/bin/env python3
"""PreToolUse (Bash, Edit, Write) hook: an agent never adjudicates a canon, and
never writes to one as a person.

WHY. canon records an act under `CANON_ACTOR`, else git's `user.name` prefixed
`human:` (~/dev/canon crates/canon-cli/src/store.rs `actor`). Every agent
session on this machine shares the operator's git identity, so `canon approve`
from an agent is, in the ledger, the operator approving (canon
docs/LOAD_TEST_COMMONWEALTH.md entry 3). Guidance is moving into canon so that
only ratified rules are in force; a rule an agent ratified in the operator's
name would defeat the move.

WHAT. Bash: each `canon` invocation is classed by its verb.
  read     no ledger write, allowed under any actor
  propose  add, question, draft (not --resume), diff --propose: allowed only
           when the actor canon will resolve starts with `agent:`
  other    refused: approving, objecting, governing, reconfiguring, and
           reviewing a draft (`draft --resume`) are the operator's acts
The classes are an ALLOWLIST, so a verb canon adds later is refused until it is
classed here. canon exposes no machine-readable verb class to read instead.
Edit/Write of a ledger (`acts.jsonl`) or a draft run (`draft-runs/*.json`) is
refused, and so is a Bash command naming `acts.jsonl` under anything but a
reader: acts come from canon, never a hand edit.

NOT A SECURITY BOUNDARY. A label is spoofable by design (ratify.rs `is_human`
is a prefix check) and `eval` or a script file hides an invocation from any
lexer. This stops an honest agent writing as a person by default.

Exit 2 refuses (the harness withholds the call and shows stderr). A command
that will not lex exits 0 and names the skip in additionalContext.
"""
from __future__ import annotations

import json
import os
import re
import shlex
import sys
from pathlib import PurePath

READ_VERBS = {"list", "why", "log", "open", "who", "pool", "overdue", "voice",
              "check", "tensions", "share", "mcp", "help", "replay", "witness"}
SHOW_VERBS = {"config", "policy", "ratification", "guard", "draw"}  # `<verb> show`
PROPOSE_VERBS = {"add", "question", "draft", "diff"}
HELP_FLAGS = {"-h", "--help", "-V", "--version"}
DRAFT_NO_WRITE = {"--dry-run", "--replay", "--refold"}

READERS = {"cat", "head", "tail", "wc", "grep", "rg", "jq", "less", "more", "ls",
           "stat", "file", "shasum", "sha256sum", "md5", "diff", "cmp", "echo",
           "printf"}
GIT_READS = {"log", "diff", "show", "blame", "status", "add", "commit", "ls-files",
             "grep"}
GIT_OPTS_WITH_ARG = {"-C", "-c", "--git-dir", "--work-tree"}
WRITE_REDIRECTS = {">", ">>", ">|", "&>", "&>>"}
REDIRECTS = WRITE_REDIRECTS | {"<", "<<", "<<<", ">&", "<&", "<>"}
WRAPPERS = {"command", "exec", "time", "nohup", "builtin"}
SHELLS = {"bash", "sh", "zsh", "dash"}
ASSIGN_RX = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$", re.S)
HEREDOC_RX = re.compile(r"(?<!<)<<-?[ \t]*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")

OPERATOR_ROLE = ("approving, objecting, governing, reconfiguring and reviewing "
                 "drafts are the operator's acts. Hand the operator the exact "
                 "command to run in their own terminal (not through `!`, which "
                 "inherits this session's CANON_ACTOR).")


def envelope() -> dict:
    raw = os.environ.get("SOVEREIGN_HOOK_INPUT")
    try:
        return json.loads(raw) if raw else json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        return {}


def strip_heredocs(cmd: str) -> str:
    """Drop heredoc bodies: their lines are data, and a commit message that
    mentions `canon approve` must not read as an invocation."""
    out, until = [], []
    for line in cmd.replace("\\\n", " ").split("\n"):
        if until:
            if line.strip() == until[0]:
                until.pop(0)
            continue
        out.append(line)
        until = [m.group(2) for m in HEREDOC_RX.finditer(line)]
    return "\n".join(out)


def simple_commands(cmd: str) -> list[list[str]]:
    """Token lists, one per simple command; an unquoted newline separates two.
    Redirect tokens stay inside their command. Raises ValueError on bad quoting."""
    lex = shlex.shlex(strip_heredocs(cmd), posix=True, punctuation_chars=";&|()<>\n")
    lex.whitespace = " \t\r"
    lex.whitespace_split = True
    lex.commenters = ""
    segs, cur = [], []
    for tok in lex:
        is_punct = tok and all(c in ";&|()<>\n" for c in tok)
        if is_punct and ("\n" in tok or tok not in REDIRECTS):
            if cur:
                segs.append(cur)
            cur = []
        else:
            cur.append(tok)
    if cur:
        segs.append(cur)
    return segs


def classify(args: list[str]) -> tuple[str, str]:
    """(class, verb) for canon's argv after the program name."""
    words = [a for a in args if not a.startswith("-")]
    if not words:
        return "read", "help"
    verb = words[0]
    if verb in READ_VERBS:
        return "read", verb
    if verb in SHOW_VERBS:
        sub = words[1] if len(words) > 1 else ""
        return ("read" if sub == "show" else "adjudicate"), f"{verb} {sub}".strip()
    if verb == "draft":
        if "--resume" in args:
            return "adjudicate", "draft --resume"
        return ("read" if DRAFT_NO_WRITE & set(args) else "propose"), verb
    if verb == "diff":
        return ("propose" if "--propose" in args else "read"), verb
    if verb in PROPOSE_VERBS:
        return "propose", verb
    return "adjudicate", verb


def acts_write(seg: list[str]) -> bool:
    """Does this simple command name a canon ledger under a writer?"""
    mentions, prev = False, ""
    for tok in seg:
        if "acts.jsonl" in tok:
            if prev in WRITE_REDIRECTS:
                return True
            if prev not in ("<", "<<<"):
                mentions = True
        prev = tok
    if not mentions:
        return False
    i = 0
    while i < len(seg) and ASSIGN_RX.match(seg[i]):
        i += 1
    prog = PurePath(seg[i]).name if i < len(seg) else ""
    if prog in READERS:
        return False
    if prog == "git":
        i += 1
        while i < len(seg) and seg[i].startswith("-"):
            i += 2 if seg[i] in GIT_OPTS_WITH_ARG else 1
        return not (i < len(seg) and seg[i] in GIT_READS)
    return True


def judge_bash(cmd: str, actor: str | None, depth: int = 0) -> tuple[str | None, str | None]:
    """(refusal, actor after the command). `actor` is what canon would see:
    None means CANON_ACTOR is unset, so canon falls back to the git name."""
    for seg in simple_commands(cmd):
        if acts_write(seg):
            return ("a canon ledger (acts.jsonl) is appended by canon alone; read "
                    "it with `canon log`, change it with a canon verb"), actor
        local, i = actor, 0
        assigned_only = True
        while i < len(seg):
            tok = seg[i].lstrip("`")
            m = ASSIGN_RX.match(tok)
            if m:
                if m.group(1) == "CANON_ACTOR":
                    local = m.group(2)
                i += 1
                continue
            assigned_only = False
            if tok in WRAPPERS:
                i += 1
            elif tok == "env":
                i += 1
                while i < len(seg) and seg[i].startswith("-"):
                    if seg[i] in ("-i", "--ignore-environment", "-"):
                        local = None
                    elif seg[i] in ("-u", "--unset") and i + 1 < len(seg):
                        if seg[i + 1] == "CANON_ACTOR":
                            local = None
                        i += 1
                    i += 1
            elif tok in ("export", "declare", "typeset") or tok == "unset":
                for a in seg[i + 1:]:
                    m2 = ASSIGN_RX.match(a)
                    if tok == "unset" and a == "CANON_ACTOR":
                        actor = None
                    elif m2 and m2.group(1) == "CANON_ACTOR":
                        actor = m2.group(2)
                break
            else:
                break
        if assigned_only:
            actor = local                            # CANON_ACTOR=x on its own
            continue
        if i >= len(seg):
            continue
        prog, args = PurePath(seg[i].lstrip("`")).name, seg[i + 1:]
        if prog in SHELLS and "-c" in args and depth < 3:
            inner = args[args.index("-c") + 1] if args.index("-c") + 1 < len(args) else ""
            refusal, _ = judge_bash(inner, local, depth + 1)
            if refusal:
                return refusal, actor
            continue
        if prog != "canon" or HELP_FLAGS & set(args):
            continue
        klass, verb = classify(args)
        if klass == "adjudicate":
            return (f"`canon {verb}` is neither a read nor a proposal, so it is "
                    f"refused: {OPERATOR_ROLE}"), actor
        if klass == "propose" and not (local or "").startswith("agent:"):
            who = local if local is not None else "human:<git user.name>"
            return (f"`canon {verb}` would be recorded as `{who}`. An agent proposes "
                    f"only under CANON_ACTOR=agent:<harness>; set it in the "
                    f"harness, never on the command line as a person"), actor
    return None, actor


def judge_path(path: str) -> str | None:
    p = PurePath(path)
    if p.name == "acts.jsonl":
        return ("a canon ledger (acts.jsonl) is appended by canon alone; change it "
                "with a canon verb")
    if "draft-runs" in p.parts and p.suffix == ".json":
        return ("a draft run is written by `canon draft`; hand-forging one breaks "
                "its verbatim-quote claim")
    return None


def main() -> int:
    env = envelope()
    tool = env.get("tool_name", "")
    tin = env.get("tool_input") or {}
    if tool == "Bash":
        try:
            refusal, _ = judge_bash(str(tin.get("command", "")),
                                    os.environ.get("CANON_ACTOR"))
        except ValueError as e:
            print(json.dumps({"hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": f"canon-actor-guard skipped: command did not lex ({e})"}}))
            return 0
    elif tool in ("Edit", "Write"):
        refusal = judge_path(str(tin.get("file_path", "")))
    else:
        return 0
    if refusal:
        print(f"canon-actor-guard: {refusal}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
