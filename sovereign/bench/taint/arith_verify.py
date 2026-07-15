#!/usr/bin/env python3
"""Does the reasoning-first verifier filter the arith:sub firehose down to real
bugs? Sample = the real levenshtein n-m + several invariant-based FPs."""
import flowgraph as fg
import verify_loop as vl

RF="""You are a skeptical Rust reviewer. Static analysis flagged a possible UNSIGNED-integer underflow. Reason step by step FIRST, then commit.

Function `{fn}` ({rel}):
```rust
{body}
```

FLAGGED: line {line}, expression `{text}`. If both operands are unsigned and the right can exceed the left for some reachable input, the subtraction underflows (wraps huge / panics in debug).

Work it out before deciding: Are both operands unsigned (`as i32` etc. are signed → cannot panic)? Is there an INVARIANT making left >= right — e.g. one string is a trim/substring of the other so its `.len()` is necessarily <=, a dominating `if left > right`, sorted order, or a loop bound? Compute the smallest inputs.
  CONFIRMED  a reachable input makes right > left
  REFUTED    an invariant / guard / signed type guarantees no underflow
  UNCERTAIN  cannot tell from the shown code

Respond ONLY a JSON object, reasoning BEFORE verdict:
{{"reasoning":"invariant/arithmetic for smallest inputs","verdict":"CONFIRMED|REFUTED|UNCERTAIN","killed_by":"invariant|guard|type|absent-code|none"}}"""

# (rel-substring, line, my label) — real bug first, then suspected FPs
SAMPLE=[
  ("frontdoor.rs",2198,"REAL: n,m are char counts, swap is by BYTE length"),
  ("executor.rs",550,"FP?: line.len() - trimmed.len(), trimmed is a trim of line"),
  ("ab.rs",249,"FP?: max - min"),
  ("model_files.rs",290,"FP?: end - start"),
  ("frontdoor.rs",910,"?: open_count - close_count"),
  ("frontdoor.rs",600,"?: cmd.len() - trimmed_start.len()"),
]

def find(relsub,line):
    for n,a in fg.NODE.items():
        if a.get("sink")=="arith:sub" and relsub in (a.get("rel") or "") and a.get("line")==line:
            return n,a
    return None,None

for relsub,line,label in SAMPLE:
    n,a=find(relsub,line)
    if n is None: print(f"  (not a flagged arith:sub: {relsub}:{line})"); continue
    q=fg.qual_of(n); rel,node,src=fg.FN[q]
    first,body=vl.numbered(node,src)
    raw=vl.chat(RF.format(fn=fg.simple(q),rel=relsub,body=body,line=line,text=a.get("text","")),
                temperature=0.0,max_tokens=460)
    o=vl.jobj(raw)
    print(f"\n[{str(o.get('verdict','?')):9}] {relsub}:{line}  `{a.get('text','')}`   ({label})")
    print(f"    killed_by={o.get('killed_by')}  ::  {str(o.get('reasoning',''))[:260]}")
