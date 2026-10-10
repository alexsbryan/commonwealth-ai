#!/usr/bin/env python3
"""Proposed canon rules, ratified a few at a time.

An agent proposed verdicts on a draft run (.canon/adjudication/ratify.jsonl).
Nobody ratifies ~600 rules in one sitting, so the operator answers a few:

  --ids / --groups
            answer `canon draft --resume <run>` for those proposals only

Ratifying drives canon's own review loop, which writes an accepted rule WITH its
source citation and records a rejection in `.canon/seen` (canon draft.rs
`review`). Candidates are matched by TEXT, the key canon's resume uses (draft.rs
`remaining`), never by position; one whose source differs from the one judged
is skipped and reported. Answering a review is the operator's act, so this
refuses an agent CANON_ACTOR.

  scripts/canon-ratify.py --run 1789349217 --ids n1a2b3c4d,n5e6f7a8b --plan
  scripts/canon-ratify.py --run 1789349217 --ids n1a2b3c4d,n5e6f7a8b

Until 2026-10-10, `--lookup` showed proposals before an edit through
.claude/hooks/intent-warn.py and `--hits` listed the ones edits had hit. The
hook printed to a channel no model reads, so both went with it.
"""
import argparse, json, os, re, shlex, subprocess, sys, time

PROMPT = "[a]ccept  [e]dit  [r]eject  [s]kip  [q]uit: "
TEXT_PROMPT = "  text: "
WS = re.compile(r"\s+")


def norm(t):
    return WS.sub(" ", t).strip()


def repo_root(arg):
    if arg:
        return arg
    out = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True).stdout.strip()
    return out or os.getcwd()


def load_rows(path):
    rows = []
    try:
        with open(path) as f:
            rows = [json.loads(l) for l in f if l.strip()]
    except OSError:
        pass
    return rows


def parse_block(buf):
    """(n, of, source, text) of the last candidate block before a prompt."""
    i = buf.rfind("Candidate ")
    m = re.match(r"Candidate (\d+) of (\d+)\n", buf[i:]) if i >= 0 else None
    if not m:
        raise ValueError("a prompt with no `Candidate n of m` header before it")
    lines = buf[i:].split("\n")[1:]
    text_part = []
    for line in lines:
        s = line.strip()
        if not s or s.startswith("because: "):
            break
        text_part.append(s)
    joined = " ".join(text_part)
    q0, q1 = joined.find('"'), joined.rfind('"')
    if q0 < 0 or q1 <= q0:
        raise ValueError(f"no quoted candidate text in {joined[:200]!r}")
    source = next((l.strip()[len("from "):].rstrip(":").strip()
                   for l in lines if l.startswith("  from ")), None)
    return int(m.group(1)), int(m.group(2)), source, norm(joined[q0 + 1:q1])


def read_until(fd, markers):
    buf = b""
    while True:
        chunk = os.read(fd, 65536)
        if not chunk:
            return buf.decode(errors="replace"), None
        buf += chunk
        s = buf.decode(errors="replace")
        for mk in markers:
            if s.endswith(mk):
                return s, mk


