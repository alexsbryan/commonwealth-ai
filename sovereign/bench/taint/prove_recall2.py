#!/usr/bin/env python3
"""Faithful pipeline for a MECHANICAL class: structural-guard FINDS it (the LLM
finder missed it), VERIFIER adjudicates. Test the verifier's INDEPENDENT reasoning
(provenance only, NO guard verdict handed over) at both commits:

  BUGGY (76b72a4b): must CONFIRM  -> saw through `.max(1)` on the numerator
  FIXED (6c9b4fd6): must REFUTE   -> understood `(len/20).max(1)` >= 1

Neutral claim (states the Rust fact that step_by(0) panics, but NOT whether the
stride can actually be 0 here -- the verifier must compute that from the code).
"""
from taint import PARSER, txt
import prove_regression as pr
import verify_loop as vl

CLAIM=dict(cls="Panic: Iterator::step_by receives a stride of 0",
    mech="the `.step_by(...)` stride argument may evaluate to 0 for some document sizes, and Rust's `Iterator::step_by(0)` panics at runtime",
    trig="(to be determined by you from the code — state a concrete document size if it can be 0, else refute)")
PROV="- input `all_chunks` is the document's retrieved chunk list; its length is data-dependent and can be SMALL (a short document has few chunks). Integers here are usize; `usize` division truncates toward zero."

def verify_at(commit, label):
    src=pr.file_at(commit); tree=PARSER.parse(src)
    n=pr.find_stepby_calls(tree.root_node, src)[0]
    fn=pr.enclosing_fn(n); first,body=vl.numbered(fn,src)
    line=n.start_point[0]+1
    vraw=vl.chat(vl.VERIFY_PROMPT.format(facts=PROV, fn="execute_synthesis",
        rel=f"document_asset.rs ({label})", body=body,
        cls=CLAIM["cls"], line=line, mech=CLAIM["mech"], trig=CLAIM["trig"]),
        temperature=0.0, max_tokens=380)
    v=vl.jobj(vraw)
    arg=txt(n.child_by_field_name("arguments"),src)
    print(f"\n=== {label} ({commit[:8]})  L{line}: .step_by{arg} ===")
    print(f"  VERDICT: {v.get('verdict')}   killed_by={v.get('killed_by')}")
    print(f"  reason:  {v.get('reason')}")
    return str(v.get("verdict","")).upper()

if __name__=="__main__":
    print("#"*80); print("# VERIFIER INDEPENDENT-REASONING TEST (structural guard found it; LLM finder missed it)")
    print("#"*80)
    b=verify_at(pr.BUGGY,"BUGGY parent")
    f=verify_at(pr.FIXED,"FIXED")
    print("\n"+"#"*80)
    print(f"# buggy -> {b}   (want CONFIRMED)")
    print(f"# fixed -> {f}   (want REFUTED)")
    if b=="CONFIRMED" and f=="REFUTED":
        print("# RECALL + FIX-CLEARANCE PROVEN: verifier confirmed the real panic, saw through")
        print("# `.max(1)` on the numerator, and correctly cleared the fix — end to end.")
    elif b=="CONFIRMED":
        print("# Verifier confirmed the real bug (recall OK); fix-clearance = "+f)
    else:
        print("# Verifier did NOT confirm the real bug independently (verdict="+b+"). The STRUCTURAL")
        print("# guard is what carries recall for this class; verifier needs the guard verdict as grounding.")
    print("#"*80)
