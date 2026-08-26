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

PROPOSAL — nc-3-backstage, 2026-08-20. NOT the registered instrument.
`scripts/nc-boundary.py` is seat-owned for this wave; this file exists so the
change could be MEASURED without re-baselining three bars under two peers
mid-flight. The seat lands it and re-baselines.

TWO CHANGES, both re-classification. Neither claims a dependency went away.
  1. the domain() asymmetry — see the comment on domain() below. `load()`
     passed the crate; the ref loop passed nothing, so BACKSTAGE_CRATES applied
     to a type's OWNER and never to its REFERENCE SITE. Back-of-house crates
     referencing their own types scored as violations.
  2. membership — see BACKSTAGE_CRATES below. +sovereign-atos,
     -commonwealth-tdd, -sovereign-authoring-harness, each adjudicated against
     "does the product ship without it?" with the evidence recorded inline.

WHAT LANDING THIS COSTS — it moves bars this rung does NOT own. Measured on
one graph (mtime 2026-08-20 01:16), same tree, all four variants:

  variant                       core_w  violating  BACKSTAGE  BACKFLOW  kernel
  registered baseline              485        337        255        82      23
    +asymmetry fix only            478        187        105        82      22
    +membership only               480        315        238        77      23
  BOTH (this file)                 480        162         85        77      22

So: core_boundary_width -5, BACKFLOW -5, kernel_size -1 as SIDE EFFECTS. The
kernel drop is `FeatureStore`, the one back-of-house-owned kernel type — with
the asymmetry fixed it is spoken by two core domains plus back-of-house rather
than three, so it is no longer kernel. The two changes are NOT additive; take
the bottom row, not the sum.

KNOWN BLIND SPOTS THIS DOES NOT FIX, both worth knowing before trusting a
reading. Neither is introduced here; both are in the registered instrument.
  - TYPES ONLY. Both the symbol load and the ref count filter
    `qualified_name LIKE '%#'`, so a dependency carried by free functions,
    consts or macros is invisible. `cargo xtask layer-gate` reads CRATE edges
    from manifests and sees those; the two instruments are complements, not
    duplicates. `sovereign-cli-llm -> sovereign-eval` happens to be visible to
    both; a free-function-only edge would be visible to layer-gate alone.
  - NOT_PRODUCTION still excludes by bare substring, so "research/" also
    matches "deep_research/". Filed; widths are byte-identical either way.

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
# v2 MEMBERSHIP (nc-3-backstage, 2026-08-20). The test is the campaign's:
# does the product ship without it? Answered per crate, with the evidence.
#
#   ADDED   sovereign-atos          the ATOS orchestrator over corpus-engine-atos
#                                   — the same subsystem, and the list already
#                                   held the store half. Omitting it split one
#                                   decider across two answers.
#   REMOVED commonwealth-tdd        NOT back-of-house. It backs two SHIPPED MCP
#                                   tools, `tdd_solve` and `tdd_bdd_cycle`,
#                                   listed in tools/list (sovereign-server
#                                   routes_mcp.rs:469), plus the `solve` verb's
#                                   daemon route (daemon_cmd/solve_http.rs). A
#                                   quality control measures SOVEREIGN; this
#                                   drives red-green-refactor over the USER's
#                                   repo and emits their code. Caught as a false
#                                   positive by `cargo xtask layer-gate` on its
#                                   first real run.
#   REMOVED sovereign-authoring-harness
#                                   NOT back-of-house. Carved out of
#                                   sovereign-eval precisely so sovereign-desktop
#                                   could consume it without rusqlite/reqwest/
#                                   clap in the Tauri build; the desktop's
#                                   recipe-testing panel is a shipped feature.
#                                   The carve-out's own purpose is the evidence.
#
# These MOVE THE BAR by re-classification, not by deletion. See the rung's
# A/B/C split — nothing here is a claim that a dependency went away.
BACKSTAGE_CRATES = {
    "sovereign-eval", "sovereign-cli-dev", "sovereign-agent-bench",
    "sovereign-atos", "xtask",
    "corpus-engine-atos", "corpus-engine-archaeology",
}
BACKSTAGE_PATHS = ("/bench/", "/xtask/", "gym/",
                   "sovereign/crates/sovereign-eval/",
                   "sovereign/crates/sovereign-cli-dev/")


