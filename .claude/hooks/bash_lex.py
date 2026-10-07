"""Bash command lexing shared by the PreToolUse guards in this directory.

A hook runs as `python3 .claude/hooks/<name>.py`, so this directory is on
sys.path and a guard imports it by name. One lexer, so two guards cannot
disagree about where a command ends or what a heredoc body is.
"""
from __future__ import annotations

import re
import shlex

WRITE_REDIRECTS = {">", ">>", ">|", "&>", "&>>"}
REDIRECTS = WRITE_REDIRECTS | {"<", "<<", "<<<", ">&", "<&", "<>"}
WRAPPERS = {"command", "exec", "time", "nohup", "builtin"}
SHELLS = {"bash", "sh", "zsh", "dash"}
ASSIGN_RX = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$", re.S)
HEREDOC_RX = re.compile(r"(?<!<)<<-?[ \t]*(['\"]?)([A-Za-z_][A-Za-z0-9_]*)\1")


def strip_heredocs(cmd: str) -> str:
    """Drop heredoc bodies: their lines are data, and a commit message that
    mentions a guarded command must not read as an invocation."""
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
