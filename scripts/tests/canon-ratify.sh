#!/usr/bin/env bash
# canon-ratify.py — proposals shown when an edit touches them, ratified by text.
#
# WHY THIS EXISTS. Two things here act for the operator. The review mode types
# answers into `canon draft --resume`, so an answer that lands on the wrong
# candidate is a rule nobody ratified, written under their name. The lookup mode
# puts rules in front of every edit, so a rejected proposal shown as live, or a
# ratified rule shown as merely proposed, misleads every session.
#
# A stub prints canon's review prompt in the format of canon's draft.rs `review`
# and wrap.rs `hang_at`, at a narrow width so candidate text wraps, and answers
# `canon list --json` from a file. Review cases: a wrapped text with quotes and a
# `because` line, a reject, an edit, a text not in the verdicts, a source that
# differs from the judged one, an unratified group, a repeat, `--plan`, `--ids`,
# and an agent CANON_ACTOR refused. Lookup cases: a proposal matched by file
# name, one matched only through its edit text, a ratified rule labelled by its
# canon id, and three that must NOT show (declined in seen, a reject verdict, an
# unrelated file). With the source check removed, the mismatch case fails
# (watched 2026-09-13).
set -uo pipefail

ROOT="$(git rev-parse --show-toplevel)"
SCRIPT="$ROOT/scripts/canon-ratify.py"
[[ -f "$SCRIPT" ]] || { echo "cannot find $SCRIPT"; exit 2; }

T="$(mktemp -d)"
trap 'rm -rf "$T"' EXIT

SCRIPT="$SCRIPT" T="$T" python3 - <<'PY'
import hashlib, json, os, subprocess, sys

