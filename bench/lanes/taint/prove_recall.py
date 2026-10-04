#!/usr/bin/env python3
"""RECALL test on a REAL shipped bug: does finder->verifier CONFIRM the
sampling-stride panic at the buggy parent commit (76b72a4b)?

Honest fact split (mirrors the real pipeline):
  FINDER   gets provenance ONLY (all_chunks length is data-dependent) -- no guard
           hint. Tests blind recall on a 179-line function.
  VERIFIER additionally gets the structural UNGUARDED verdict. Tests whether the
           LLM confirms a real bug -- and whether it sees through the `.max(1)`
           trap (the buggy line CONTAINS `.max(1)`, on the numerator).
"""
from taint import PARSER, txt, build_letmap
import guard, prove_regression as pr
import verify_loop as vl

src=pr.file_at(pr.BUGGY); tree=PARSER.parse(src)
calls=pr.find_stepby_calls(tree.root_node, src)
fn=pr.enclosing_fn(calls[0])
first, body = vl.numbered(fn, src)
lm=build_letmap(fn.child_by_field_name("body"), src)

PROV="- input `all_chunks` is the document's retrieved chunk list; its length is data-dependent and can be SMALL (a short document has few chunks)."
GUARD_FACTS=[PROV]
for n in calls:
    v,r=guard.sink_guard("panic:step_by", n, lm, src)
    GUARD_FACTS.append(f"- sink at line {n.start_point[0]+1} (`.step_by{txt(n.child_by_field_name('arguments'),src)}`): structural guard verdict = {v} ({r}).")
finder_facts=PROV                       # provenance only
verifier_facts="\n".join(GUARD_FACTS)   # provenance + structural verdict

print("#"*80); print("# RECALL PROOF — buggy commit 76b72a4b, fn execute_synthesis (179 lines)")
print("#"*80); print("finder facts (blind):\n ",finder_facts)
print("verifier facts (grounded):"); print("  "+verifier_facts.replace("\n","\n  ")); print()

raw=vl.chat(vl.FIND_PROMPT.format(facts=finder_facts, fn="execute_synthesis",
            rel="document_asset.rs (buggy parent)", body=body), temperature=0.3, max_tokens=1300)
findings=vl.jarr(raw)
print(f"-- FINDER produced {len(findings)} findings --")
def is_stepby(f):
    t=(str(f.get("class",""))+" "+str(f.get("mechanism",""))+" "+str(f.get("trigger",""))).lower()
    return "step_by" in t or "step by" in t or "divi" in t or "stride" in t or "/ 20" in t or "zero" in t
hit=None
for f in findings:
    mark="  <-- STEP_BY/DIV-ZERO" if is_stepby(f) else ""
    print(f"   [{f.get('confidence','?'):6}] L{f.get('line')} {str(f.get('class'))[:50]}{mark}")
    if is_stepby(f) and hit is None: hit=f
print()
if hit is None:
    print("RESULT: FINDER MISSED the stride panic (no step_by/div-zero finding). Recall fails at the finder.")
else:
    print(f"FINDER FOUND it: class=`{hit.get('class')}` line={hit.get('line')}")
    print(f"   mechanism: {hit.get('mechanism')}")
    print(f"   trigger:   {hit.get('trigger')}")
    vraw=vl.chat(vl.VERIFY_PROMPT.format(facts=verifier_facts, fn="execute_synthesis", rel="document_asset.rs",
                 body=body, cls=hit.get("class"), line=hit.get("line"), mech=hit.get("mechanism"), trig=hit.get("trigger")),
                 temperature=0.0, max_tokens=360)
    v=vl.jobj(vraw)
    print(f"\n-- VERIFIER verdict: {v.get('verdict')}  (killed_by={v.get('killed_by')}) --")
    print(f"   reason: {v.get('reason')}")
    print()
    if str(v.get("verdict","")).upper()=="CONFIRMED":
        print("RECALL PROVEN: finder->verifier CONFIRMED a real shipped panic, and the LLM")
        print("saw through the `.max(1)` trap (the buggy line contains .max(1) on the numerator).")
    else:
        print("RECALL GAP: the verifier did NOT confirm the real bug (likely fooled by the visible")
        print(".max(1), or too conservative). The STRUCTURAL guard still catches it -> both layers needed.")
