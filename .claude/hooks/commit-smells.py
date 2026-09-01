#!/usr/bin/env python3
"""PreToolUse (Bash) hook: the ARCH §15 smell rows, at the commit, next to the diff.

WHY HERE. The eleven principles sit at token 0 of every session and are
followed well early. Past ~300k of context the recent window is diffs and
cargo output; nothing in it looks like a principle, and a block the model
has already discounted does not shape a decision made 300k tokens later
(operator observation, 2026-09-01). What does shape it is the principle
arriving NEXT TO the lines it judges, in the turn where fixing is still
cheap. The commit is that turn and the diff is those lines.

WHAT. On a Bash call that runs `git commit`: replay any `git add` (or `-a`,
or pathspecs) from the same command into a TEMPORARY index, so the audited
diff is what the commit would record rather than what the real index held
before the call; run the model-free gate of scripts/co-arch.py over it
(rules: quality/arch-probes.toml, one source, ARCH §10.6); hand back the
matched sites. The agent is the judge; the nightly audit puts the same
questions to a model. No second set of greps lives here (§19).

BLOCKS ONCE. exit 2 (the harness feeds stderr back and withholds the tool)
for a site not yet shown in this session; a site never blocks twice, the
ledger lives under the session dir. A commit is therefore never stuck: the
same command re-issued proceeds, and a deliberate deviation is named in
the body rather than left silent (§18.3). The git-side pre-commit hook
stays warn-only (operator directive 2026-08-06): this interposes one
review turn for the agent and never gates the human.

COST. A non-commit Bash call: one regex, no subprocess. A commit: an index
copy, one diff, one `git grep` per backticked symbol in the message.

NEVER RAISES OUT. Its own failure exits 0; when that means a check was
skipped, the skip is named in additionalContext (§18.3, absence reported).

Opt-out: SOVEREIGN_NO_COMMIT_SMELLS=1. The engine's location is
CO_ARCH_SCRIPT, the rule set CO_ARCH_PROFILE (both read by the tests).
"""
from __future__ import annotations

import importlib.util
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

# Sites per rule shown to the agent. The agent already holds the diff, so a
# site is a pointer, not evidence; the model bundle needs the profile's
# max_sites=16 because the model holds nothing else (arch-probes.toml sweep).
MAX_SITES = 4
SESSIONS_ROOT = Path(os.environ.get("SOVEREIGN_SESSIONS_DIR")
                     or Path.home() / ".svrnmesh" / "sessions")
LEDGER_NAME = "commit-smells-shown.json"
COMMIT_RX = re.compile(
    r"(?:^|[;&|(\s])git\s+(?:-C\s+\S+\s+|-c\s+\S+\s+|--[\w-]+(?:=\S+)?\s+)*commit\b")
# `-m "$(cat <<'EOF' … EOF)"`: the message form the harness favours. Stashed
# before lexing because the body may hold quotes that would end the token.
HEREDOC_RX = re.compile(
    r"\$\(\s*cat\s*<<-?\s*['\"]?(\w+)['\"]?[ \t]*\n(.*?)\n[ \t]*\1[ \t]*\n?\s*\)", re.S)
STASH_RX = re.compile(r"__COMMIT_SMELLS_HEREDOC_(\d+)__")
OPERATORS = {"&&", "||", ";", ";;", "|", "|&", "&", "(", ")"}
GIT_OPTS_WITH_ARG = {"-C", "-c", "--git-dir", "--work-tree"}
COMMIT_LONG_WITH_ARG = {"--author", "--date", "--cleanup", "--fixup", "--squash",
                        "--reuse-message", "--reedit-message", "--trailer",
                        "--template", "--pathspec-from-file"}
INTERACTIVE_ADD = {"-p", "--patch", "-i", "--interactive", "-e", "--edit"}


def envelope() -> dict:
    raw = os.environ.get("SOVEREIGN_HOOK_INPUT")
    try:
        return json.loads(raw) if raw else json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        return {}


