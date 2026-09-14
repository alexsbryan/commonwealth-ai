#!/usr/bin/env bash
# canon-ratify.py — answers canon's review by candidate text, never by position.
#
# WHY THIS EXISTS. canon-ratify.py types answers into `canon draft --resume` in
# the operator's terminal, so an answer that lands on the wrong candidate is a
# rule nobody ratified, written under their name. A stub prints canon's review
# prompt in the format of canon's draft.rs `review` and wrap.rs `hang_at`, at a
# narrow width so candidate text wraps. Cases: a wrapped text with quotes and a
# `because` line, a reject, an edit, a text not in the verdicts, a source that
# differs from the judged one, an unratified group, and a repeat in one sitting.
# `--plan` must send only skips, and an agent CANON_ACTOR must be refused. With
# the source check removed, the mismatch case fails (watched 2026-09-13).
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
SCRIPT="$ROOT/scripts/canon-ratify.py"
[[ -f "$SCRIPT" ]] || { echo "cannot find $SCRIPT"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

SCRIPT="$SCRIPT" T="$T" python3 - <<'PY'
import json, os, subprocess, sys

SCRIPT, d = os.environ["SCRIPT"], os.environ["T"]
STUB = r'''
import json, os, sys
def hang(prefix, text, width=48):
    pad, out, col = " " * len(prefix), prefix, 0
    avail = max(width - len(prefix), 16)
    for w in text.split():
        if col and col + 1 + len(w) > avail:
            out += "\n" + pad; col = 0
        elif col:
            out += " "; col += 1
        out += w; col += len(w)
    return out
cands = [json.loads(l) for l in open(os.environ["STUB_CANDS"])]
rec = open(os.environ["STUB_RECORD"], "w")
print("resuming x.json — %d of %d left, no model call" % (len(cands), len(cands)))
for n, c in enumerate(cands, 1):
    print("\nCandidate %d of %d" % (n, len(cands)))
    print(hang("  %-9s \"" % "rule", c["text"] + "\""))
    if c.get("because"):
        print(hang("        because: ", c["because"]))
    print()
    print("  from %s:" % c["source"])
    for l in c["quote"].splitlines():
        print("    " + l)
    sys.stdout.write("\n  [a]ccept  [e]dit  [r]eject  [s]kip  [q]uit: "); sys.stdout.flush()
    line = sys.stdin.readline()
    if not line:
        print("\n(end of input)"); break
    print()
    ans, edit = line.strip(), None
    if ans == "e":
        sys.stdout.write("  text: "); sys.stdout.flush()
        edit = sys.stdin.readline().strip()
    rec.write(json.dumps({"text": c["text"], "answer": ans, "edit": edit}) + "\n")
    if ans == "q":
        break
    if ans in ("a", "e"):
        print("  can-%012x" % n)
'''
LONG = ("A signed, verified, shippable artifact must never live ONLY inside a directory "
        "a routine dev command is entitled to delete; stage it \"outside\" target/.")
cands = [
    {"text": LONG, "source": "invariant/a.md:3-9", "quote": "q1\nq1b", "because": "a reason that also wraps across the narrow width"},
    {"text": "Direct-inbound only.", "source": "attempt/b.md:3-3", "quote": "q2"},
    {"text": "Use  debug   builds.", "source": "memory/c.md:1-2", "quote": "q3"},
    {"text": "Not in the verdict file.", "source": "invariant/d.md:1-1", "quote": "q4"},
    {"text": "Same text, other source.", "source": "invariant/e.md:1-1", "quote": "q5"},
    {"text": "Rule in an unratified group.", "source": "invariant/f.md:1-1", "quote": "q6"},
    {"text": "Direct-inbound only.", "source": "attempt/b.md:3-3", "quote": "q2 again"},
]
verdicts = [
    {"text": LONG, "source": "invariant/a.md:3-9", "verdict": "accept", "group": "g-accept", "edit_text": None},
    {"text": "Direct-inbound only.", "source": "attempt/b.md:3-3", "verdict": "reject", "group": "g-reject", "edit_text": None},
    {"text": "Use debug builds.", "source": "memory/c.md:1-2", "verdict": "edit", "group": "g-accept", "edit_text": "Use debug builds, not release."},
    {"text": "Same text, other source.", "source": "invariant/zzz.md:1-1", "verdict": "accept", "group": "g-accept", "edit_text": None},
    {"text": "Rule in an unratified group.", "source": "invariant/f.md:1-1", "verdict": "accept", "group": "g-later", "edit_text": None},
]
for name, rows in (("cands.jsonl", cands), ("verdicts.jsonl", verdicts)):
    with open(os.path.join(d, name), "w") as f:
        f.writelines(json.dumps(r) + "\n" for r in rows)
open(os.path.join(d, "stub.py"), "w").write(STUB)
env = dict(os.environ, STUB_CANDS=os.path.join(d, "cands.jsonl"), STUB_RECORD=os.path.join(d, "record.jsonl"))
env.pop("CANON_ACTOR", None)
rc = 0


def check(ok, label):
    global rc
    print(("  ok    " if ok else "  FAIL  ") + label)
    rc |= 0 if ok else 1


def run(extra, e=env):
    return subprocess.run([sys.executable, SCRIPT, "--verdicts", os.path.join(d, "verdicts.jsonl"), "--run", "x",
                           "--canon", f"{sys.executable} {os.path.join(d, 'stub.py')}", "--log", os.path.join(d, "log.jsonl")] + extra,
                          env=e, capture_output=True, text=True, timeout=30)


p = run(["--groups", "g-accept,g-reject"])
got = [json.loads(l) for l in open(os.path.join(d, "record.jsonl"))]
check([g["answer"] for g in got] == ["a", "r", "e", "s", "s", "s", "s"],
      f"answers by text across a wrapped prompt: {[g['answer'] for g in got]}")
check(got[2]["edit"] == "Use debug builds, not release.", "edit sends the ratified text")
check("source mismatch" in p.stdout, "a source that differs from the judged one is skipped and reported")
log = [json.loads(l) for l in open(os.path.join(d, "log.jsonl"))]
check([l["act"] for l in log][:3] == ["can-000000000001", None, "can-000000000003"], "act ids logged against their answers")

run(["--plan"])
got = [json.loads(l) for l in open(os.path.join(d, "record.jsonl"))]
check({g["answer"] for g in got} == {"s"}, "--plan sends only skips")

p = subprocess.run([sys.executable, SCRIPT, "--verdicts", os.path.join(d, "verdicts.jsonl"), "--run", "x", "--plan"],
                   env=dict(env, CANON_ACTOR="agent:test"), capture_output=True, text=True)
check(p.returncode != 0 and "agent" in p.stderr, "an agent CANON_ACTOR is refused before canon runs")
sys.exit(rc)
PY