SCRIPT, d = os.environ["SCRIPT"], os.environ["T"]
STUB = r'''
import json, os, sys
if "list" in sys.argv[1:]:
    print(open(os.environ["STUB_LIST"]).read()); sys.exit(0)
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
open(os.path.join(d, "stub.py"), "w").write(STUB)
CANON = f"{sys.executable} {os.path.join(d, 'stub.py')}"
rc = 0


def check(ok, label):
    global rc
    print(("  ok    " if ok else "  FAIL  ") + label)
    rc |= 0 if ok else 1


def write_jsonl(path, rows):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.writelines(json.dumps(r) + "\n" for r in rows)


def canon_list(texts):
    with open(os.path.join(d, "list.json"), "w") as f:
        json.dump({"commitments": [{"id": f"can-{i:012x}", "text": t, "status": {"status": "active"},
                                    "source": "x"} for i, t in enumerate(texts, 0xabc)]}, f)


env = dict(os.environ, STUB_CANDS=os.path.join(d, "cands.jsonl"), STUB_RECORD=os.path.join(d, "record.jsonl"),
           STUB_LIST=os.path.join(d, "list.json"))
env.pop("CANON_ACTOR", None)
canon_list([])


def ratify(verdicts, extra, e=env, root=d):
    return subprocess.run([sys.executable, SCRIPT, "--root", root, "--verdicts", verdicts, "--run", "x",
                           "--canon", CANON, "--log", os.path.join(d, "log.jsonl")] + extra,
                          env=e, capture_output=True, text=True, timeout=30)


def answers():
    return [json.loads(l) for l in open(os.path.join(d, "record.jsonl"))]


# ---- review, by text
LONG = ("A signed, verified, shippable artifact must never live ONLY inside a directory "
        "a routine dev command is entitled to delete; stage it \"outside\" target/.")
write_jsonl(os.path.join(d, "cands.jsonl"), [
    {"text": LONG, "source": "invariant/a.md:3-9", "quote": "q1\nq1b", "because": "a reason that also wraps across the narrow width"},
    {"text": "Direct-inbound only.", "source": "attempt/b.md:3-3", "quote": "q2"},
    {"text": "Use  debug   builds.", "source": "memory/c.md:1-2", "quote": "q3"},
    {"text": "Not in the verdict file.", "source": "invariant/d.md:1-1", "quote": "q4"},
    {"text": "Same text, other source.", "source": "invariant/e.md:1-1", "quote": "q5"},
    {"text": "Rule in an unratified group.", "source": "invariant/f.md:1-1", "quote": "q6"},
    {"text": "Direct-inbound only.", "source": "attempt/b.md:3-3", "quote": "q2 again"},
])
V = os.path.join(d, "verdicts.jsonl")
write_jsonl(V, [
    {"id": "n00000001", "text": LONG, "source": "invariant/a.md:3-9", "verdict": "accept", "group": "g-accept", "edit_text": None},
    {"id": "n00000002", "text": "Direct-inbound only.", "source": "attempt/b.md:3-3", "verdict": "reject", "group": "g-reject", "edit_text": None},
    {"id": "n00000003", "text": "Use debug builds.", "source": "memory/c.md:1-2", "verdict": "edit", "group": "g-accept", "edit_text": "Use debug builds, not release."},
    {"id": "n00000005", "text": "Same text, other source.", "source": "invariant/zzz.md:1-1", "verdict": "accept", "group": "g-accept", "edit_text": None},
    {"id": "n00000006", "text": "Rule in an unratified group.", "source": "invariant/f.md:1-1", "verdict": "accept", "group": "g-later", "edit_text": None},
])
p = ratify(V, ["--groups", "g-accept,g-reject"])
got = answers()
check([g["answer"] for g in got] == ["a", "r", "e", "s", "s", "s", "s"],
      f"answers by text across a wrapped prompt: {[g['answer'] for g in got]}")
check(got[2]["edit"] == "Use debug builds, not release.", "edit sends the ratified text")
check("source mismatch" in p.stdout, "a source that differs from the judged one is skipped and reported")
log = [json.loads(l) for l in open(os.path.join(d, "log.jsonl"))]
check([l["act"] for l in log][:3] == ["can-000000000001", None, "can-000000000003"], "act ids logged against their answers")
ratify(V, ["--ids", "n00000006"])
check([g["answer"] for g in answers()] == ["s", "s", "s", "s", "s", "a", "s"], "--ids answers only the named proposal")
ratify(V, ["--plan"])
check({g["answer"] for g in answers()} == {"s"}, "--plan sends only skips")
p2 =subprocess.run([sys.executable, SCRIPT, "--root", d, "--verdicts", V, "--run", "x", "--plan"],
                    env=dict(env, CANON_ACTOR="agent:test"), capture_output=True, text=True)
check(p2.returncode != 0 and "agent" in p2.stderr, "an agent CANON_ACTOR is refused before canon's review runs")

# ---- lookup: what an edit is shown
R = os.path.join(d, "repo")
RV = os.path.join(R, ".canon", "adjudication", "ratify.jsonl")
declined_text = "`worker_gate.rs` retries forever."
write_jsonl(RV, [
    {"id": "nprop0001", "text": "Every `worker_gate.rs` change keeps request ids outside the pick.", "source": "invariant/p1.md:1-2", "verdict": "accept", "group": "accept-invariant-1", "edit_text": None, "reason": "r1", "runs": {"rerun": 1}},
    {"id": "nrat00002", "text": "`worker_gate.rs` must log every refusal.", "source": "invariant/p2.md:1-1", "verdict": "accept", "group": "accept-invariant-1", "edit_text": None, "reason": "r2", "runs": {"rerun": 2}},
    {"id": "nrej00003", "text": declined_text, "source": "invariant/p3.md:1-1", "verdict": "accept", "group": "accept-invariant-1", "edit_text": None, "reason": "r3", "runs": {"rerun": 3}},
    {"id": "nrjv00004", "text": "`worker_gate.rs` is fast.", "source": "invariant/p4.md:1-1", "verdict": "reject", "group": "reject-fact-1", "edit_text": None, "reason": "r4", "runs": {"rerun": 4}},
    {"id": "nunr00005", "text": "`other_file.rs` owns its retries.", "source": "invariant/p5.md:1-1", "verdict": "accept", "group": "accept-invariant-1", "edit_text": None, "reason": "r5", "runs": {"rerun": 5}},
    {"id": "nedt00006", "text": "Do not fold the ids.", "source": "invariant/p6.md:1-1", "verdict": "edit", "group": "edit", "edit_text": "Keep `SinglePeerSelection` ids outside its `Option<pick>`.", "reason": "r6", "runs": {"rerun": 6}},
])
with open(os.path.join(R, ".canon", "seen"), "w") as f:
    f.write(hashlib.sha256(declined_text.encode()).hexdigest()[:8] + " rejected\n")
canon_list(["`worker_gate.rs` must log every refusal."])


def lookup(file, edit):
    return subprocess.run([sys.executable, SCRIPT, "--lookup", "--root", R, "--canon", CANON, "--file", file,
                           "--session", "s1"], input=edit, env=env, capture_output=True, text=True, timeout=30)


def run_hits(extra=()):
    return subprocess.run([sys.executable, SCRIPT, "--hits", "--root", R, "--canon", CANON, *extra],
                          env=env, capture_output=True, text=True, timeout=30)


out = lookup("src/mesh/worker_gate.rs", "let pick = SinglePeerSelection::new();").stdout
check("[proposed nprop0001]" in out, "a proposal naming the edited file is shown as proposed")
check("[proposed nedt00006]" in out, "an edit proposal matches through its corrected text")
check("[can-000000000abc]" in out and "nrat00002" not in out, "a ratified rule is shown by its canon id, not as a proposal")
check("nrej00003" not in out and "retries forever" not in out, "a proposal the operator declined in seen is not shown")
check("nrjv00004" not in out and "nunr00005" not in out, "a reject verdict and an unrelated file are not shown")
hit_ids = {json.loads(l)["id"] for l in open(os.path.join(R, ".canon", "adjudication", "hits.jsonl"))}
check(hit_ids == {"nprop0001", "nedt00006"}, f"only shown proposals are logged as hits: {sorted(hit_ids)}")
check(lookup("src/unrelated.rs", "fn nothing_here() {}").stdout == "", "an edit that touches no rule prints nothing")

h = run_hits().stdout
check("--run 1789349217 --ids nprop0001,nedt00006" in h or "--run 1789349217 --ids nedt00006,nprop0001" in h,
      "--hits prints the one command that ratifies the hit proposals")
check("2 proposed" in run_hits(["--brief"]).stdout, "--hits --brief gives the boot line")
canon_list(["`worker_gate.rs` must log every refusal.", "Every `worker_gate.rs` change keeps request ids outside the pick."])
h = run_hits().stdout
check("nprop0001" not in h and "nedt00006" in h, "a hit proposal drops off --hits once canon holds it")
sys.exit(rc)
PY
