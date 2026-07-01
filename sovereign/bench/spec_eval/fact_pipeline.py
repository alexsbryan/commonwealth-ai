#!/usr/bin/env python3
"""Phases 1-2 measured pipeline: NL claim -> closed-schema query over the deterministic
fact base -> verdict, scored against the answer key. See docs/internal/FACT_BASE_SCALE_OUT.md.

  TAG (LLM, concrete-first, recognizes CONFIG — the Phase 0 correction)
  SCOPE (fn_vecs subject resolution + SCIP call-neighborhood)
  DISPATCH (facts.json + scip_graph.db) -> verdict, SAFETY: drift only on a cited
           contradicting fact; absence -> unverifiable.
"""
import json, os, re, sys, struct, sqlite3, urllib.request
from collections import Counter

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.expanduser("~/.sovereign")
DB = f"{DATA}/indexes/commonwealth-ai/scip_graph.db"
cur = sqlite3.connect(DB).cursor()
facts = json.load(open(f"{DATA}/indexes/commonwealth-ai/facts.json"))
CTOR, LITS, FNDEFS = facts["ctor_fields"], facts["str_lits"], facts["fn_defs"]

# ── fn_vecs for subject resolution ──
side = json.load(open(f"{DATA}/specs/_fn_vecs/commonwealth-ai.json"))
FNS, DIM = side["fns"], side["dim"]
raw = open(f"{DATA}/specs/_fn_vecs/commonwealth-ai.bin", "rb").read()
VECS = [struct.unpack_from(f"<{DIM}f", raw, i * DIM * 4) for i in range(len(FNS))]
VNORM = [sum(x * x for x in v) ** 0.5 or 1.0 for v in VECS]

# Phase 1c — restrict subject resolution to capability ENTRIES (front-doors), not all 31k.
# The tool *definitions* the claim's predicate drifts to are leaf functions, not entries, so
# they're excluded; a genuine entry like handle_code_query wins even on a predicate-heavy claim.
CAPMAP = json.load(open(f"{DATA}/capabilities/commonwealth-ai/capability_map.json"))["capabilities"]
_name2i = {}
for _i, _fm in enumerate(FNS):
    _name2i.setdefault(_fm.get("name", ""), _i)
ENTRY_IDX, _seen = [], set()
for _c in CAPMAP:
    _reps = [r if isinstance(r, str) else r.get("name", "") for r in _c.get("reps", [])]
    for _e in _c.get("entries", []):
        _s = _e if isinstance(_e, str) else _e.get("name", "")
        _short = _s.split("#")[-1].split("]")[-1].rstrip("().").strip() if _s else ""
        if _short and "(" not in _short:
            _reps.append(_short)
    for _n in _reps:
        if _n in _name2i and _n not in _seen:
            _seen.add(_n); ENTRY_IDX.append((_n, _name2i[_n]))


def post(p, pl):
    return json.load(urllib.request.urlopen(urllib.request.Request(
        f"http://localhost:9741{p}", data=json.dumps(pl).encode(),
        headers={"Content-Type": "application/json"}), timeout=120))


def chat(sys, user, mx=300):
    return post("/v1/chat/completions", {"model": "Qwen3.5-9B-UD-MTP-Q6_K_XL", "temperature": 0.1,
        "max_tokens": mx, "messages": [{"role": "system", "content": sys}, {"role": "user", "content": user}]})["choices"][0]["message"]["content"]


def embed(t):
    return post("/v1/embeddings", {"model": "Qwen3-Embedding-0.6B-Q8_0", "input": t})["data"][0]["embedding"]


def resolve(desc, k=8):
    q = embed(desc); qn = sum(x * x for x in q) ** 0.5 or 1.0
    idx = sorted(range(len(FNS)), key=lambda i: -sum(a * b for a, b in zip(q, VECS[i])) / (VNORM[i] * qn))[:k]
    return [FNS[i].get("name", "") for i in idx]


def resolve_entry(desc, k=1):
    q = embed(desc); qn = sum(x * x for x in q) ** 0.5 or 1.0
    ranked = sorted(ENTRY_IDX, key=lambda ni: -sum(a * b for a, b in zip(q, VECS[ni[1]])) / (VNORM[ni[1]] * qn))
    return [n for n, _ in ranked[:k]]


def _qstem(q):
    """file-stem from a SCIP qualified id's module path (language-agnostic: SCIP indexers all
    encode module/path#symbol). e.g. '...runtime/handlers/knowledge_query/impl#[R]f().' -> knowledge_query"""
    path = q.split("#")[0].strip()
    segs = [s for s in path.split("/") if s and s not in ("impl", "mod")]
    return segs[-1] if segs else ""


