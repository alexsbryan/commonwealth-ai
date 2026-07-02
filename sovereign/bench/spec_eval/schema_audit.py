#!/usr/bin/env python3
"""Phase 0 — schema-coverage audit for the fact-base scale-out.

Classify real claims against the CLOSED relation schema and measure in-schema
coverage. This is the cheap gate that decides whether the deterministic fact base
has enough reach to be worth building. Doubles as a dry-run of the Phase 2a claim
tagger (same closed-set classification the real pipeline will do).
See docs/internal/FACT_BASE_SCALE_OUT.md.
"""
import json, os, re, urllib.request
from collections import Counter

SETS = [
    ("CODE_INTEL_CHAT", "~/.sovereign/specs/commonwealth-ai/CODE_INTEL_CHAT/claims.json"),
    ("semver",          "~/.sovereign/specs/semver/README/claims.json"),
    ("tinyorders",      "~/.sovereign/specs/tinyorders/README/claims.json"),
]
SCHEMA = """EXISTS - the code defines/has a named function, type, or symbol.
CALLS - X calls / invokes / dispatches / routes-to / uses Y (a wiring relation).
CONFIG - X sets a config field / flag / parameter to a specific value (e.g. tools=None).
LITERAL - X contains / emits / outputs a specific string, format, endpoint, or constant.
CAPABILITY - X is the entry point / handler for a situation or capability (the claim's "when/for" trigger).
CONTROL - control-flow shape: loops, single-shot, ordering, before/after.
TYPE - X produces / returns / has a specific type or data structure.
OUT_OF_SCHEMA - needs deep data-flow (value computed through variables/branches), cross-function emergent behavior, or subjective/qualitative judgment."""
SYS = ("Classify a spec claim by which RELATION TYPES its conditions assert about the code, from this "
       "closed set. A claim may assert more than one. Use OUT_OF_SCHEMA only if NONE of the concrete "
       "relations fit.\n\n" + SCHEMA + "\n\nOutput JSON: {\"relations\": [\"...\"], \"note\": \"<=12 words\"}.")
VALID = {"EXISTS", "CALLS", "CONFIG", "LITERAL", "CAPABILITY", "CONTROL", "TYPE", "OUT_OF_SCHEMA"}


def chat(u):
    req = {"model": "Qwen3.5-9B-UD-MTP-Q6_K_XL", "temperature": 0.1, "max_tokens": 200,
           "messages": [{"role": "system", "content": SYS}, {"role": "user", "content": u}]}
    r = urllib.request.urlopen(urllib.request.Request(
        "http://localhost:9741/v1/chat/completions",
        data=json.dumps(req).encode(), headers={"Content-Type": "application/json"}), timeout=120)
    return json.load(r)["choices"][0]["message"]["content"]


def load(path):
    d = json.load(open(os.path.expanduser(path)))
    if isinstance(d, dict) and "sections" in d:
        return [c for s in d["sections"] for c in s.get("claims", [])]
    return d.get("claims", d) if isinstance(d, dict) else d


rel_count = Counter()
in_schema = total = 0
out = []
for name, path in SETS:
    claims = load(path)
    print(f"\n=== {name} ({len(claims)} claims) ===")
    for c in claims:
        stmt = c["statement"]
        try:
            raw = chat(f"CLAIM: {stmt}\nCONDITIONS: {c.get('conditions', [])}")
            ex = json.loads(re.search(r"\{.*\}", raw, re.S).group(0))
            rels = [r for r in ex.get("relations", []) if r in VALID] or ["OUT_OF_SCHEMA"]
        except Exception as e:
            rels, ex = ["OUT_OF_SCHEMA"], {"note": f"parse-fail {e}"}
        total += 1
        concrete = [r for r in rels if r != "OUT_OF_SCHEMA"]
        if concrete:
            in_schema += 1
        else:
            out.append((name, stmt))
        for r in rels:
            rel_count[r] += 1
        print(f"  [{'IN ' if concrete else 'OUT'}] {','.join(rels):32} {stmt[:54]}")

print("\n" + "=" * 72)
print(f"COVERAGE: {in_schema}/{total} claims have >=1 in-schema relation ({100 * in_schema // total}%)")
print("relation distribution (a claim may assert several):")
for r, n in rel_count.most_common():
    print(f"   {r:16} {n}")
print(f"\nOUT_OF_SCHEMA claims ({len(out)}):")
for name, s in out:
    print(f"   [{name}] {s[:72]}")
