#!/usr/bin/env python3
"""Finder -> structural-grounding -> adversarial-verifier loop, and MEASURE it.

Pipeline per candidate function (an untrusted->sink wire from the flow graph):
  1. FACTS   the graph's structural grounding: which param is untrusted (+kind),
             the resolved struct field TYPES, and each sink's guard verdict.
  2. FIND    open-ended generative pass (no taxonomy) -> candidate findings.
             High recall, KNOWN to emit confident false positives (the probe).
  3. VERIFY  independent adversarial pass per finding. Never sees the finder's
             reasoning -- only the claim + the same code + the structural facts.
             Told to REFUTE by default; CONFIRM only with a concrete reachable
             trigger that no earlier guard/type prevents. -> CONFIRMED/REFUTED/UNCERTAIN.

Measurement: does finder->verifier convert noisy recall into precision?
  - survivors = CONFIRMED findings.
  - the acid test: does VERIFY kill the finder's known false positive
    (exec_patch_file "negative end_line" -- refuted by u32 field type + line 419),
    while KEEPING the real ones (symlink recursion, unconfined path, tier underflow)?

Both roles run on the 122B here; the finder=cheap/verifier=strong MODEL split is a
follow-up. What we measure now is the ARCHITECTURE split (independent refutation).
"""
import json, os, re, sys, urllib.request
from collections import defaultdict
import flowgraph as fg
from taint import txt, params_ordered

DAEMON="http://localhost:9741/v1/chat/completions"
M122="Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003"
REPO=fg.REPO

def chat(prompt, temperature=0.2, max_tokens=1100, model=M122):
    body=json.dumps(dict(model=model,temperature=temperature,max_tokens=max_tokens,
        messages=[{"role":"user","content":prompt}])).encode()
    req=urllib.request.Request(DAEMON,data=body,headers={"content-type":"application/json"})
    with urllib.request.urlopen(req,timeout=600) as r:
        return json.load(r)["choices"][0]["message"]["content"]

def jarr(s):
    a,b=s.find("["),s.rfind("]")
    if a<0 or b<a: return []
    try: return json.loads(s[a:b+1])
    except Exception:
        # tolerate a trailing truncated object: keep up to the last complete }
        frag=s[a:b+1]
        cut=frag.rfind("},")
        if cut>0:
            try: return json.loads(frag[:cut+1]+"]")
            except Exception: return []
        return []

def jobj(s):
    a,b=s.find("{"),s.rfind("}")
    if a<0 or b<a: return {}
    try: return json.loads(s[a:b+1])
    except Exception: return {}

# ---- structural facts from the graph -------------------------------------
_struct_cache={}
def struct_fields(type_str):
    """Best-effort resolve `Foo`/`&Foo`/`State<Foo>` -> [(field,type)] by grepping
    `struct Foo { ... }` in the workspace. This is the typed grounding the
    structural layer computes (types kill whole false-positive families)."""
    m=re.search(r"([A-Z][A-Za-z0-9_]+)", type_str or "")
    if not m: return []
    name=m.group(1)
    if name in _struct_cache: return _struct_cache[name]
    out=[]
    try:
        import subprocess
        # 1) locate the definition site
        r=subprocess.run(["grep","-rn","--include=*.rs","-m1",f"struct {name} ",
                          REPO+"/sovereign",REPO+"/commonwealth"],
                         capture_output=True,text=True,timeout=20)
        hit=r.stdout.strip().splitlines()
        if hit:
            path,lno=hit[0].split(":",2)[0],int(hit[0].split(":",2)[1])
            # 2) read that file and brace-scan the struct body
            with open(path,"r",errors="ignore") as f: flines=f.readlines()
            depth=0; started=False
            for ln in flines[lno-1:lno-1+60]:
                depth+=ln.count("{")-ln.count("}")
                if "{" in ln: started=True
                fm=re.match(r"\s*(?:pub\s+)?([a-z_][a-z0-9_]*)\s*:\s*([^,]+),",ln)
                if fm: out.append((fm.group(1),fm.group(2).strip()))
                if started and depth<=0: break
    except Exception: pass
    _struct_cache[name]=out
    return out