def neighborhood_stems(names, depth=2):
    """the set of source-file stems the call-flow touches (from the QUALIFIED closure — no bare-name
    collisions). Scoping facts by file-stem membership is principled + language-agnostic."""
    seeds = []
    for n in names:
        pat = f"%{n}%()." if len(n) >= 6 else f"%]{n}()."
        seeds += [q for (q,) in cur.execute("SELECT DISTINCT caller_qualified FROM refs WHERE caller_qualified LIKE ? LIMIT 6", (pat,))]
    seen, frontier = set(seeds), list(seeds)
    for _ in range(depth):
        nxt = []
        for i in range(0, len(frontier), 400):
            c = frontier[i:i + 400]; qm = ",".join("?" * len(c))
            for (cal,) in cur.execute(f"SELECT DISTINCT callee_qualified FROM refs WHERE caller_qualified IN ({qm}) AND callee_qualified<>''", c):
                if cal not in seen:
                    seen.add(cal)
                    if cal.rstrip().endswith("()."):
                        nxt.append(cal)
        frontier = nxt
    return {_qstem(s) for s in seen if s.rstrip().endswith("().")}


def fstem(path):
    return os.path.basename(path).rsplit(".", 1)[0]


def resolve_files(desc, k=4):
    q = embed(desc); qn = sum(x * x for x in q) ** 0.5 or 1.0
    idx = sorted(range(len(FNS)), key=lambda i: -sum(a * b for a, b in zip(q, VECS[i])) / (VNORM[i] * qn))[:k]
    return {FNS[i].get("file", "") for i in idx if FNS[i].get("file")}


def neighborhood(names, depth=2):
    seeds = []
    for n in names:
        pat = f"%{n}%()." if len(n) >= 6 else f"%]{n}()."
        seeds += [q for (q,) in cur.execute("SELECT DISTINCT caller_qualified FROM refs WHERE caller_qualified LIKE ? LIMIT 6", (pat,))]
    seen, frontier = set(seeds), list(seeds)
    for _ in range(depth):
        nxt = []
        for i in range(0, len(frontier), 400):
            c = frontier[i:i + 400]; qm = ",".join("?" * len(c))
            for (cal,) in cur.execute(f"SELECT DISTINCT callee_qualified FROM refs WHERE caller_qualified IN ({qm}) AND callee_qualified<>''", c):
                if cal not in seen:
                    seen.add(cal)
                    if cal.rstrip().endswith("()."):
                        nxt.append(cal)
        frontier = nxt
    short = lambda q: re.split(r"[\]/ ]", q.rstrip().rstrip(".").rstrip("()"))[-1]
    return {short(s) for s in seen if s.rstrip().endswith("().")} | set(names)


# ── TAG: concrete-first, recognizes CONFIG ──
TAG_SYS = (
    "Classify a spec claim's PRIMARY checkable relation about the code, preferring CONCRETE relations. "
    "Check in this order and pick the FIRST that fits:\n"
    "CONFIG - asserts a request/struct field or flag is set/unset (e.g. 'exposes tools to the model' or 'has no tools' => field=tools; 'agentic loop enabled' => field=enabled). Give `field` = the bare field name.\n"
    "LITERAL - asserts a VERBATIM string appears: an endpoint ('/v1/chat/completions'), a marker ('SUMMARY:'), a model name ('qwen-embedding-0.6b'). Give `literal` = the exact substring only, no surrounding words.\n"
    "EXISTS - asserts a named function or type exists / is the named entry. Give `target` (the bare name).\n"
    "CALLS - asserts X calls / invokes / reaches Y. Give `target`.\n"
    "CAPABILITY / CONTROL / OUT_OF_SCHEMA - only if none of the concrete relations above fit.\n"
    "`subject` = a short description of the code path/situation the claim is about (for locating it). "
    "`expected` = YES if the claim asserts the thing is present/true, NO if absent/false.\n"
    'Output JSON: {"relation":"...","subject":"...","field":"","literal":"","target":"","expected":"YES"}.')


def tag(claim):
    raw = chat(TAG_SYS, f"CLAIM: {claim['statement']}\nCONDITIONS: {claim.get('conditions', [])}")
    m = re.search(r"\{.*\}", raw, re.S)
    return json.loads(m.group(0)) if m else None


