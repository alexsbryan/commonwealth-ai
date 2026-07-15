#!/usr/bin/env python3
"""Generalized data-dependence graph generator (the "untangled wires").

NOT a bug detector. Generates ONE artifact — a typed flow graph over the whole
codebase — where every question is `reach(source_pred, sink_pred)`. Panic-taint,
injection-taint, and program slicing are the SAME reachability, differing only
in which nodes are sources/sinks (the IFDS decomposition: build once, query many).

Nodes (per function, keyed by SCIP qualified name QUAL):
  QUAL::@p:<name>     parameter (carries type)     QUAL::<name>        local binding
  QUAL::@sink:<line>  expression sink (op text)    QUAL::@ret          return value
  QUAL::@srccall:<ln> a deserialize/body SOURCE call
Edge  a -> b  ==  "b derives from a"  (data dependence), intra- + inter-procedural.
"""
import os, re, sqlite3, sys, json, time
from collections import defaultdict, deque
from taint import (PARSER, txt, params_ordered, build_letmap, data_idents,
                   idents_in_pattern, is_test_function)
import sources, guard

REPO="/home/alexbryan/dev/commonwealth-ai"
DB="/home/alexbryan/.sovereign/indexes/commonwealth-ai/scip_graph.db"
con=sqlite3.connect(DB)

PANIC_SINK={"unwrap","expect"}
INJECT_CALL=re.compile(r"Command::new|process::Command|fs::(read|write|remove_|create|File|"
                       r"copy|rename|open)|tokio::fs::|PathBuf::from|Path::new|"
                       r"\.execute\(|\.query\(|reqwest::(get|Client)")

# ---- helpers -------------------------------------------------------------
_fc={}
def parse_file(rel):
    if rel in _fc: return _fc[rel]
    try:
        with open(os.path.join(REPO,rel),"rb") as f: src=f.read()
    except OSError:
        _fc[rel]=(None,None); return _fc[rel]
    _fc[rel]=(src, PARSER.parse(src)); return _fc[rel]

def has_attr(fn_node, src, needle):
    prev=fn_node.prev_named_sibling
    while prev is not None and prev.type in ("attribute_item","line_comment","block_comment"):
        if prev.type=="attribute_item" and needle in txt(prev,src): return True
        prev=prev.prev_named_sibling
    return False

def simple(qual): return qual.rstrip("().").split("#")[-1].split("/")[-1]
def node_p(q,n): return f"{q}::@p:{n}"
def node_l(q,n): return f"{q}::{n}"
def node_ret(q): return f"{q}::@ret"

fwd=defaultdict(set); rev=defaultdict(set); NODE={}
def add_node(nid,**a):
    NODE.setdefault(nid,{}).update(a)
def add_edge(a,b):
    if a!=b: fwd[a].add(b); rev[b].add(a)
def src_node(q,v):
    pid=node_p(q,v)
    return pid if pid in NODE else node_l(q,v)
def add_sink(q,rel,n,kind,expr,src,letmap):
    sid=f"{q}::@sink:{n.start_point[0]+1}:{kind}"
    verdict,reason=guard.sink_guard(kind,n,letmap,src)
    add_node(sid,kind="sink",sink=kind,fn=simple(q),rel=rel,
             line=n.start_point[0]+1,text=txt(n,src)[:70].replace("\n"," "),
             guard=verdict,guard_reason=reason)
    for v in data_idents(expr,src): add_edge(src_node(q,v),sid)

# ---- SCIP load -----------------------------------------------------------
t0=time.time()
print("[scip] loading refs + symbols...", file=sys.stderr)
refs_by_caller=defaultdict(list)
for cq,kq,ln in con.execute(
    "SELECT caller_qualified, callee_qualified, line FROM refs "
    "WHERE caller_qualified LIKE '% 0.1.20 %().' AND callee_qualified LIKE '% 0.1.20 %().'"):
    refs_by_caller[cq].append((kq,ln))
rows_by_file=defaultdict(list)
for q,f,ls in con.execute("SELECT qualified_name,file_path,line_start FROM symbols "
                          "WHERE qualified_name LIKE '% 0.1.20 %().'"):
    rows_by_file[f].append((ls,q))

# ---- locate every function's AST node, keyed by QUAL ---------------------
FN={}   # qual -> (rel, node, src)
def index_file_functions(rel):
    src,tree=parse_file(rel)
    if src is None: return
    file_quals=rows_by_file.get(rel,[])
    if not file_quals: return
    asts=[]; st=[tree.root_node]
    while st:
        n=st.pop()
        if n.type=="function_item":
            nm=n.child_by_field_name("name")
            asts.append((txt(nm,src) if nm else "?", n))
        for c in n.children: st.append(c)
    used=set()
    for name,node in asts:
        if is_test_function(node,src): continue
        srow=node.start_point[0]; best=None
        for ls,q in file_quals:
            if simple(q)==name and abs(ls-srow)<=3 and q not in used:
                if best is None or abs(ls-srow)<abs(best[0]-srow): best=(ls,q)
        if best: FN[best[1]]=(rel,node,src); used.add(best[1])