_STD_TYPE=re.compile(r"^&?\s*(String|str|Path|PathBuf|Bytes|BytesMut|Vec|Option|usize|u\d+|i\d+|bool|char|HashMap|HashSet)\b")
def facts_for(qual, rel, node, src):
    """Assemble the structural grounding block for one function."""
    lines=[]; tagged=[]
    params=[(pn,pt) for pn,pt in params_ordered(node,src)
            if pn not in ("_","ctx","self","state","app")]
    for pn,pt in params:
        a=fg.NODE.get(fg.node_p(qual,pn),{})
        if a.get("source_kind"):
            tagged.append(pn)
            lines.append(f"- param `{pn}: {pt.strip()}` is UNTRUSTED (source_kind={a['source_kind']}, "
                         f"confidence={a.get('source_conf')}).")
    # name adversary-influenced inputs even when the taint arrives interprocedurally
    if not tagged and params:
        lines.insert(0,f"- inputs {', '.join(f'`{p}`' for p,_ in params)} are adversary-influenced "
                       f"(the flow graph established the taint interprocedurally).")
    # resolved struct field TYPES for the adversarial/untrusted params only (types kill FP families)
    adv=set(p for p,_ in params)
    for pn,pt in params:
        if pn in adv and not _STD_TYPE.match(pt.strip()):
            for f,t in struct_fields(pt):
                lines.append(f"- field `{pn}.{f}` has resolved type `{t}` "
                             f"(a type constrains which inputs are possible, e.g. an unsigned int is never negative).")
    # sinks in this fn + guard verdicts
    for nid,a in fg.NODE.items():
        if a.get("kind")=="sink" and fg.qual_of(nid)==qual:
            lines.append(f"- sink at line {a.get('line')} (`{a.get('text','')}`): structural guard verdict "
                         f"= {a.get('guard')} ({a.get('guard_reason')}).")
    return "\n".join(lines) if lines else "(no additional structural facts)"

def numbered(node, src):
    s,e=node.start_point[0]+1,node.end_point[0]+1
    txt_lines=src.decode("utf8","ignore").splitlines()
    return s,"\n".join(f"{i:>4} | {txt_lines[i-1]}" for i in range(s,e+1))

# ---- prompts -------------------------------------------------------------
FIND_PROMPT="""You are auditing one Rust function the data-flow analyzer proved receives ADVERSARIAL input.

Structural facts established by the analyzer (ground truth):
{facts}

Function `{fn}` ({rel}):

```rust
{body}
```

Enumerate EVERY distinct way a motivated adversary controlling the untrusted input could make this function misbehave. Do not limit yourself to any category. For each distinct issue output an object:
  "class"      - a short name you choose for the failure kind
  "line"       - the single most relevant line number
  "mechanism"  - one sentence referencing the actual code
  "trigger"    - a concrete adversarial input
  "confidence" - high | medium | low
Do not invent code that isn't shown. Respond with ONLY a JSON array, most severe first."""

VERIFY_PROMPT="""You are a SKEPTICAL Rust security reviewer. Another tool CLAIMS the function below has a specific defect. Your job is to REFUTE the claim. Assume it is wrong until a concrete, reachable adversarial input proves otherwise.

Structural facts established by static analysis (ground truth -- use these to refute):
{facts}

Function `{fn}` ({rel}):

```rust
{body}
```

CLAIM to adjudicate:
  class: {cls}
  line: {line}
  mechanism: {mech}
  trigger: {trig}

Refute if ANY of these hold:
  - an earlier line in this function guards/validates away the trigger,
  - a field/parameter TYPE makes the trigger impossible (e.g. a u32 field can never be negative),
  - the claim references behavior not present in the shown code.
CONFIRM only if you can state a concrete input that (a) is type-valid, (b) reaches the cited line past every earlier guard, and (c) produces the harm. When you cannot construct that, answer REFUTED. If genuinely undecidable from the shown code, answer UNCERTAIN.

Respond with ONLY a JSON object:
  {{"verdict":"CONFIRMED"|"REFUTED"|"UNCERTAIN","reason":"one sentence citing the deciding line or type","killed_by":"guard|type|absent-code|none"}}"""

# ---- select candidate functions from the graph ---------------------------
def qual_by_name(name):
    for q,(rel,node,src) in fg.FN.items():
        if fg.simple(q)==name: yield q

