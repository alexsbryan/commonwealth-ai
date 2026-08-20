#!/usr/bin/env python3
"""nc-boundary — how wide is each domain boundary, and what crosses it.

The system is three systems: sovereign (the turn), corpus-engine (knowledge),
commonwealth (federation). A domain boundary is only a boundary if a small,
named set of types crosses it. This measures the real width.

  BOUNDARY WIDTH   distinct types referenced across a domain edge
  BACKFLOW         edges that run the wrong way down the layer map
  KERNEL           types all three domains speak — the shared contract, if
                   it is a contract at all

Measured 2026-08-19 before any of this campaign landed:
  sovereign    -> corpus-engine   5588 refs over 372 distinct types
  sovereign    -> commonwealth     765 refs over  93 distinct types
  commonwealth -> corpus-engine    192 refs over  31 distinct types
  BACKFLOW (lower layer naming a higher one): 82 types over 4 edges
  kernel: 23 types, ALL owned by corpus-engine, including CorpusEngine (a
  102-method god object) and its Error/Result. The three systems do not talk
  through a contract; they talk through corpus-engine's internals.

  scripts/nc-boundary.py           # the table
  scripts/nc-boundary.py --json    # for a bar instrument
  scripts/nc-boundary.py --edge sovereign:corpus-engine   # what crosses it
"""
import collections, json, os, sqlite3, sys

GRAPH = os.path.expanduser("~/.svrnmesh/indexes/commonwealth-ai/scip_graph.db")
PREFIX = "rust-analyzer cargo "
NOT_PRODUCTION = ("/tests/", "/benches/", "/examples/", "research/",
                  "vendor/", "/target/", ".claude/")

# The domain map. Path-based because that IS the workspace boundary — three
# Cargo workspaces, three release cadences.
# Back-of-house: the quality controls. Named by CRATE because that is what a
# layer gate can enforce — and the finding that made this a rung is that today
# it is NOT a crate boundary: sovereign-cli-llm holds bench_cmd and gym_judge
# next to chat and corpus in one crate, so there is nothing to enforce against.
BACKSTAGE_CRATES = {
    "sovereign-eval", "sovereign-cli-dev", "sovereign-agent-bench",
    "sovereign-authoring-harness", "commonwealth-tdd", "xtask",
    "corpus-engine-atos", "corpus-engine-archaeology",
}
BACKSTAGE_PATHS = ("/bench/", "/xtask/", "gym/",
                   "sovereign/crates/sovereign-eval/",
                   "sovereign/crates/sovereign-cli-dev/")


def domain(path, crate=None):
    if any(b in path for b in NOT_PRODUCTION):
        return None
    if (crate in BACKSTAGE_CRATES) or any(p in path for p in BACKSTAGE_PATHS):
        return "back-of-house"
    if path.startswith("sovereign/"):
        return "sovereign"
    if path.startswith("corpus-engine"):
        return "corpus-engine"
    if path.startswith("commonwealth/"):
        return "commonwealth"
    if path.startswith(("oicp-types/", "oicp-client/")):
        return "oicp"          # the intended shared membrane
    if path.startswith("studio/"):
        return "studio"        # a FOURTH system, entangled both ways
    return None

CORE = ("sovereign", "corpus-engine", "commonwealth")

# THE INTENDED SHAPE (operator, 2026-08-19): "sovereign ended up kind of being
# the interface layer that presents and packages commonwealth + corpus engine
# capabilities." So sovereign sits ABOVE both, and the traffic agrees:
# sovereign -> commonwealth is 765 refs (deliberate composition) while
# commonwealth -> sovereign is 122 (leakage).
#
# AGENTS.md's "commonwealth != sovereign, they are peer projects, not
# parent/child" is a GOVERNANCE claim — separate repos, separate release
# cadence — not a layering one. Reading it as layering was an error in the
# first cut of this instrument.
#
#   3  studio          the fourth system, out of scope, named not hidden
#   2  sovereign       presents and packages the two below
#   1  corpus-engine   knowledge      commonwealth  federation
#   0  oicp            the wire contract between them
#
# One violation class: an edge from a lower layer to a higher one. A lower
# layer that names a higher one's type cannot be released, reasoned about, or
# reused independently — which is the whole point of the boundary.
LAYER = {"oicp": 0, "corpus-engine": 1, "commonwealth": 1, "sovereign": 2, "studio": 3}
# Back-of-house sits outside the stack, not on top of it: it may observe every
# layer, and NOTHING may depend on it. That one-way rule is what separates a
# quality control from a product feature — a bench you cannot ship without is
# not a bench.
BACKSTAGE = "back-of-house"