def review(a, canon_argv):
    if os.path.basename(canon_argv[0]) == "canon" and os.environ.get("CANON_ACTOR", "").startswith("agent:"):
        sys.exit("canon-ratify: CANON_ACTOR is an agent. Answering a review is the operator's act; "
                 "run this in your own terminal. (A label check, not a security boundary.)")
    if not a.run:
        sys.exit("canon-ratify: name the draft --run to resume")
    groups = {g for g in a.groups.split(",") if g}
    ids = {i for i in a.ids.split(",") if i}
    if not groups and not ids and not a.plan:
        sys.exit("canon-ratify: name --ids or --groups to ratify, or pass --plan")
    verdicts = {norm(r["text"]): r for r in load_rows(a.verdicts)}
    if not verdicts:
        sys.exit(f"canon-ratify: no verdicts in {a.verdicts}")
    unknown = groups - {r.get("group") for r in verdicts.values()}
    unknown |= ids - {r.get("id") for r in verdicts.values()}
    if unknown:
        sys.exit(f"canon-ratify: no such group or id: {', '.join(sorted(unknown))}")
    select = (lambda r: True) if a.plan and not groups and not ids else \
        (lambda r: r.get("group") in groups or r.get("id") in ids)
    log = open(a.log or os.path.join(os.path.dirname(os.path.abspath(a.verdicts)), "ratify-log.jsonl"), "a")

    proc = subprocess.Popen(canon_argv + ["draft", "--resume", a.run], cwd=a.root, stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, bufsize=0)
    fd = proc.stdout.fileno()
    count = {"a": 0, "r": 0, "e": 0, "s": 0}
    seen_texts, pending, problems = set(), None, []
    rc_status = 0
    while True:
        buf, mk = read_until(fd, [PROMPT])
        act = re.search(r"^\s*(can-[0-9a-f]+)\s*$", buf, re.M)
        if pending:
            pending["act"] = act.group(1) if act else None
            log.write(json.dumps(pending, ensure_ascii=False) + "\n")
            pending = None
        if mk is None:
            break
        try:
            n, of, source, text = parse_block(buf)
        except ValueError as e:
            problems.append(str(e))
            proc.stdin.write(b"q\n")
            rc_status = 3
            break
        r = verdicts.get(text)
        why, answer = "not in verdicts", "s"
        if r is not None:
            why = r.get("group", "")
            if text in seen_texts:
                why = "repeat in this sitting"
            elif source != r["source"]:
                why = f"source mismatch: offered {source}, judged {r['source']}"
                problems.append(why)
            elif select(r):
                answer = {"accept": "a", "reject": "r", "edit": "e"}.get(r["verdict"], "s")
        sent = "s" if a.plan else answer
        if answer != "s" or why.startswith("source mismatch"):
            print(f"[{n}/{of}] {answer}{'' if sent == answer else ' (plan: s)'}  {why}  {source}  {text[:90]}",
                  flush=True)
        seen_texts.add(text)
        count[sent] += 1
        proc.stdin.write((sent + "\n").encode())
        if sent == "e":
            _, mk2 = read_until(fd, [TEXT_PROMPT])
            if mk2 is None:
                problems.append("canon closed before the edit text prompt")
                rc_status = 3
                break
            proc.stdin.write((norm(r["edit_text"]) + "\n").encode())
        pending = {"ts": int(time.time()), "run": a.run, "n": n, "source": source, "text": text,
                   "answer": sent, "planned": answer, "why": why}
    try:
        proc.stdin.close()
    except BrokenPipeError:
        pass
    rc = proc.wait()
    print(f"\nanswered: accept {count['a']}  reject {count['r']}  edit {count['e']}  skip {count['s']}"
          f"  (canon exit {rc})")
    for p in problems:
        print(f"  problem: {p}")
    return rc_status or (1 if rc else 0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=None, help="the repository holding .canon/ (default: git toplevel)")
    ap.add_argument("--verdicts", default=None, help="default: <root>/.canon/adjudication/ratify.jsonl")
    ap.add_argument("--canon", default="canon", help="the canon command (tests pass a stub)")
    ap.add_argument("--run", default=None, help="the draft run to resume, e.g. 1789349217")
    ap.add_argument("--groups", default="")
    ap.add_argument("--ids", default="")
    ap.add_argument("--plan", action="store_true", help="print the answers, send skip for all: writes nothing")
    ap.add_argument("--log", default=None)
    a = ap.parse_args()
    a.root = repo_root(a.root)
    a.verdicts = a.verdicts or os.path.join(a.root, ".canon", "adjudication", "ratify.jsonl")
    canon_argv = shlex.split(a.canon)
    return review(a, canon_argv)


if __name__ == "__main__":
    sys.exit(main())
