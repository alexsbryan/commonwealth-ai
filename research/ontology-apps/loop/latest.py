"""latest.py <corpus> <arm>...: re-append to the corpus's answer store the lines each arm recorded (its run.json
answers_before..answers_after), in the order given, so the arms' answers are the latest: a replay, and a `recorded`
asker, give the latest recording of a question (corpus-engine `enrichment/asker.rs`), so after a second read of one
cut, or a fresh RESOLVE pass, an earlier read replays only this way. Append-only: nothing recorded is removed."""
import json, os, sys

cid, arms = sys.argv[1], sys.argv[2:]
store = os.path.expanduser(f"~/.svrnmesh/enrichment/{cid}/answers.jsonl")
lines = open(store).read().splitlines()
add = []
for arm in arms:
    r = json.load(open(f"runs/loop/{arm}/{cid}/run.json"))
    lo, hi = int(r["answers_before"]), int(r["answers_after"])
    if not 0 <= lo <= hi <= len(lines):
        sys.exit(f"latest.py: {arm}/{cid} recorded lines {lo}..{hi}, the store holds {len(lines)}")
    add += lines[lo:hi]
    print(f"{arm}/{cid}: lines {lo}..{hi} ({hi - lo})")
with open(store, "a") as f:
    f.writelines(line + "\n" for line in add)
print(f"{store}: {len(lines)} -> {len(lines) + len(add)}")