def violation(src, dst):
    """Why this edge should not exist, or None."""
    if dst == BACKSTAGE and src != BACKSTAGE:
        return "BACKSTAGE"          # product depending on its own instrument
    if src == BACKSTAGE:
        return None                 # observing anything is legal
    if LAYER.get(src, 9) < LAYER.get(dst, 9):
        return "BACKFLOW"
    return None


def load(db):
    """{qualified_name: (bare_name, owning_domain)} for production types."""
    owner = {}
    for qn, fp in db.execute(
            "SELECT qualified_name, file_path FROM symbols WHERE qualified_name LIKE '%#'"):
        if not qn.startswith(PREFIX):
            continue
        rest = qn[len(PREFIX):]
        d = domain(fp, rest[:rest.index(" ")] if " " in rest else rest)
        if not d:
            continue
        desc = qn.split(" ", 4)[-1]
        if desc.count("#") != 1 or "/tests/" in desc:
            continue
        owner[qn] = (desc.rsplit("/", 1)[-1].rstrip("#"), d)
    return owner


def main():
    if not os.path.exists(GRAPH):
        sys.exit(f"nc-boundary: no graph at {GRAPH} — svrn refresh")
    db = sqlite3.connect(f"file:{GRAPH}?mode=ro", uri=True)
    owner = load(db)

    flow = collections.defaultdict(collections.Counter)   # (src,dst) -> types
    users = collections.defaultdict(set)                  # type -> domains using it
    for cq, fp in db.execute(
            "SELECT callee_qualified, file_path FROM refs WHERE callee_qualified LIKE '%#'"):
        if cq not in owner:
            continue
        src = domain(fp)
        if not src:
            continue
        name, dst = owner[cq]
        users[cq].add(src)
        if src != dst:
            flow[(src, dst)][name] += 1

    kernel = sorted({owner[q][0] for q, ds in users.items()
                     if len(ds & set(CORE)) >= 3})
    kernel_homes = collections.Counter(
        owner[q][1] for q, ds in users.items() if len(ds & set(CORE)) >= 3)

    edges = []
    for (s, d), c in flow.items():
        edges.append({"from": s, "to": d, "refs": sum(c.values()),
                      "width": len(c),
                      "violation": violation(s, d),
                      "top": [n for n, _ in c.most_common(10)]})
    edges.sort(key=lambda e: -e["width"])

    if "--edge" in sys.argv:
        s, d = sys.argv[sys.argv.index("--edge") + 1].split(":")
        c = flow[(s, d)]
        print(f"{s} -> {d}: {len(c)} distinct types, {sum(c.values())} refs\n")
        for n, k in c.most_common():
            print(f"  {k:>5}  {n}")
        return 0

    if "--json" in sys.argv:
        core = [e for e in edges if e["from"] in CORE and e["to"] in CORE]
        print(json.dumps({
            "core_boundary_width": sum(e["width"] for e in core),
            "violating_types": sum(e["width"] for e in edges if e["violation"]),
            "kernel_size": len(kernel),
            "kernel_homes": dict(kernel_homes),
            "edges": edges}, indent=2))
        return 0

    print("DOMAIN BOUNDARY WIDTH — distinct types crossing each edge\n")
    print(f"{'from':>14} -> {'to':<14}{'refs':>7}{'width':>7}   flag")
    print("-" * 62)
    for e in edges:
        if e["refs"] < 20:
            continue
        flag = e["violation"] or ""
        print(f"{e['from']:>14} -> {e['to']:<14}{e['refs']:>7}{e['width']:>7}   {flag}")
    core = [e for e in edges if e["from"] in CORE and e["to"] in CORE]
    print("-" * 62)
    print(f"core boundary width (the three systems) : "
          f"{sum(e['width'] for e in core)}")
    v = collections.Counter()
    for e in edges:
        if e["violation"]:
            v[e["violation"]] += e["width"]
    print(f"types on edges that should not exist    : {sum(v.values())}"
          f"   {dict(v)}")
    print()
    print(f"SHARED KERNEL — types all three systems speak: {len(kernel)}")
    print(f"  owned by: {dict(kernel_homes)}")
    print(f"  {', '.join(kernel)}")
    if set(kernel_homes) == {"corpus-engine"}:
        print("\n  ^ every kernel type is owned by ONE domain. That is not a")
        print("    contract, it is a dependency on an implementation.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
