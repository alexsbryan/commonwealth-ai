#!/usr/bin/env python3
"""Daemon route census — every `.route("<path>", ...)` the serving hosts register,
sorted into the four things the daemon is (quality/DAEMON_CORE.md §1) plus the
two it is not.

  turn      needs the loaded weights / a Runtime           stays, one host
  job       run a thing and report on it                   -> /v1/jobs over commonwealth-work
  node      identity, membership, liveness                 stays; one principal->scope table
  resource  read of a thing only the daemon may own        stays; one read family per resource
  store     agent-state CRUD (non-exclusive capability)    owned by sv-surface, recorded here only
  out       not the daemon's (app proxy, dev shims, test)  leaves

Classification is by path prefix, longest rule first. A path no rule names is
printed under UNCLASSIFIED — that list is the honest residue, not an error.

    python3 scripts/daemon-route-census.py            # summary
    python3 scripts/daemon-route-census.py --paths    # every path with its class
    python3 scripts/daemon-route-census.py --dupes    # paths registered by >1 crate
"""
import collections, os, re, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOSTS = [
    "sovereign/crates/sovereign-mesh/src",
    "commonwealth/crates/commonwealth-api/src",
    "sovereign/crates/sovereign-cli-daemon/src",
    "sovereign/crates/sovereign-server/src",
    "sovereign/crates/sovereign-workflow-host/src",
]
ROUTE = re.compile(r'\.route\(\s*"([^"]+)"\s*,\s*((?:[a-z]+\([^()]*(?:\([^()]*\))?[^()]*\)\s*\.?\s*)+)', re.S)

# (prefix-or-regex, class). First match wins; order matters.
RULES = [
    # ── turn ──
    (r"^/v1/(chat/completions|completions|responses|embeddings|search|models)$", "turn"),
    (r"^/api/", "turn"),
    (r"^/v1/conversations/\{id\}/(messages|stream|provenance|end)$", "turn"),
    (r"^/v1/(knowledge/search|solve|cycle/bdd|edit_predictions)", "turn"),
    (r"^/internal/(knowledge/search|inference/warmup|rpc-warm)$", "turn"),
    (r"^/internal/corpus/local/\{corpus_id\}/(search|preview)$", "turn"),
    (r"^/v1/documents/\{id\}/ask$", "turn"),
    (r"^/oicp/v1/capabilities$", "turn"),
    # ── job ──
    (r"^/v1/solve/jobs", "job"),
    (r"^/internal/corpus/local/\{corpus_id\}/(ingest|ingest/progress|cancel|clean|rollback|write-tags)$", "job"),
    (r"^/internal/corpus/local/incomplete-jobs$", "job"),
    (r"^/internal/corpus/(enrich-once|enrich-reset|cancel|pause|progress|expand|ingest_partition|next_unit|complete_unit|heartbeat|install|partition_evict|collaborate)", "job"),
    (r"^/internal/corpus/\{corpus\}/retry-enrichment$", "job"),
    (r"^/internal/corpus/watch/", "job"),
    (r"^/internal/(atlas/status|enrichment/status|newsworthy/status)$", "job"),
    (r"^/internal/governance/\{corpus\}/(seed|post-build-seed|recipe)$", "job"),
    (r"^/v1/projects/\{corpus_id\}/rebuild$", "job"),
    (r"^/v1/documents/(\{id\}/skeleton|legacy/promote|upload)$", "job"),
    (r"^/v1/corpora/upload$", "job"),
    (r"^/internal/worker/", "job"),
    (r"^/internal/workflows", "job"),
    (r"^/internal/(model|index)/transfer$", "job"),
    (r"^/internal/models/(load|unload|inventory)$", "job"),
    (r"^/internal/(scheduling|ingest/budget|storage/budget|contribution)", "job"),
    (r"^/v1/apps/\{app_id\}/(install|status)$", "job"),
    (r"^/oicp/v1/(corpus/install|corpus/progress|recipe/test)$", "job"),
    # ── node ──
    (r"^/v1/mesh/", "node"),
    (r"^/internal/(join|gossip|ring/sync|guest/grant|corpus/grant|node/activity|mesh/quiesce|daemon/foreground_state|activity|latency/probe)", "node"),
    (r"^/v1/conversations/\{id\}/enabled-corpora$", "node"),
    (r"^/v1/rail/", "node"),
    (r"^/(status|health|hello)$", "node"),
    (r"^/v1/ready$", "node"),
    (r"^/v1/admin/reload$", "node"),
    # ── resource ──
    (r"^/internal/corpus/\{corpus\}/", "resource"),
    (r"^/internal/corpus/(catalog|notebooks|diagnose|status|canonical)", "resource"),
    (r"^/internal/corpus/local(/\{corpus_id\}(/git|/snapshots)?|/ocr-available)?$", "resource"),
    (r"^/internal/atlas/", "resource"),
    (r"^/internal/meshapp/", "resource"),
    (r"^/internal/governance/\{corpus\}/(view|tensions)", "resource"),
    (r"^/internal/v1/models/", "resource"),
    (r"^/internal/index/serve$", "resource"),
    (r"^/blob$", "resource"),
    (r"^/mcp", "resource"),
    (r"^/v1/corpora", "resource"),
    (r"^/v1/knowledge/landscape_digest$", "resource"),
    (r"^/v1/documents/\{id\}/state$", "resource"),
    # ── store ──
    (r"^/v1/(conversations|notes|memories|insights|projects|recipe-projects|features|skills|mcp/servers|documents|tasks|tools)", "store"),
    # ── out ──
    (r"^/v1/apps", "out"),
    (r"^/app/", "out"),
    (r"^/chat$", "out"),
]
RULES = [(re.compile(p), c) for p, c in RULES]

def classify(path):
    for rx, c in RULES:
        if rx.search(path):
            return c
    return "UNCLASSIFIED"

def main():
    rows = []
    for h in HOSTS:
        for dp, _, fs in os.walk(os.path.join(ROOT, h)):
            for f in fs:
                if not f.endswith(".rs") or "test" in f or "example" in dp:
                    continue
                p = os.path.join(dp, f)
                s = open(p).read()
                for m in ROUTE.finditer(s):
                    crate = h.split("/")[2]
                    rows.append((m.group(1), crate, os.path.relpath(p, ROOT), s[: m.start()].count("\n") + 1))
    regs = len(rows)
    by_path = collections.defaultdict(set)
    for path, crate, *_ in rows:
        by_path[path].add(crate)
    classes = collections.Counter(classify(p) for p in by_path)
    per_crate = collections.Counter(crate for _, crate, *_ in rows)
    dupes = {p: c for p, c in by_path.items() if len(c) > 1}

    if "--paths" in sys.argv:
        for p in sorted(by_path):
            print(f"{classify(p):13} {p:60} {','.join(sorted(by_path[p]))}")
        return
    if "--dupes" in sys.argv:
        for p in sorted(dupes):
            print(f"{p:60} {','.join(sorted(dupes[p]))}")
        return
    print(f"registrations: {regs}   unique paths: {len(by_path)}   paths in >1 crate: {len(dupes)}")
    print("per host crate:", ", ".join(f"{c}={n}" for c, n in per_crate.most_common()))
    print()
    for c in ["turn", "job", "node", "resource", "store", "out", "UNCLASSIFIED"]:
        print(f"{c:13} {classes.get(c, 0):4}")
    un = [p for p in by_path if classify(p) == "UNCLASSIFIED"]
    if un:
        print("\nUNCLASSIFIED:")
        for p in sorted(un):
            print("  ", p)

if __name__ == "__main__":
    main()
