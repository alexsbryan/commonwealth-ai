#!/usr/bin/env python3
"""Blind sweep with arithmetic-underflow sinks + guard fixes. Lives in the taint
dir so it imports the REAL guard.py (not a scratchpad shadow)."""
import flowgraph as fg
from collections import Counter
ung=[(n,a) for n,a in fg.NODE.items() if a.get("kind")=="sink" and a.get("guard")=="UNGUARDED"]
print(f"guard module: {__import__('guard').__file__}")
print(f"UNGUARDED total: {len(ung)}   by kind: {dict(Counter(a.get('sink') for _,a in ung))}\n")
# did we find the levenshtein n-m bug?
lev=[(n,a) for n,a in ung if 'frontdoor' in (a.get('rel') or '') and a.get('sink')=='arith:sub'
     and 2190 <= (a.get('line') or 0) <= 2205]
print("=== levenshtein n-m (frontdoor ~2198) ===")
for n,a in lev: print(f"  FOUND {a.get('rel').split('/')[-1]}:{a.get('line')} `{a.get('text','')}` ({a.get('guard_reason')})")
if not lev: print("  NOT flagged")
# did executor:450 clear (guard-and-return fix)?
ex=[(n,a) for n,a in fg.NODE.items() if a.get('kind')=='sink' and 'executor' in (a.get('rel') or '') and a.get('line')==450]
print("\n=== executor:450 (guard-and-return fix should clear -> not UNGUARDED) ===")
for n,a in ex: print(f"  executor.rs:450 -> {a.get('guard')} ({a.get('guard_reason')})")
# full arith:sub UNGUARDED list (new class — inspect FP blast radius)
arith=[(a.get('rel'),a.get('line'),a.get('text',''),a.get('guard_reason')) for n,a in ung if a.get('sink')=='arith:sub']
seen=set(); arith=[x for x in arith if (x[0],x[1]) not in seen and not seen.add((x[0],x[1]))]
print(f"\n=== arith:sub UNGUARDED ({len(arith)} unique) ===")
for rel,line,text,reason in sorted(arith)[:40]:
    print(f"  {rel.split('/')[-1]}:{line}  `{text}`")
