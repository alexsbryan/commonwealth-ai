#!/usr/bin/env python3
"""Proposed canon rules: shown when work touches them, ratified a few at a time.

An agent proposed verdicts on a draft run (.canon/adjudication/ratify.jsonl).
Nobody ratifies ~600 rules in one sitting, so a proposal waits until work runs
into it:

  --lookup  the pre-Edit hook (.claude/hooks/intent-warn.py) passes the file and
            the edit text; prints the rules whose backticked names the edit
            touches, each labelled ratified (can-…) or proposed, and logs each
            proposed one to .canon/adjudication/hits.jsonl
  --hits    the proposals edits have hit that are still undecided, and the one
            command that ratifies them (`--brief`: one line or nothing, for the
            session boot block)
  --ids / --groups
            answer `canon draft --resume <run>` for those proposals only

Ratifying drives canon's own review loop, which writes an accepted rule WITH its
source citation and records a rejection in `.canon/seen` (canon draft.rs
`review`). Candidates are matched by TEXT, the key canon's resume uses (draft.rs
`remaining`), never by position; one whose source differs from the one judged
is skipped and reported. Answering a review is the operator's act, so that mode
refuses an agent CANON_ACTOR.

A proposal's status is decided here and nowhere else: ratified when its recorded
text is a commitment `canon list --json` returns, rejected when its candidate
text is declined in `.canon/seen`, proposed otherwise.

  scripts/canon-ratify.py --hits
  scripts/canon-ratify.py --run 1789349217 --ids n1a2b3c4d,n5e6f7a8b --plan
  scripts/canon-ratify.py --run 1789349217 --ids n1a2b3c4d,n5e6f7a8b
"""
import argparse, collections, hashlib, json, os, re, shlex, subprocess, sys, time

PROMPT = "[a]ccept  [e]dit  [r]eject  [s]kip  [q]uit: "
TEXT_PROMPT = "  text: "
WS = re.compile(r"\s+")
TICK = re.compile(r"`([^`\n]+)`")
IDENT = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
FILENAME = re.compile(r"[A-Za-z0-9_\-]+\.[a-z]{1,5}\b")
# Which draft run a proposal is offered in. The adjudication keyed each row to
# the runs that held its text; the rescued run is no longer in .canon/draft-runs.
RUN_OF = {"rerun": "1789349217"}
MAX_SHOWN = 5


def norm(t):
    return WS.sub(" ", t).strip()


def short_digest(text):
    # canon's key for `.canon/seen` (canon-core id.rs `short_digest`): sha256, first 8 hex.
    return hashlib.sha256(text.encode()).hexdigest()[:8]


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


def commitments(canon_argv, root):
    """recorded text -> (id, status) from canon's own fold, or None when canon cannot be read."""
    try:
        p = subprocess.run(canon_argv + ["list", "--json"], cwd=root, capture_output=True, text=True, timeout=5)
        d = json.loads(p.stdout)
    except Exception:
        return None
    return {norm(c["text"]): (c["id"], (c.get("status") or {}).get("status", "?"))
            for c in d.get("commitments", [])}


def declined(root):
    try:
        with open(os.path.join(root, ".canon", "seen")) as f:
            return {l.split()[0] for l in f if l.strip().endswith(" rejected")}
    except OSError:
        return set()


def status(row, known, rejected):
    """('ratified'|'rejected'|'proposed'|'unknown'|<canon status>, can-id or None)."""
    recorded = row.get("edit_text") if row.get("verdict") == "edit" else row["text"]
    if known is not None and norm(recorded) in known:
        cid, st = known[norm(recorded)]
        return ("ratified" if st == "active" else st), cid
    if short_digest(row["text"]) in rejected:
        return "rejected", None
    return ("proposed" if known is not None else "unknown"), None


def referents(text):
    """The specific names a rule is about: backticked file names and shaped identifiers."""
    names = set()
    for tok in TICK.findall(text):
        tok = re.sub(r":\d+(-\d+)?$", "", tok.strip().split("(")[0].rstrip(".,;:"))
        if " " in tok or re.search(r"[<>…*]|\.\.\.", tok):
            continue
        last = tok.rstrip("/").split("/")[-1].split("::")[-1]
        if len(last) >= 5 and (re.search(r"[_.]", last) or len(re.findall(r"[A-Z]", last)) >= 2):
            names.add(last)
    return names