def advise(msg: str) -> int:
    print(json.dumps({"hookSpecificOutput": {
        "hookEventName": "PreToolUse", "additionalContext": msg}}))
    return 0


def find_repo(env: dict) -> Path | None:
    start = os.environ.get("CLAUDE_PROJECT_DIR") or env.get("cwd") or os.getcwd()
    r = subprocess.run(["git", "-C", start, "rev-parse", "--show-toplevel"],
                       capture_output=True, text=True)
    return Path(r.stdout.strip()) if r.returncode == 0 and r.stdout.strip() else None


# --------------------------------------------------------------------------
# what would this call commit?
# --------------------------------------------------------------------------

def parse_command(cmd: str) -> dict:
    """The `git add` segments before the commit, `-a`, pathspecs, and the
    message (from -m, -F, or the stashed heredoc form)."""
    plan = {"adds": [], "all": False, "paths": [], "message": "",
            "message_known": True, "note": None}
    bodies: list[str] = []

    def stash(m):
        bodies.append(m.group(2))
        return f"__COMMIT_SMELLS_HEREDOC_{len(bodies) - 1}__"

    def unstash(v):
        m = STASH_RX.fullmatch(v or "")
        return bodies[int(m.group(1))] if m else v

    flat = HEREDOC_RX.sub(stash, cmd)
    try:
        lex = shlex.shlex(flat, posix=True, punctuation_chars=True)
        lex.whitespace_split = True
        toks = list(lex)
    except ValueError as e:
        plan["note"] = (f"could not parse the command ({e}); audited the index "
                        f"as-is, message not checked")
        plan["message_known"] = False
        return plan

    segs: list[list[str]] = []
    cur: list[str] = []
    for t in toks:
        if t in OPERATORS:
            if cur:
                segs.append(cur)
            cur = []
        else:
            cur.append(t)
    if cur:
        segs.append(cur)

    for seg in segs:
        i = 0
        while i < len(seg) and re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", seg[i]):
            i += 1                                   # FOO=bar git …
        if i >= len(seg) or seg[i] != "git":
            continue
        i += 1
        while i < len(seg) and seg[i].startswith("-"):
            i += 2 if seg[i] in GIT_OPTS_WITH_ARG else 1
        if i >= len(seg):
            continue
        verb, args = seg[i], seg[i + 1:]
        if verb == "add":
            plan["adds"].append(args)
        elif verb == "commit":
            _parse_commit_args(args, plan, unstash)
            break                                    # later segments run after it
    return plan


def _read_message_file(val: str, plan: dict):
    if val in ("", "-"):
        return None                                  # stdin: not recoverable here
    try:
        return Path(val).read_text(encoding="utf-8")
    except OSError:
        return None


def _message_arg(raw: str, unstash):
    """A `-m` value. A stashed heredoc is fully resolved whatever its body
    says; any OTHER value still holding `$(` is a command substitution the
    shell would have run and we cannot (`-m "$(cat notes)"`), so it is
    unknown rather than taken literally. Prose that merely mentions `$(`
    inside a heredoc or an -F file is therefore never mistaken for one
    (that misfire was watched live on this hook's own first commit)."""
    if STASH_RX.fullmatch(raw or ""):
        return unstash(raw)
    return None if "$(" in (raw or "") else raw


