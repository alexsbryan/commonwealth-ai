#!/usr/bin/env python3
"""Instrument: for each false-positive sink, trace WHICH guard predicate fails."""
from taint import PARSER, txt, build_letmap
import guard

REPO="/home/alexbryan/dev/commonwealth-ai"
SITES=[
    ("sovereign/crates/sovereign-core/src/title.rs", 800),
    ("sovereign/crates/sovereign-core/src/title.rs", 809),
    ("sovereign/crates/sovereign-tools/src/code/atos_utils.rs", 177),
    ("sovereign/crates/sovereign-tools/src/local_corpus/frontmatter.rs", 64),
    ("commonwealth/crates/commonwealth-api/src/frontdoor.rs", 2209),
]

def index_at(root, src, line):
    hits=[]; st=[root]
    while st:
        n=st.pop()
        if n.type=="index_expression" and n.start_point[0]+1==line: hits.append(n)
        for c in n.children: st.append(c)
    return hits

def enclosing_fn(n):
    a=n.parent
    while a is not None and a.type!="function_item": a=a.parent
    return a

for rel,line in SITES:
    with open(f"{REPO}/{rel}","rb") as f: src=f.read()
    tree=PARSER.parse(src)
    idxs=index_at(tree.root_node, src, line)
    print(f"\n=== {rel.split('/')[-1]}:{line}  ({len(idxs)} index_expr on this line) ===")
    for n in idxs:
        base,offs=guard._bounds_of_index(n)
        base_text=txt(base,src) if base is not None else ""
        fn=enclosing_fn(n); body=fn.child_by_field_name("body") if fn else None
        lm=build_letmap(body,src) if body else {}
        uvars=guard._underflow_vars(offs,src)
        print(f"  index=`{txt(n,src)}`  base=`{base_text}`  offset_vars={uvars}")
        print(f"    parent chain: ", end="")
        a=n.parent; chain=[]
        for _ in range(6):
            if a is None: break
            chain.append(a.type); a=a.parent
        print(" -> ".join(chain))
        print(f"    dominating_len_guard = {guard.dominating_len_guard(n, base_text, src)}")
        print(f"    loop_bound_guard     = {guard.loop_bound_guard(n, offs, src)}")
        print(f"    positivity_guard     = {guard.positivity_guard(n, uvars, src)}")
        print(f"    FINAL sink_guard     = {guard.sink_guard('panic:index', n, lm, src)}")