def is_test_rel(rel):
    return "/tests/" in rel or rel.endswith(("tests.rs","_test.rs","integration_tests.rs")) \
        or "/test-" in rel or "test_harness" in rel or "/benches/" in rel

for rel in list(rows_by_file.keys()):
    if is_test_rel(rel): continue
    index_file_functions(rel)
print(f"[graph] {len(FN)} functions located ({time.time()-t0:.1f}s)", file=sys.stderr)

# ---- emit nodes + edges (one AST pass per function) ----------------------
def build():
    for q,(rel,node,src) in FN.items():
        params=params_ordered(node,src)
        is_tauri=has_attr(node, src, "tauri::command")
        for pn,pt in params:
            sk=sources.classify_param(pn, pt, is_tauri_cmd=is_tauri, rel=rel)
            add_node(node_p(q,pn),kind="param",type=pt.strip(),fn=simple(q),rel=rel,
                     line=node.start_point[0]+1,
                     source_kind=sk[0] if sk else None, source_conf=sk[1] if sk else None)
        body=node.child_by_field_name("body")
        if body is None: continue
        lm=build_letmap(body,src)
        callsites=defaultdict(list)   # callee simple-name -> [(argnode,pos)]
        def walk(n):
            if n.type=="let_declaration":
                pat=n.child_by_field_name("pattern"); val=n.child_by_field_name("value")
                if val is not None and pat is not None:
                    for tgt in idents_in_pattern(pat,src):
                        tid=node_l(q,tgt); add_node(tid,kind="local",fn=simple(q),rel=rel)
                        for v in data_idents(val,src): add_edge(src_node(q,v),tid)
                        sc=sources.source_call_kind(txt(val,src))
                        if sc:
                            sn=f"{q}::@srccall:{val.start_point[0]+1}"
                            add_node(sn,kind="source_call",source_kind=sc[0],source_conf=sc[1],
                                     fn=simple(q),rel=rel,line=val.start_point[0]+1,text=txt(val,src)[:60])
                            add_edge(sn,tid)
            elif n.type=="return_expression":
                for v in data_idents(n,src): add_edge(src_node(q,v),node_ret(q))
            elif n.type=="call_expression":
                f=n.child_by_field_name("function"); cn=None
                if f is not None:
                    if f.type=="identifier": cn=txt(f,src)
                    elif f.type=="scoped_identifier":
                        nm=f.child_by_field_name("name"); cn=txt(nm,src) if nm else None
                    elif f.type=="field_expression":
                        fld=f.child_by_field_name("field"); cn=txt(fld,src) if fld else None
                        recv=f.child_by_field_name("value")
                        if cn in PANIC_SINK and recv is not None: add_sink(q,rel,n,"panic:"+cn,recv,src,lm)
                        elif cn=="step_by":
                            a=n.child_by_field_name("arguments")
                            if a is not None and a.named_children: add_sink(q,rel,n,"panic:step_by",a.named_children[0],src,lm)
                    a=n.child_by_field_name("arguments")
                    if cn and a is not None:
                        for i,arg in enumerate(a.named_children): callsites[cn].append((arg,i))
                if INJECT_CALL.search(txt(n,src)[:90]):
                    a=n.child_by_field_name("arguments")
                    if a is not None:
                        for arg in a.named_children: add_sink(q,rel,n,"inject",arg,src,lm)
            elif n.type=="index_expression":
                kids=[c for c in n.named_children]
                if len(kids)>=2: add_sink(q,rel,n,"panic:index",kids[1],src,lm)
            elif n.type=="binary_expression":
                ops=[txt(c,src) for c in n.children if not c.is_named]
                if "-" in ops:
                    kids=[c for c in n.named_children]
                    if len(kids)==2 and not any(k.type=="integer_literal" for k in kids):
                        add_sink(q,rel,n,"arith:sub",n,src,lm)
            for c in n.named_children: walk(c)
        walk(body)
        # interproc param-bind: caller arg_i -> callee @p:param_i
        for (callee_q,cl) in refs_by_caller.get(q,[]):
            callee=FN.get(callee_q)
            if callee is None: continue
            cparams=params_ordered(callee[1],callee[2])
            for (argnode,pos) in callsites.get(simple(callee_q),[]):
                if pos<len(cparams):
                    for v in data_idents(argnode,src): add_edge(src_node(q,v),node_p(callee_q,cparams[pos][0]))

build()
print(f"[graph] nodes={len(NODE)} edges={sum(len(v) for v in fwd.values())} "
      f"({time.time()-t0:.1f}s)", file=sys.stderr)

