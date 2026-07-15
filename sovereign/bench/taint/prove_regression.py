#!/usr/bin/env python3
"""PROVE VALUE: would the tool have caught a real shipped panic at commit time?

Target: the sampling-stride panic fixed in 6c9b4fd6 ("fix(tools): document
sampling stride panic on <20-chunk docs"). Buggy parent = 76b72a4b.

  BUGGY  .step_by(all_chunks.len().max(1) / 20)   -> 5.max(1)/20 == 0 -> step_by(0) PANIC
  FIXED  .step_by((all_chunks.len() / 20).max(1)) -> (5/20).max(1) == 1 -> safe

The trap: BOTH lines contain `.max(1)`. A grep/naive check says "guarded" for
both. Only the structural discriminator (does .max wrap the DIVISION or just the
numerator?) tells them apart. We run guard.sink_guard on the real function text
at each commit and show RED (UNGUARDED) -> GREEN (SAFE) across the exact fix.
"""
import subprocess, sys
from taint import PARSER, txt, build_letmap
import guard

REL="sovereign/crates/sovereign-tools/src/document_asset.rs"
BUGGY="76b72a4baa8e25ed6e6ca98d1296e00444a4e1db"
FIXED="6c9b4fd6"

def file_at(commit):
    return subprocess.check_output(["git","show",f"{commit}:{REL}"],
        cwd="/home/alexbryan/dev/commonwealth-ai").decode("utf8","ignore").encode()

def find_stepby_calls(root, src, needle="all_chunks.len"):
    """Yield step_by call_expression nodes whose arg mentions the needle."""
    out=[]; st=[root]
    while st:
        n=st.pop()
        if n.type=="call_expression":
            f=n.child_by_field_name("function")
            if f is not None and f.type=="field_expression":
                fld=f.child_by_field_name("field")
                if fld is not None and txt(fld,src)=="step_by":
                    a=n.child_by_field_name("arguments")
                    if a is not None and a.named_children and needle in txt(a,src):
                        out.append(n)
        for c in n.children: st.append(c)
    return out

def enclosing_fn(node):
    a=node.parent
    while a is not None and a.type!="function_item": a=a.parent
    return a

def audit(commit, label):
    src=file_at(commit)
    tree=PARSER.parse(src)
    calls=find_stepby_calls(tree.root_node, src)
    print(f"\n=== {label}  ({commit[:8]})  — {len(calls)} matching step_by site(s) ===")
    verdicts=[]
    for n in calls:
        fn=enclosing_fn(n)
        body=fn.child_by_field_name("body") if fn is not None else None
        lm=build_letmap(body,src) if body is not None else {}
        verdict,reason=guard.sink_guard("panic:step_by", n, lm, src)
        line=n.start_point[0]+1
        arg=txt(n.child_by_field_name("arguments"),src)
        print(f"  L{line}: .step_by{arg}")
        print(f"        -> {verdict}  ({reason})")
        verdicts.append(verdict)
    return verdicts

if __name__=="__main__":
    print("#"*80)
    print("# REGRESSION-GATE PROOF: real shipped panic (doc sampling stride, <20 chunks)")
    print("#"*80)
    buggy=audit(BUGGY,"BUGGY (parent, panic shipped)")
    fixed=audit(FIXED,"FIXED (guard added)")
    print("\n"+"#"*80)
    red = any(v=="UNGUARDED" for v in buggy)
    green = all(v=="SAFE" for v in fixed) and len(fixed)>0
    print(f"# RED at buggy commit (>=1 UNGUARDED): {red}   {buggy}")
    print(f"# GREEN at fixed commit (all SAFE):    {green}   {fixed}")
    if red and green:
        print("# PROVEN: the tool flags the panic at the buggy commit and clears it at the fix.")
        print("# A naive `.max(1)` grep would call BOTH commits safe (both contain .max(1)).")
    else:
        print("# NOT proven as expected — inspect above.")
    print("#"*80)