def lookup(a, canon_argv):
    root = a.root
    edit = sys.stdin.read()[:200_000] if not sys.stdin.isatty() else ""
    touched = set(IDENT.findall(edit)) | set(FILENAME.findall(edit))
    if a.file:
        touched.add(os.path.basename(a.file))
    if not touched:
        return 0
    known = commitments(canon_argv, root)
    rejected = declined(root)
    shown = []
    for cid_text, (cid, st) in (known or {}).items():
        hit = referents(cid_text) & touched
        if hit and st == "active":
            shown.append((f"[{cid}]", cid_text, "", hit, None))
    ratified_texts = set(known or {})
    for r in load_rows(a.verdicts):
        if r.get("verdict") not in ("accept", "edit"):
            continue
        recorded = r["edit_text"] if r["verdict"] == "edit" else r["text"]
        hit = referents(recorded) & touched
        if not hit or norm(recorded) in ratified_texts:
            continue
        st, _ = status(r, known, rejected)
        if st == "rejected":
            continue
        label = f"[proposed {r['id']}]" if st == "proposed" else f"[status unknown {r['id']}: canon list failed]"
        shown.append((label, norm(recorded), f"  (from {r['source']})", hit, r["id"] if st == "proposed" else None))
    if not shown:
        return 0
    print("## Canon rules naming what this edit touches (hook: intent-warn)")
    for label, text, src, hit, _ in shown[:MAX_SHOWN]:
        print(f"- {label} {text[:300]}{src}")
    if len(shown) > MAX_SHOWN:
        print(f"- …and {len(shown) - MAX_SHOWN} more")
    pending = [s[4] for s in shown if s[4]]
    if pending:
        print("  proposed = not yet ratified; the operator ratifies the ones edits hit: "
              "`scripts/canon-ratify.py --hits`")
        try:
            with open(os.path.join(root, ".canon", "adjudication", "hits.jsonl"), "a") as f:
                for rid in pending:
                    f.write(json.dumps({"ts": int(time.time()), "session": a.session, "id": rid,
                                        "file": a.file}) + "\n")
        except OSError as e:
            print(f"  (hit not logged: {e})")
    return 0


def hits(a, canon_argv):
    root = a.root
    counts = collections.Counter(h.get("id") for h in load_rows(os.path.join(root, ".canon", "adjudication", "hits.jsonl")))
    if not counts:
        return 0 if a.brief else print("no proposed rule has come up in an edit yet.") or 0
    rows = {r.get("id"): r for r in load_rows(a.verdicts)}
    known = commitments(canon_argv, root)
    if known is None:
        print("canon: `canon list --json` failed, so the status of hit proposals is unknown.")
        return 1
    rejected = declined(root)
    pending = collections.defaultdict(list)
    unofferable = []
    for rid, n in counts.most_common():
        r = rows.get(rid)
        if not r or status(r, known, rejected)[0] != "proposed":
            continue
        run = r.get("run") or next((RUN_OF[k] for k in (r.get("runs") or {}) if k in RUN_OF), None)
        (pending[run] if run else unofferable).append((rid, n, r))
    total = sum(len(v) for v in pending.values())
    if a.brief:
        if total:
            print(f"_canon: {total} proposed rule(s) came up in edits and await ratification — "
                  f"`scripts/canon-ratify.py --hits`_")
        return 0
    if not total and not unofferable:
        print("every proposal an edit has hit is already ratified or rejected.")
        return 0
    for run, items in pending.items():
        print(f"\nrun {run}:")
        for rid, n, r in items:
            recorded = r["edit_text"] if r["verdict"] == "edit" else r["text"]
            print(f"- {rid}  {'EDIT TO: ' if r['verdict'] == 'edit' else ''}{norm(recorded)[:200]}")
            print(f"    why: {r['reason']}  ({n} edit(s); from {r['source']})")
        ids = ",".join(rid for rid, _, _ in items)
        print(f"\n  scripts/canon-ratify.py --run {run} --ids {ids}")
    for rid, _, r in unofferable:
        print(f"- {rid} is in no run canon can resume: {norm(r['text'])[:120]}")
    return 0


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
    ap.add_argument("--lookup", action="store_true")
    ap.add_argument("--file", default="")
    ap.add_argument("--session", default="")
    ap.add_argument("--hits", action="store_true")
    ap.add_argument("--brief", action="store_true")
    ap.add_argument("--run", default=None, help="the draft run to resume, e.g. 1789349217")
    ap.add_argument("--groups", default="")
    ap.add_argument("--ids", default="")
    ap.add_argument("--plan", action="store_true", help="print the answers, send skip for all: writes nothing")
    ap.add_argument("--log", default=None)
    a = ap.parse_args()
    a.root = repo_root(a.root)
    a.verdicts = a.verdicts or os.path.join(a.root, ".canon", "adjudication", "ratify.jsonl")
    canon_argv = shlex.split(a.canon)
    if a.lookup:
        return lookup(a, canon_argv)
    if a.hits:
        return hits(a, canon_argv)
    return review(a, canon_argv)


if __name__ == "__main__":
    sys.exit(main())