def _parse_commit_args(args: list[str], plan: dict, unstash) -> None:
    msgs: list = []
    i, after_dd = 0, False
    while i < len(args):
        a = args[i]
        if after_dd:
            plan["paths"].append(a)
        elif a == "--":
            after_dd = True
        elif a.startswith("--"):
            key, eq, val = a.partition("=")
            if key in ("--message", "--file"):
                if not eq:
                    val = args[i + 1] if i + 1 < len(args) else ""
                    i += 1
                msgs.append(_message_arg(val, unstash) if key == "--message"
                            else _read_message_file(val, plan))
            elif key == "--all":
                plan["all"] = True
            elif key in COMMIT_LONG_WITH_ARG and not eq:
                i += 1
        elif a.startswith("-") and len(a) > 1:
            j = 1
            while j < len(a):
                ch = a[j]
                if ch in "mF":                       # -m X, -mX, -am X
                    val = a[j + 1:]
                    if not val:
                        val = args[i + 1] if i + 1 < len(args) else ""
                        i += 1
                    msgs.append(_message_arg(val, unstash) if ch == "m"
                                else _read_message_file(val, plan))
                    break
                if ch == "a":
                    plan["all"] = True
                elif ch in "Cct":                    # -C <commit>, -c <commit>, -t <file>
                    if not a[j + 1:]:
                        i += 1
                    break
                j += 1
        else:
            plan["paths"].append(a)
        i += 1
    plan["message"] = "\n\n".join(m for m in msgs if m is not None)
    if any(m is None for m in msgs):
        plan["message_known"] = False


def temp_index(repo: Path, plan: dict) -> tuple[Path | None, str | None]:
    """A COPY of the real index with this call's adds replayed into it, or
    None when nothing needs replaying. The real index is never written."""
    if not (plan["adds"] or plan["all"] or plan["paths"]):
        return None, None
    r = subprocess.run(["git", "-C", str(repo), "rev-parse", "--absolute-git-dir"],
                       capture_output=True, text=True)
    if r.returncode != 0:
        return None, "could not locate .git; audited the real index as-is"
    src = Path(r.stdout.strip()) / "index"
    fd, tmp = tempfile.mkstemp(prefix="commit-smells-index-")
    os.close(fd)
    tmp_path = Path(tmp)
    if src.exists():
        shutil.copyfile(src, tmp_path)
    else:
        tmp_path.unlink()                            # git creates a fresh one
    env = {**os.environ, "GIT_INDEX_FILE": str(tmp_path)}
    notes: list[str] = []

    def add(args: list[str], label: str) -> None:
        if any(a in INTERACTIVE_ADD for a in args):
            notes.append(f"`git add {' '.join(args)}` is interactive; not replayed")
            return
        rr = subprocess.run(["git", "-C", str(repo), "add", *args],
                            capture_output=True, text=True, env=env)
        if rr.returncode != 0:
            notes.append(f"replay of `{label}` failed: "
                         f"{rr.stderr.strip().splitlines()[-1][:100] if rr.stderr.strip() else 'rc ' + str(rr.returncode)}")

    for args in plan["adds"]:
        add(args, "git add " + " ".join(args))
    if plan["all"]:
        add(["-u"], "git commit -a")
    if plan["paths"]:
        add(["--", *plan["paths"]], "git commit -- " + " ".join(plan["paths"]))
    return tmp_path, ("; ".join(notes) or None)


# --------------------------------------------------------------------------
# the engine, the ledger, the render
# --------------------------------------------------------------------------

def load_engine(repo: Path):
    script = Path(os.environ.get("CO_ARCH_SCRIPT")
                  or Path(__file__).resolve().parents[2] / "scripts" / "co-arch.py")
    if not script.exists():
        raise FileNotFoundError(f"engine not found at {script} (set CO_ARCH_SCRIPT)")
    os.environ["CO_ARCH_REPO"] = os.environ.get("CO_ARCH_REPO") or str(repo)
    spec = importlib.util.spec_from_file_location("co_arch", script)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)                     # type: ignore[union-attr]
    return mod


def ledger_load(session_id: str) -> set[str]:
    try:
        return set(json.loads((SESSIONS_ROOT / session_id / LEDGER_NAME)
                              .read_text(encoding="utf-8")))
    except (OSError, ValueError):
        return set()


def ledger_save(session_id: str, keys: set[str]) -> None:
    try:
        d = SESSIONS_ROOT / session_id
        d.mkdir(parents=True, exist_ok=True)
        (d / LEDGER_NAME).write_text(json.dumps(sorted(keys)), encoding="utf-8")
    except OSError:
        pass


