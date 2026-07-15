#!/usr/bin/env python3
"""BLIND discovery test: adjudicate every UNGUARDED underflow shape at HEAD
(17 candidate needles nobody placed) with the fixed reasoning-first verifier.

For each site: hand the verifier the enclosing function + the structural claim,
force it to compute the offset for the smallest reachable inputs BEFORE deciding.
CONFIRMED = a real reachable panic; REFUTED = an earlier guard/loop-bound/type
prevents it; UNCERTAIN = undecidable from shown code. I hand-adjudicate the
CONFIRMED/UNCERTAIN survivors against the code afterward.
"""
import flowgraph as fg
from taint import txt
import verify_loop as vl

RF="""You are a skeptical Rust reviewer. Static analysis flagged a possible index/underflow panic. Reason step by step FIRST, then commit.

Function `{fn}` ({rel}):
```rust
{body}
```

FLAGGED: line {line}, expression `{text}` — the offset could underflow (index computed as `x - k`; if `x` can be 0/<k, `usize` wraps → out-of-bounds panic). Static verdict: {reason}.

Work it out: identify the index variable, find every place it's bounded (loop range `for i in 1..n`, an `if x > 0` / early-return guard, `.max`/`saturating_sub`, or a type). Compute the offset for the smallest value the variable can take. Decide only after the arithmetic:
  CONFIRMED  a reachable input makes the offset underflow / index out of bounds
  REFUTED    an earlier guard, loop lower-bound, or type makes underflow impossible
  UNCERTAIN  cannot tell from the shown code

Respond with ONLY a JSON object, reasoning BEFORE verdict:
{{"reasoning":"the bounding facts + arithmetic for the smallest input","verdict":"CONFIRMED|REFUTED|UNCERTAIN","killed_by":"guard|loop-bound|type|absent-code|none"}}"""

def main():
    ung=[(n,a) for n,a in fg.NODE.items() if a.get("kind")=="sink" and a.get("guard")=="UNGUARDED"]
    seen=set(); sites=[]
    for n,a in ung:
        key=(a.get('rel'),a.get('line'))
        if key in seen: continue
        seen.add(key); sites.append((n,a))
    print(f"[blind] adjudicating {len(sites)} unique UNGUARDED sites\n",flush=True)
    tally={}
    for n,a in sites:
        q=fg.qual_of(n); rel=a.get('rel'); line=a.get('line')
        fn_entry=fg.FN.get(q)
        if fn_entry is None:
            print(f"  (skip {rel}:{line} — no fn node)"); continue
        frel,node,src=fn_entry
        first,body=vl.numbered(node,src)
        raw=vl.chat(RF.format(fn=fg.simple(q),rel=rel.split('/')[-1],body=body,
            line=line,text=a.get('text',''),reason=a.get('guard_reason','')),
            temperature=0.0,max_tokens=520)
        o=vl.jobj(raw)
        vd=str(o.get('verdict','UNPARSED')).upper(); tally[vd]=tally.get(vd,0)+1
        print(f"  [{vd:9}] {rel.split('/')[-1]}:{line} {fg.simple(q)}() killed_by={o.get('killed_by')}",flush=True)
        print(f"            {str(o.get('reasoning',''))[:220]}")
    print(f"\n[blind] tally: {tally}")

if __name__=="__main__":
    main()
