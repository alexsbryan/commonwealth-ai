#!/usr/bin/env python3
"""Feasibility probe: can an open-weight model GENERATIVELY find defect classes
we never formalized, given a grounded candidate flow?

We take real functions the flow-graph flags (untrusted source -> sink) and hand
the model ONLY:
  - the untrusted-input note (what the graph proved about provenance)
  - the exact function body (grounded, line-numbered)
and an OPEN-ENDED prompt: "enumerate every distinct way adversarial input makes
this misbehave -- name the class yourself, cite the line, give a trigger."

No taxonomy is supplied. The prompt never says "underflow", "path traversal",
"panic", "TOCTOU". Whatever classes appear are the model's own. We then read the
output and judge: (a) does it rediscover the class we DID formalize? (b) does it
surface classes we did NOT? (c) is it grounded or hallucinating?
"""
import json, sys, urllib.request
from tree_sitter import Language, Parser
import tree_sitter_rust as tsr

LANG = Language(tsr.language()); PARSER = Parser(LANG)
ROOT = "/home/alexbryan/dev/commonwealth-ai/"
DAEMON = "http://localhost:9741/v1/chat/completions"
MODEL = "Qwen3.5-122B-A10B-UD-Q5_K_XL-00001-of-00003"

def _fns(node):
    st=[node]
    while st:
        n=st.pop()
        if n.type=="function_item": yield n
        for c in n.children: st.append(c)

def extract(rel, *, name=None, line=None):
    """Return (fn_name, first_line, numbered_body) for the fn matching name or spanning line."""
    with open(ROOT+rel,"rb") as f: src=f.read()
    tree=PARSER.parse(src)
    best=None
    for fn in _fns(tree.root_node):
        s,e = fn.start_point[0]+1, fn.end_point[0]+1
        if name is not None:
            nm=fn.child_by_field_name("name")
            if nm is not None and src[nm.start_byte:nm.end_byte].decode()==name:
                best=(fn,s,e); break
        elif line is not None and s<=line<=e:
            # innermost enclosing fn spanning the line
            if best is None or (fn.start_point[0] > best[0].start_point[0]):
                best=(fn,s,e)
    if best is None: raise SystemExit(f"fn not found in {rel} name={name} line={line}")
    fn,s,e = best
    nm=fn.child_by_field_name("name")
    fn_name = src[nm.start_byte:nm.end_byte].decode() if nm is not None else "?"
    lines = src.decode("utf8","ignore").splitlines()
    numbered = "\n".join(f"{i:>4} | {lines[i-1]}" for i in range(s,e+1))
    return fn_name, s, numbered

PROMPT = """You are auditing one Rust function that the data-flow analyzer has proven receives ADVERSARIAL input.

Provenance (established by the analyzer, treat as ground truth):
{prov}

Function `{fn}` ({rel}, lines shown with real line numbers):

```rust
{body}
```

Task: enumerate EVERY distinct way a motivated adversary controlling the untrusted input could make this function misbehave. Do not limit yourself to any category. For each distinct issue, output an object:
  - "class": a short name YOU choose for the failure kind
  - "line": the single most relevant line number from the listing
  - "mechanism": one sentence, referencing the actual code, on why it goes wrong
  - "trigger": a concrete adversarial input that exercises it
  - "confidence": high | medium | low
If a concern you considered is actually PREVENTED by the code, instead emit it with "class":"prevented" and explain in "mechanism" which line stops it. Be skeptical and specific; do not invent code that isn't shown.

Respond with ONLY a JSON array of these objects, most severe first."""

TARGETS = [
    dict(label="T1 tier_counts (formalized: underflow)", rel="sovereign/crates/sovereign-tools/src/atlas_postinstall.rs",
         line=608,
         prov="`records`/`expansions` are rows deserialized from an on-disk atlas file produced by an earlier pipeline stage; each row's `.tier` field is an untrusted integer with no proven range."),
    dict(label="T2 count_emails (unformalized: fs recursion on untrusted path)", rel="sovereign/crates/sovereign-desktop/src-tauri/src/import_commands.rs",
         name="count_emails",
         prov="`path` originates from a #[tauri::command] argument (`import_email_archive`) — a filesystem path chosen entirely by the caller/UI, no allow-list applied before it reaches here."),
    dict(label="T3 apply_edit_range (control: slice IS guarded at :423)", rel="sovereign/crates/commonwealth-agent-tools/src/executor.rs",
         line=450,
         prov="`args.start_line`/`args.end_line` come from a tool-call JSON emitted by an LLM agent — attacker-influenceable integers. Earlier lines in this function may or may not constrain them."),
    dict(label="T4 validate_email_source (generative: validator logic)", rel="sovereign/crates/sovereign-desktop/src-tauri/src/import_commands.rs",
         name="validate_email_source",
         prov="`path` is a #[tauri::command]-supplied filesystem path. This function is the SECURITY GATE that decides whether import proceeds; anything it fails to reject flows downstream to recursive fs reads."),
]

def call(prompt, max_tokens=1400):
    body=json.dumps(dict(model=MODEL, temperature=0.2, max_tokens=max_tokens,
        messages=[{"role":"user","content":prompt}])).encode()
    req=urllib.request.Request(DAEMON, data=body, headers={"content-type":"application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.load(r)["choices"][0]["message"]["content"]

if __name__=="__main__":
    only = sys.argv[1] if len(sys.argv)>1 else None
    for t in TARGETS:
        if only and only not in t["label"]: continue
        fn_name, first, body = extract(t["rel"], name=t.get("name"), line=t.get("line"))
        prompt = PROMPT.format(prov=t["prov"], fn=fn_name, rel=t["rel"].split("/")[-1], body=body)
        print("="*100); print(t["label"]); print(f"  fn `{fn_name}` @ {t['rel'].split('/')[-1]}:{first}"); print("="*100)
        out = call(prompt)
        print(out)
        print()