def render(found: list[dict], n_new: int, note: str | None) -> str:
    mark = {"decided": "B", "question": "?"}
    lines = [f"commit-smells: {n_new} site(s) in this commit match ARCH §15 smell "
             f"rows not yet shown this session.", ""]
    for f in found:
        head = f"  {mark[f['kind']]}  {f['id']} · {f['sec']}"
        if f["kind"] == "decided":
            head += f" · {f['text']}"
        lines.append(head)
        if f["kind"] == "question":
            lines.append(f"       {f['text']}")
        for c in f["cites"][:MAX_SITES]:
            lines.append(f"       {c}")
        if len(f["cites"]) > MAX_SITES:
            lines.append(f"       … {len(f['cites']) - MAX_SITES} more site(s) not shown")
        lines.append("")
    lines += [
        "B = decided by code. ? = a question the nightly audit puts to a model; "
        "here you answer it, with the diff in front of you.",
        "Fix what is real. Re-issue the same commit to proceed: these sites do "
        "not block again this session.",
        "A deliberate deviation is named in the commit body, never left silent (§18.3).",
        "Open sovereign/ARCH_PRINCIPLES.md at the cited section before deciding; "
        "do not recall it (§11.1).",
    ]
    if note:
        lines += ["", f"note: {note}"]
    return "\n".join(lines)


def main() -> int:
    if os.environ.get("SOVEREIGN_NO_COMMIT_SMELLS") == "1":
        return 0
    env = envelope()
    if env.get("tool_name", "Bash") != "Bash":
        return 0
    cmd = (env.get("tool_input") or {}).get("command") or ""
    if not COMMIT_RX.search(cmd):
        return 0                                     # the common case: no subprocess
    repo = find_repo(env)
    if repo is None:
        return 0
    session_id = env.get("session_id") or ""
    plan = parse_command(cmd)
    notes = [plan["note"]] if plan["note"] else []
    if not plan["message_known"]:
        notes.append("commit message not recovered from the command; "
                     "§11.1 (uncited-symbol) not checked")

    tmp = None
    try:
        tmp, replay_note = temp_index(repo, plan)
        if replay_note:
            notes.append(replay_note)
        if tmp is not None:
            os.environ["GIT_INDEX_FILE"] = str(tmp)
        try:
            ca = load_engine(repo)
            prof = ca.load_profile()
        except Exception as e:                       # noqa: BLE001 — named, never raised
            return advise(f"commit-smells: skipped — {e}")
        added, files = ca.collect_staged(prof["globs"])
        msg = plan["message"] if plan["message_known"] else ""
        found = ca.findings(added, files, msg, prof)
    finally:
        if tmp is not None:
            os.environ.pop("GIT_INDEX_FILE", None)
            tmp.unlink(missing_ok=True)

    notes += [f"{f['id']}: {f['text']}" for f in found if f["kind"] == "unjudged"]
    found = [f for f in found if f["cites"]]
    if not found:
        return advise("commit-smells: " + "; ".join(notes)) if notes else 0

    keys = {f"{f['id']}|{c}" for f in found for c in f["cites"]}
    if not session_id:
        return advise(render(found, len(keys),
                             "no session id in the hook envelope, so this is advisory "
                             "(nothing to ledger what was shown against)"))
    shown = ledger_load(session_id)
    new_keys = keys - shown
    if not new_keys:
        rules = ", ".join(sorted({k.split("|", 1)[0] for k in keys}))
        return advise(f"commit-smells: {len(keys)} previously shown site(s) still "
                      f"present ({rules}); proceeding. If deliberate, the body says so.")
    ledger_save(session_id, shown | keys)
    fresh = []
    for f in found:
        cites = [c for c in f["cites"] if f"{f['id']}|{c}" in new_keys]
        if cites:
            fresh.append({**f, "cites": cites})
    print(render(fresh, len(new_keys), "; ".join(notes) or None), file=sys.stderr)
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:                           # noqa: BLE001
        sys.exit(advise(f"commit-smells: skipped — {type(e).__name__}: {e}"))