def reaches(scope_fns, target):
    # bounded SCIP callee reachability from scope to a target name
    seeds = []
    for n in list(scope_fns)[:10]:
        seeds += [q for (q,) in cur.execute("SELECT DISTINCT caller_qualified FROM refs WHERE caller_qualified LIKE ? LIMIT 4", (f"%{n}%().",))]
    seen, frontier = set(seeds), list(seeds)
    for _ in range(3):
        nxt = []
        for i in range(0, len(frontier), 400):
            c = frontier[i:i + 400]; qm = ",".join("?" * len(c))
            for (cal,) in cur.execute(f"SELECT DISTINCT callee_qualified FROM refs WHERE caller_qualified IN ({qm})", c):
                if target in cal:
                    return True
                if cal not in seen:
                    seen.add(cal); nxt.append(cal)
        frontier = nxt
    return False


def claim_literals(stmt, tagged):
    """Distinctive code-like literals from the claim itself — robust to the tagger mangling them."""
    cands = [tagged] if tagged and len(tagged) >= 4 else []
    cands += [a or b for a, b in re.findall(r"'([^']+)'|\"([^\"]+)\"", stmt)]  # quoted
    cands += re.findall(r"/v\d[a-z0-9/_]+", stmt)                              # endpoints
    cands += re.findall(r"\b[A-Z]{3,}:", stmt)                                 # SUMMARY: / ASKS:
    cands += re.findall(r"\b[a-z]+-[a-z0-9][a-z0-9.\-]{3,}", stmt)             # qwen-embedding-0.6b
    # dedup, distinctive-first
    seen, out = set(), []
    for c in sorted({c.strip() for c in cands if len(c.strip()) >= 4}, key=len, reverse=True):
        if c not in seen:
            seen.add(c); out.append(c)
    return out


def resolve_symbol(name):
    """A tagged target -> a real defined symbol. Exact first; fn_vecs fallback ONLY for
    identifier-shaped targets whose resolved symbol shares a token (kills prose->garbage)."""
    d = next((d for d in FNDEFS if d["name"] == name), None)
    if d:
        return name, d
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name or ""):
        return None, None  # prose target -> don't guess
    key = name.lower()
    for cand in resolve(name, k=3):
        if cand and (key in cand.lower() or cand.lower() in key):
            d = next((d for d in FNDEFS if d["name"] == cand), None)
            if d:
                return cand, d
    return None, None


def dispatch(t, scope_fns, stmt):
    rel = t.get("relation", "OUT_OF_SCHEMA")
    exp = t.get("expected", "YES").upper()
    if rel == "CONFIG":
        field = t.get("field", "")
        # scope by the flow's FILES (from the qualified call graph): registry.rs is not in the
        # code-query flow, so its `new`-set tools=Vec::new() is excluded — no magic name list.
        hits = [c for c in CTOR if c["field"] == field and fstem(c["file"]) in scope_fns]
        if not hits:
            return "unverifiable", f"no `{field}` config fact in the resolved scope"
        vals = {c["value"] for c in hits}
        nones = {v for v in vals if v.startswith("None")}
        somes = vals - nones
        c = hits[0]
        if exp == "YES":
            if somes and not nones:
                return "corroborated", f"{field} set present {sorted(somes)} at {c['file']}:{c['line']}"
            if nones and not somes:  # asserted present, all scoped sites set it absent -> cited contradiction
                return "drift", f"{field}=None at all {len(hits)} scoped site(s), e.g. {c['file']}:{c['line']}"
            return "unverifiable", f"{field} mixed {sorted(vals)} — scope impure, abstain"
        return "unverifiable", f"{field}={sorted(vals)}"
    if rel == "LITERAL":
        cands = [t["literal"]] if t.get("literal") else []      # architecture path: the clean tag literal
        cands += claim_literals(stmt, "")                       # fallback: only compensates for weak LLM tags
        for lit in [l for l in cands if len(l) >= 4]:
            hit = next((s for s in LITS if lit in s["content"]), None)
            if hit:
                return ("corroborated" if exp == "YES" else "drift"), f'literal "{lit}" at {hit["file"]}:{hit["line"]}'
        return "unverifiable", f'no literal found (tried {cands[:3]})'
    if rel == "EXISTS":
        sym, d = resolve_symbol(t.get("target", ""))
        if d:
            return ("corroborated" if exp == "YES" else "drift"), f"{sym} defined at {d['file']}:{d['line']}"
        return "unverifiable", f"{t.get('target','')} not resolvable to a defined symbol"
    if rel == "CALLS":
        tgt = t.get("target", "")
        sym, _ = resolve_symbol(tgt)
        probe = sym or tgt
        if probe and reaches(scope_fns, probe):
            return ("corroborated" if exp == "YES" else "drift"), f"scope reaches {probe}"
        return "unverifiable", f"no path to {probe} (absence != drift)"
    return "unverifiable", f"{rel} — deferred to the fuzzy path"


