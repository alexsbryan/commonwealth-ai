#!/usr/bin/env python3
"""Render the test-writing burndown. A VIEW — it stores nothing.

quality/TEST_LEDGER.md is the model. This joins its two sources into one
ordered queue and prints it:

  quality/tests/backlog.toml       102 notes-derived records   kind=write
                                     (a record carrying `landed` is excluded:
                                      its mutation was watched red on the test
                                      that field names — see the file header)
  quality/conformance-specs.toml    75 exists-untagged clauses kind=tag
                                     4 landed-but-unclaimable  kind=blocked
                                     1 spec/code conflict      kind=decide
  quality/conformance/*.toml        claims already minted (excluded)

THE UNIT OF WORK IS A FILE, NOT AN ITEM. Interlock 5 forbids two agents in one
file, so a file is what an order can claim; and 18 of the 102 notes records
share a file with an unsettled clause, so taking the file closes both at once.

ORDER: (1) lowest tier in the file, (2) most items in the file, (3) tag-heavy
before write-heavy — a tag is a covers: line plus its mutation, a write is a
test. Ties by id, so the queue is stable across runs.

Nothing here is a coverage verdict. An item is OPEN because nobody has run its
mutation, not because anyone judged it uncovered.

  scripts/test-queue.py                 top 25 files
  scripts/test-queue.py --all           every file
  scripts/test-queue.py --kind tag      one kind
  scripts/test-queue.py --tier 1        one tier
  scripts/test-queue.py --counts        the arithmetic only
"""
import argparse, collections, glob, re, tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TIER3_FAMILIES = {"CI"}          # code intelligence is dev tooling; see the worklist
NO_ROUTE = {"UI-9", "UI-10", "EV-25", "EV-33"}   # tests exist, no route to a claim


def one_line(s, n=92):
    """Collapse a triple-quoted field to one printable line."""
    s = re.sub(r"\s+", " ", (s or "")).strip()
    return s[:n] + ("…" if len(s) > n else "")


def norm(target):
    """A comparable path: no ::symbol, no :line, no crates/ prefix noise."""
    t = (target or "").split("::")[0].strip()
    t = re.sub(r":\d+$", "", t)
    return t.replace("sovereign/crates/", "").replace("commonwealth/crates/", "")


def same(a, b):
    return bool(a) and bool(b) and (a == b or a.endswith("/" + b) or b.endswith("/" + a))


def load():
    items = []
    for r in tomllib.load(open(ROOT / "quality/tests/backlog.toml", "rb"))["test"]:
        if r.get("landed"):
            continue                        # mutation watched red: not queue work
        items.append(dict(id=r["id"], kind="write", tier=r["tier"], path=norm(r["target"]),
                          why=one_line(r["failure"].split(". ")[0]), witness="note " + r["note"]))
    d = tomllib.load(open(ROOT / "quality/conformance-specs.toml", "rb"))
    for s in d["spec"]:
        if s["status"] == "exists-untagged":
            kind = "tag"
        elif s["id"] in NO_ROUTE:
            kind = "blocked"
        else:
            continue                        # landed AND claimed: not queue work
        items.append(dict(id=s["id"], kind=kind,
                          tier=3 if s["family"] in TIER3_FAMILIES else 1,
                          path=norm(s.get("target")), why=one_line(s["clause"].split(". ")[0]),
                          witness="clause " + s["id"]))
    items.append(dict(id=d["id"], kind="decide", tier=1, path=norm(d.get("target")),
                      why=one_line(d["clause"].split(". ")[0]), witness="clause " + d["id"]))
    return items


def cluster(items):
    """Group by file, merging paths that are the same file written two ways."""
    files, keys = collections.defaultdict(list), []
    for it in items:
        hit = next((k for k in keys if same(k, it["path"])), None)
        if hit is None:
            keys.append(it["path"]); hit = it["path"]
        files[hit].append(it)
    order = collections.Counter(i["kind"] for i in items)
    ranked = sorted(files.items(), key=lambda kv: (
        min(i["tier"] for i in kv[1]),
        -len(kv[1]),
        -sum(i["kind"] == "tag" for i in kv[1]),
        kv[0]))
    return ranked, order


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--kind"); ap.add_argument("--tier", type=int)
    ap.add_argument("--counts", action="store_true")
    a = ap.parse_args()
    items = load()
    if a.kind: items = [i for i in items if i["kind"] == a.kind]
    if a.tier: items = [i for i in items if i["tier"] == a.tier]
    ranked, kinds = cluster(items)

    print(f"OPEN {len(items)} items in {len(ranked)} files "
          f"— {dict(kinds.most_common())}")
    req = tomllib.load(open(ROOT / "quality/requirements.toml", "rb"))["requirements"]
    enf = tomllib.load(open(ROOT / "quality/requirements-enforceability.toml", "rb"))
    ids = {x["id"] for x in req if not x.get("alias_of") and x["level"] != "out-of-scope"}
    claimed = {c["requirement"] for f in glob.glob(str(ROOT / "quality/conformance/*.toml"))
               for c in tomllib.load(open(f, "rb")).get("claim", [])}
    spec = tomllib.load(open(ROOT / "quality/conformance-specs.toml", "rb"))
    surveyed = {s["id"] for s in spec["spec"]} | {spec["id"]}
    rest = ids - claimed - surveyed
    print(f"NOT YET QUEUE WORK: {len(rest)} requirements never surveyed "
          f"({sum(1 for i in rest if enf.get(i) in ('cli','desktop','structural'))} mechanically "
          f"settleable, {sum(1 for i in rest if enf.get(i)=='review')} review-only). "
          f"A survey turns each into a tag or a write.")
    if a.counts:
        return
    print()
    for path, its in (ranked if a.all else ranked[:25]):
        tags = collections.Counter(i["kind"] for i in its)
        print(f"── {path or '(no target)'}   tier {min(i['tier'] for i in its)}   "
              f"{len(its)} items  {dict(tags)}")
        for i in sorted(its, key=lambda x: (x["kind"] != "tag", x["id"])):
            print(f"     {i['kind']:8} {i['id']:7} {i['witness']:14} {i['why']}")
    if not a.all and len(ranked) > 25:
        print(f"\n… {len(ranked)-25} more files. --all for every one.")


if __name__ == "__main__":
    main()