def crate_of(path):
    """Owning crate directory for a repo-relative path.

    `<ws>/crates/<name>/…` for the workspace crates, else the first path
    component (`corpus-engine-atos/src/…` and friends sit at the repo root).
    """
    parts = path.split("/")
    for i, p in enumerate(parts):
        if p == "crates" and i + 1 < len(parts):
            return parts[i + 1]
    return parts[0] if parts else ""


def domain(path, crate=None):
    if any(b in path for b in NOT_PRODUCTION):
        return None
    # v2 FIX (nc-3-backstage). `load()` passes the crate from the qualified
    # name; the ref loop only has a file path and used to pass NOTHING, so
    # BACKSTAGE_CRATES applied to the type's OWNER and never to the REFERENCE
    # SITE. A back-of-house crate referencing its own types therefore scored as
    # `sovereign -> back-of-house`, which the instrument's own rule calls legal
    # (`if src == BACKSTAGE: return None  # observing anything is legal`).
    # 184 of the 255 BACKSTAGE violations were exactly that self-reference:
    # sovereign-agent-bench 92, commonwealth-tdd 40, corpus-engine-archaeology
    # 25, corpus-engine-atos 19, sovereign-authoring-harness 8.
    if crate is None:
        crate = crate_of(path)
    if (crate in BACKSTAGE_CRATES) or any(p in path for p in BACKSTAGE_PATHS):
        return "back-of-house"
    if path.startswith("sovereign/"):
        return "sovereign"
    if path.startswith("corpus-engine"):
        return "corpus-engine"
    if path.startswith("commonwealth/"):
        return "commonwealth"
    if path.startswith("kernel-types/"):
        # Layer-0 identity + provenance, owned by NO product domain. Registered
        # by the seat 2026-08-20 for rung nc-1-kernel. Without this branch the
        # kernel is unmeasurable in either direction: under `sovereign/` it
        # classifies as a PRODUCT domain (the exact failure the rung's done-when
        # names), and at any new top-level path it falls through to None, which
        # `load()` DROPS — so kernel_size would fall as types moved into it and
        # read as progress while actually meaning "stopped being measured"
        # (ARCH §18.3). The branch matches nothing on the tree at the time it was
        # added, so every prior reading is byte-identical under both versions.
        return "kernel"
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
LAYER = {"oicp": 0, "kernel": 0, "corpus-engine": 1, "commonwealth": 1, "sovereign": 2, "studio": 3}
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


def index_provenance(db):
    """Which tree do these numbers actually describe?

    THIS TOOL DOES NOT READ THE WORKING TREE. It queries the daemon's SCIP
    index, so its numbers describe `last_indexed_head` — NOT your HEAD, and not
    your uncommitted work. Three consequences, all found the hard way by
    nc-10's worker on 2026-08-20:

      - a `git worktree` does NOT isolate this measurement; every session on
        the machine reads the same index;
      - the numbers MOVE when the daemon re-indexes a peer's commits, so a
        delta across two runs is not necessarily attributable to your change;
      - the index lags. Measured at the moment this was written: index at
        `f4a85bad`, HEAD at `10aa9a28` — FIVE commits behind.

    Emitting the indexed head is what lets `co-lineage` stamp a measurement row
    with the commit the number DESCRIBES rather than the commit that happened to
    be checked out when the row was written. Before this, the row said `ref=head`
    and meant something else — a success-shaped stamp over an unverified claim
    (ARCH §18.3).
    """
    meta = dict(db.execute("SELECT key, value FROM scip_meta"))
    return meta.get("last_indexed_head"), meta.get("last_export_at")


def main():
    if not os.path.exists(GRAPH):
        sys.exit(f"nc-boundary: no graph at {GRAPH} — svrn refresh")
    db = sqlite3.connect(f"file:{GRAPH}?mode=ro", uri=True)
    indexed_head, indexed_at = index_provenance(db)
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
            # The tree these numbers describe — see index_provenance(). A
            # consumer that stamps a row with anything else is guessing.
            "indexed_head": indexed_head,
            "indexed_at": indexed_at,
            "edges": edges}, indent=2))
        return 0

    print("DOMAIN BOUNDARY WIDTH — distinct types crossing each edge\n")
    print(f"  index describes {(indexed_head or '(unknown)')[:12]} "
          f"exported {indexed_at or '(unknown)'} — NOT your working tree\n")
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