# ── drive + score ──
claims = [c for s in json.load(open(f"{DATA}/specs/commonwealth-ai/CODE_INTEL_CHAT/claims.json"))["sections"] for c in s.get("claims", [])]
key = json.load(open(f"{HERE}/answer_key.CODE_INTEL_CHAT.json"))["labels"]
GOLD = json.load(open(f"{HERE}/gold_tags.CODE_INTEL_CHAT.json"))["tags"]
MODE = "gold" if "--gold" in sys.argv else ("tagger" if "--tagger" in sys.argv else "llm")  # gold = architecture; tagger = tagger-vs-gold


def gold_tag(claim):
    s = claim["statement"]
    for g in GOLD:
        if g["match"] in s:
            return {k: v for k, v in g.items() if k != "match"}
    return {"relation": "OUT_OF_SCHEMA"}

print(f"MODE={MODE}")
if MODE == "tagger":  # measure the tagger against gold — separate number from the architecture
    rel_ok = arg_ok = 0
    for c in claims:
        g, l = gold_tag(c), (tag(c) or {"relation": "OUT_OF_SCHEMA"})
        m = g.get("relation") == l.get("relation")
        rel_ok += m
        garg = g.get("literal") or g.get("target") or g.get("field") or ""
        larg = l.get("literal") or l.get("target") or l.get("field") or ""
        a = m and (not garg or garg.lower() in larg.lower() or larg.lower() in garg.lower())
        arg_ok += a
        if not m:
            print(f"  REL-MISS  gold={g.get('relation'):13} llm={l.get('relation'):13} | {c['statement'][:46]}")
        elif not a and garg:
            print(f"  ARG-MISS  {g.get('relation'):13} gold_arg={garg!r:24} llm_arg={larg!r:24}")
    print(f"\nTAGGER relation-match: {rel_ok}/{len(claims)}   relation+arg usable: {arg_ok}/{len(claims)}")
    sys.exit(0)
print(f"fact base: {len(CTOR)} ctor-fields · {len(LITS)} lits · {len(FNDEFS)} fn-defs\n" + "=" * 74)
rows = []
for c in claims:
    t = gold_tag(c) if MODE == "gold" else (tag(c) or {"relation": "OUT_OF_SCHEMA"})
    subj = t.get("subject", c["statement"])
    if t.get("relation") == "CONFIG":
        scope = neighborhood_stems(resolve_entry(c["statement"], k=1), depth=2)  # entry-restricted + flow FILES (qualified, collision-free)
    elif t.get("relation") == "CALLS":
        scope = neighborhood(resolve(subj))
    else:
        scope = set(resolve(subj))
    v, receipt = dispatch(t, scope, c["statement"])
    gt = key.get(c["statement"], {}).get("expect", "?")
    rows.append((v, gt, c["statement"], t.get("relation"), receipt))
    flag = "  ⚠FALSE-DRIFT" if v == "drift" and gt != "drift" else ("  ✓caught" if v == "drift" and gt == "drift" else "")
    print(f"[{v:12}|{t.get('relation',''):11}] key={gt:12} {c['statement'][:44]}{flag}")
    print(f"       {receipt[:96]}")

print("=" * 74)
opined = [r for r in rows if r[0] in ("drift", "corroborated")]
drift = [r for r in rows if r[0] == "drift"]
false_drift = [r for r in drift if r[1] != "drift"]
false_corrob = [r for r in opined if r[0] == "corroborated" and r[1] in ("drift", "gap")]
caught = [r for r in drift if r[1] == "drift"]
# agreement on opined (drift/corroborated vs key; map key drift->drift, corroborated/todo->corroborated-ish)
agree = sum(1 for v, gt, *_ in opined if (v == "drift" and gt == "drift") or (v == "corroborated" and gt in ("corroborated", "todo")))
print(f"deterministic answers (drift|corrob): {len(opined)}/{len(rows)}   abstained (unverifiable): {len(rows)-len(opined)}")
print(f"agreement on answered: {agree}/{len(opined)}")
print(f"DRIFT recall: {len(caught)}/{sum(1 for _,gt,*_ in rows if gt=='drift')}   FALSE-DRIFT: {len(false_drift)}   FALSE-CORROBORATED: {len(false_corrob)}   (trust-killers, want 0)")
print("relation mix:", dict(Counter(r[3] for r in rows)))
for r in false_drift + false_corrob:
    print(f"  ⚠ {r[0]} vs key={r[1]}: {r[2][:52]} | {r[4][:56]}")