# ---- QUERY ENGINE: reach(source_pred, sink_pred) -------------------------
def sources(pred): return [n for n,a in NODE.items() if pred(a)]
def reach_paths(source_pred, sink_pred, max_paths=25):
    srcs=set(sources(source_pred))
    parent={s:None for s in srcs}
    dq=deque(srcs); sinks_hit=[]
    while dq:
        n=dq.popleft()
        if sink_pred(NODE.get(n,{})) and n not in srcs:
            sinks_hit.append(n)
        for nb in fwd.get(n,()):
            if nb not in parent:
                parent[nb]=n; dq.append(nb)
    def path(n):
        p=[];
        while n is not None: p.append(n); n=parent.get(n)
        return list(reversed(p))
    return srcs, sinks_hit, path

def backward_slice(node):
    seen={node}; dq=deque([node])
    while dq:
        n=dq.popleft()
        for pr in rev.get(n,()):
            if pr not in seen: seen.add(pr); dq.append(pr)
    return seen

def qual_of(nid): return nid.split("::@")[0].split("::")[0]
def fn_short(nid): return qual_of(nid).split("0.1.20 ")[-1]
def distinct_funcs(p): return len({qual_of(s) for s in p})

if __name__=="__main__":
    from collections import Counter
    UNTRUSTED=lambda a: a.get("source_kind") is not None
    srccount=Counter(a["source_kind"] for a in NODE.values() if a.get("source_kind"))
    print(f"\n[input layer] structural untrusted sources: {sum(srccount.values())}")
    for k,n in srccount.most_common(): print(f"    {k:20} {n}")
    def show_wire(h,p):
        a=NODE.get(h,{})
        print(f"  WIRE ({distinct_funcs(p)} fns) [{a.get('guard')}: {a.get('guard_reason','')}]")
        print(f"       -> {a.get('rel')}:{a.get('line')} `{a.get('text','')}`")
        last=None
        for step in p:
            fq=fn_short(step)
            if fq!=last: print(f"         {fq}()"); last=fq
    def qshow(name, spred, kpred, bucket_guard=False):
        srcs,hits,path=reach_paths(spred,kpred)
        wires=[(h,path(h)) for h in hits if distinct_funcs(path(h))>=2]
        wires.sort(key=lambda hp: -distinct_funcs(hp[1]))
        print(f"\n### QUERY: {name}")
        print(f"  sources={len(srcs)}  interprocedural wires={len(wires)}")
        if bucket_guard:
            b=Counter(NODE.get(h,{}).get('guard') for h,_ in wires)
            print(f"  SINK-GUARD buckets: {dict(b)}")
            for tag,label in (("UNGUARDED","REAL candidates (unguarded panic shape)"),
                              ("UNCERTAIN","need verifier")):
                sel=[(h,p) for h,p in wires if NODE.get(h,{}).get('guard')==tag]
                if sel:
                    print(f"\n  --- {tag}: {label} ({len(sel)}) ---")
                    for h,p in sel[:4]: show_wire(h,p)
        else:
            for h,p in wires[:4]: show_wire(h,p)
    qshow("PANIC — untrusted value reaches unwrap/index/step_by",
          UNTRUSTED, lambda a: a.get("sink","").startswith("panic:"), bucket_guard=True)
    qshow("INJECTION — untrusted value reaches Command/fs/path/http sink",
          UNTRUSTED, lambda a: a.get("sink")=="inject")
    panic_sinks=[n for n,a in NODE.items() if a.get("sink","").startswith("panic:")]
    if panic_sinks:
        ex=max(panic_sinks, key=lambda n: len(backward_slice(n)))
        sl=backward_slice(ex)
        files=set(NODE.get(n,{}).get('rel') for n in sl); files.discard(None)
        print(f"\n### QUERY: SLICE — backward dependency cone of one sink (same graph)")
        print(f"  sink {NODE[ex].get('rel')}:{NODE[ex].get('line')} `{NODE[ex].get('text','')}`")
        print(f"  transitively depends on {len(sl)} flow nodes across {len(files)} files")
    # guard-as-lint: UNGUARDED panic shapes anywhere (taint-independent latent panics)
    unguarded=[(n,a) for n,a in NODE.items()
               if a.get("kind")=="sink" and a.get("guard")=="UNGUARDED"]
    gb=Counter(a.get("sink") for _,a in unguarded)
    print(f"\n### GUARD-LINT: UNGUARDED panic shapes anywhere in the tree (no taint needed)")
    print(f"  total UNGUARDED sinks: {len(unguarded)}   by kind: {dict(gb)}")
    for n,a in unguarded[:12]:
        print(f"    {a.get('rel')}:{a.get('line')}  [{a.get('guard_reason')}]  `{a.get('text','')}`")

    print(f"\n[generality] one generated graph ({len(NODE)} nodes / "
          f"{sum(len(v) for v in fwd.values())} edges); each question above is a "
          f"different (source_pred, sink_pred). A new vuln class = one new predicate.")
