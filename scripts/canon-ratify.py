#!/usr/bin/env python3
"""Answer `canon draft --resume <run>` from ratified verdict groups.

The OPERATOR runs this, in their own terminal. It drives canon's own review
loop, the path that writes each accepted rule WITH its source citation and
records each rejection in `.canon/seen` (canon draft.rs `review`), and answers
only for candidates in the groups named on the command line. Everything else
is skipped, and skip records nothing, so an unratified group is offered again
next sitting.

Matched by candidate TEXT, the key canon's resume uses (draft.rs `remaining`),
never by position. A candidate whose source differs from the one the verdict
was judged against is skipped and reported, not answered.

  scripts/canon-ratify.py --verdicts .canon/adjudication/ratify.jsonl --run 1789349217 --plan
  scripts/canon-ratify.py --verdicts .canon/adjudication/ratify.jsonl --run 1789349217 --groups accept-invariant,reject-fragment
"""
import argparse, json, os, re, shlex, subprocess, sys, time

PROMPT = "[a]ccept  [e]dit  [r]eject  [s]kip  [q]uit: "
TEXT_PROMPT = "  text: "
WS = re.compile(r"\s+")


def norm(t):
    return WS.sub(" ", t).strip()


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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--verdicts", required=True)
    ap.add_argument("--run", required=True, help="the draft run to resume, e.g. 1789349217")
    ap.add_argument("--groups", default="", help="comma-separated ratified groups")
    ap.add_argument("--plan", action="store_true", help="print every answer, send skip for all: writes nothing")
    ap.add_argument("--log", default=None)
    ap.add_argument("--canon", default="canon", help="the canon command (tests pass a stub)")
    a = ap.parse_args()

    argv = shlex.split(a.canon)
    if os.path.basename(argv[0]) == "canon" and os.environ.get("CANON_ACTOR", "").startswith("agent:"):
        sys.exit("canon-ratify: CANON_ACTOR is an agent. Answering a review is the operator's act; "
                 "run this in your own terminal. (A label check, not a security boundary.)")
    groups = {g for g in a.groups.split(",") if g}
    if not groups and not a.plan:
        sys.exit("canon-ratify: name the ratified --groups, or pass --plan")
    verdicts = {}
    for line in open(a.verdicts):
        if line.strip():
            r = json.loads(line)
            verdicts[norm(r["text"])] = r
    unknown = groups - {r["group"] for r in verdicts.values()}
    if unknown:
        sys.exit(f"canon-ratify: no such group(s): {', '.join(sorted(unknown))}")
    log = open(a.log or os.path.join(os.path.dirname(os.path.abspath(a.verdicts)), "ratify-log.jsonl"), "a")

    proc = subprocess.Popen(argv + ["draft", "--resume", a.run], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, bufsize=0)
    fd = proc.stdout.fileno()
    count = {"a": 0, "r": 0, "e": 0, "s": 0}
    seen_texts, pending, problems = set(), None, []
    status = 0
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
            status = 3
            break
        r = verdicts.get(text)
        why = "not in verdicts"
        answer = "s"
        if r is not None:
            why = f"{r['group']}"
            if text in seen_texts:
                why = "repeat in this sitting"
            elif source != r["source"]:
                why = f"source mismatch: offered {source}, judged {r['source']}"
                problems.append(why)
            elif r["group"] in groups:
                answer = {"accept": "a", "reject": "r", "edit": "e"}.get(r["verdict"], "s")
        sent = "s" if a.plan else answer
        print(f"[{n}/{of}] {answer}{'' if sent == answer else ' (plan: s)'}  {why}  {source}  {text[:90]}",
              flush=True)
        seen_texts.add(text)
        count[sent] += 1
        proc.stdin.write((sent + "\n").encode())
        if sent == "e":
            _, mk2 = read_until(fd, [TEXT_PROMPT])
            if mk2 is None:
                problems.append("canon closed before the edit text prompt")
                status = 3
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
    sys.exit(status or (1 if rc else 0))


if __name__ == "__main__":
    main()
