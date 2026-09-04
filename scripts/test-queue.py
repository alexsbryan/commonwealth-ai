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
  scripts/test-queue.py --next          ONE file's self-contained order

--next is the burndown loop's unit of work: it renders the top-ranked file's
records WHOLE — failure, observable, mutation, and for a tag the existing test —
so an iteration is one command and one production read, never a note lookup.
`--avoid <fragment,...>` skips files a peer session holds; `--skip N` takes the
next-ranked one when the top is blocked. scripts/test-close.py is the only
write path back.
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
        items.append(dict(id=r["id"], kind="blocked" if r.get("blocked") else "write",
                          tier=r["tier"], path=norm(r["target"]),
                          why=one_line(r["failure"].split(". ")[0]), witness="note " + r["note"],
                          raw=r))
    d = tomllib.load(open(ROOT / "quality/conformance-specs.toml", "rb"))
    for s in d["spec"]:
        if s["status"] == "exists-untagged":
            kind = "tag"
        elif s["status"] == "subject-deleted":
            # The code this clause claimed was DELETED — usually because it had
            # no callers, which means the clause never had a live
            # implementation. It is write-work again. Falling through to the
            # `continue` below would make a lost proof indistinguishable from a
            # kept one (ARCH §18.3).
            kind = "write"
        elif s["id"] in NO_ROUTE:
            kind = "blocked"
        else:
            continue                        # landed AND claimed: not queue work
        items.append(dict(id=s["id"], kind=kind,
                          tier=3 if s["family"] in TIER3_FAMILIES else 1,
                          path=norm(s.get("target")), why=one_line(s["clause"].split(". ")[0]),
                          witness="clause " + s["id"], raw=s))
    # The top-level spec/code conflict. Appended unconditionally until
    # 2026-09-03, which meant a RESOLVED conflict still read as open — the
    # view could not record the thing it exists to track closing.
    if d["status"] != "landed":
        items.append(dict(id=d["id"], kind="decide", tier=1, path=norm(d.get("target")),
                          why=one_line(d["clause"].split(". ")[0]),
                          witness="clause " + d["id"], raw=d))
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


def wrap(label, body, w=76):
    """A field, indented under its label, wrapped — never truncated."""
    body = re.sub(r"\s+", " ", (body or "")).strip()
    if not body:
        return
    out, line = [], ""
    for word in body.split(" "):
        if len(line) + len(word) + 1 > w:
            out.append(line); line = word
        else:
            line = (line + " " + word).strip()
    out.append(line)
    print(f"  {label:<12}{out[0]}")
    for extra in out[1:]:
        print(f"  {'':<12}{extra}")


def order(path, items):
    """ONE file's whole brief, self-contained. TEST_LEDGER.md: an agent picking
    up a record must never need to open the witness note."""
    print(f"ORDER   {path}")
    print(f"        {len(items)} item(s): " + ", ".join(
        f"{i['id']}({i['kind']})" for i in sorted(items, key=lambda x: x["id"])))
    print("        interlock 5: one agent per file. Take all of them or none.")
    for i in sorted(items, key=lambda x: (x["kind"] != "tag", x["id"])):
        r = i["raw"]
        print(f"\n── {i['id']}   {i['kind']}   {i['witness']}")
        if i["kind"] == "blocked" and r.get("blocked"):
            wrap("blocked", r["blocked"])
            continue
        if i["kind"] == "write":
            wrap("class", f"{r.get('class')}   surface {r.get('surface')}   found {r.get('found')}")
            wrap("target", r.get("target"))
            wrap("failure", r.get("failure"))
            wrap("observable", r.get("observable"))
            wrap("mutation", r.get("mutation"))
        else:
            wrap("clause", r.get("clause"))
            wrap("target", r.get("target"))
            wrap("existing", r.get("existing") or "(none named — this is a write, not a tag)")
            wrap("asserts", r.get("asserts"))
            wrap("catches", r.get("catches"))
    print("\nCLOSE   watch the mutation redden the test THIS record names, then:")
    print("        scripts/test-close.py " + " ".join(
        f"{i['id']}=<junit-key>" for i in sorted(items, key=lambda x: x["id"])))
    print("        A mutation that reddens a test the record does not name is")
    print("        could-not-judge, never covered (TEST_LEDGER.md §Closure).")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--kind"); ap.add_argument("--tier", type=int)
    ap.add_argument("--counts", action="store_true")
    ap.add_argument("--next", action="store_true",
                    help="print ONE file's self-contained order — the loop's unit of work")
    ap.add_argument("--skip", type=int, default=0,
                    help="with --next: take the Nth-ranked file instead of the top one")
    ap.add_argument("--avoid", default="",
                    help="with --next: comma-separated path fragments to skip "
                         "(a peer holds the file, or it is blocked)")
    a = ap.parse_args()
    items = load()
    if a.kind: items = [i for i in items if i["kind"] == a.kind]
    if a.tier: items = [i for i in items if i["tier"] == a.tier]
    ranked, kinds = cluster(items)

    if a.next:
        avoid = [x.strip() for x in a.avoid.split(",") if x.strip()]
        live = [(p, its) for p, its in ranked
                if not any(x in (p or "") for x in avoid)
                and any(i["kind"] in ("write", "tag") for i in its)]
        if not live:
            print("QUEUE EMPTY — nothing left that is not avoided.")
            return
        if a.skip >= len(live):
            print(f"ONLY {len(live)} file(s) available; --skip {a.skip} is past the end.")
            return
        path, its = live[a.skip]
        print(f"OPEN {len(items)} items in {len(ranked)} files "
              f"— {dict(kinds.most_common())}\n")
        order(path, its)
        return

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