def select_targets(cap=7):
    UNTRUSTED=lambda a: a.get("source_kind") is not None
    srcs,hits,path=fg.reach_paths(UNTRUSTED, lambda a:a.get("kind")=="sink")
    # rank sink functions: inject first, then UNGUARDED, then UNCERTAIN
    prio={"inject":0,"UNGUARDED":1,"UNCERTAIN":2}
    perfn={}
    for h in hits:
        a=fg.NODE.get(h,{}); q=fg.qual_of(h)
        if q not in fg.FN: continue
        rank=prio.get(a.get("sink")) if a.get("sink")=="inject" else prio.get(a.get("guard"),3)
        perfn[q]=min(perfn.get(q,9),rank if rank is not None else 3)
    ordered=[q for q,_ in sorted(perfn.items(),key=lambda kv:kv[1])]
    picked=list(ordered[:cap])
    # force-include ground-truth targets so precision can be scored
    for nm in ("build_triage_candidates","count_emails","validate_email_source","exec_patch_file"):
        for q in qual_by_name(nm):
            if q not in picked: picked.append(q)
            break
    return picked

# ground-truth labels for the functions I hand-verified (for the scoreboard)
GROUND_TRUTH={
  "build_triage_candidates":"REAL underflow: tier_counts[(tier)-1] on [0usize;6] panics if tier==0/>6; also (6-tier) score wrap.",
  "count_emails":"REAL: unbounded recursion + symlink-cycle infinite recursion (is_dir follows symlinks).",
  "validate_email_source":"REAL: no path confinement; is_dir follows symlinks -> arbitrary-path read + TOCTOU.",
  "exec_patch_file":"GUARDED: slice at :450 prevented by :419 (start_line>=1) and u32 fields; 'negative line' claims are FALSE POSITIVES.",
}

def main():
    cap=int(os.environ.get("CAP","7"))
    maxf=int(os.environ.get("MAXF","4"))
    targets=select_targets(cap)
    print(f"[loop] {len(targets)} candidate functions selected from the graph\n",flush=True)
    tally=defaultdict(int); rows=[]
    for q in targets:
        rel,node,src=fg.FN[q]; fn=fg.simple(q)
        first,body=numbered(node,src)
        facts=facts_for(q,rel,node,src)
        base=os.path.basename(rel)
        print("="*100); print(f"{fn}  ({base}:{first})")
        print("-- structural facts --"); print(facts); print("-- finder --",flush=True)
        raw=chat(FIND_PROMPT.format(facts=facts,fn=fn,rel=base,body=body),temperature=0.3,max_tokens=1100)
        findings=jarr(raw)
        # order by confidence, cap
        order={"high":0,"medium":1,"low":2}
        findings=sorted(findings,key=lambda f:order.get(str(f.get("confidence","low")).lower(),3))[:maxf]
        print(f"   finder produced {len(findings)} findings (capped {maxf})",flush=True)
        fn_rows=[]
        for f in findings:
            cls=str(f.get("class","?")); line=f.get("line","?")
            vraw=chat(VERIFY_PROMPT.format(facts=facts,fn=fn,rel=base,body=body,
                       cls=cls,line=line,mech=f.get("mechanism",""),trig=f.get("trigger","")),
                      temperature=0.0,max_tokens=320)
            v=jobj(vraw); verdict=str(v.get("verdict","UNPARSED")).upper()
            tally[verdict]+=1
            fn_rows.append((verdict,cls,line,v.get("killed_by",""),v.get("reason","")))
            print(f"     [{verdict:9}] L{line} {cls[:46]:48} killed_by={v.get('killed_by','')}")
        rows.append((fn,base,first,fn_rows,GROUND_TRUTH.get(fn)))
        print(flush=True)
    # scoreboard
    print("#"*100); print("# SCOREBOARD"); print("#"*100)
    print(f"verdict tally: {dict(tally)}")
    print(f"\nper-function survivors (CONFIRMED) vs ground truth:")
    for fn,base,first,fn_rows,gt in rows:
        conf=[r for r in fn_rows if r[0]=="CONFIRMED"]
        ref=[r for r in fn_rows if r[0]=="REFUTED"]
        print(f"\n  {fn} ({base}:{first})  confirmed={len(conf)} refuted={len(ref)} other={len(fn_rows)-len(conf)-len(ref)}")
        if gt: print(f"    GROUND TRUTH: {gt}")
        for verdict,cls,line,kb,reason in fn_rows:
            mark="✓" if verdict=="CONFIRMED" else ("✗" if verdict=="REFUTED" else "?")
            print(f"      {mark} [{verdict:9}] L{line} {cls[:44]:46} :: {str(reason)[:70]}")

if __name__=="__main__":
    main()
