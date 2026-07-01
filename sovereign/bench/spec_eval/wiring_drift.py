#!/usr/bin/env python3
"""Measured pipeline for behavioral/wiring DRIFT — the layer summary-recall can't do.

Summary-recall (spec_reconcile) is strong at CORROBORATION ("find the function whose
PURPOSE is X") but structurally blind to wiring drift ("this path does the OPPOSITE"):
it surfaces the tool *definitions* and misses the answer path that disables them, and
deep call-graph reachability false-corroborates (pollution). This check adds the piece
that works, validated end-to-end on claim 12:

  EXTRACT  (LLM)  claim prose -> {wiring?, subject desc, local yes/no question, expected}
  RESOLVE  (cosine) subject desc -> real function(s), via the fn_vecs purpose index
  RECALL   (graph)  shallow depth<=2 CALL-neighborhood of the subject (qualified refs)
  JUDGE    (LLM)    the LOCAL FACTUAL question over the neighborhood bodies -> actual + cite
  VERDICT  (code)   expected vs actual  ->  DRIFT | CORROBORATED   (judge can't manufacture either)

Non-wiring claims ABSTAIN (summary-recall owns those). Scored against the verified
answer key: does #12 flip to drift, and is false-DRIFT (the trust-killer) held at 0?

Run: python3 wiring_drift.py            (needs the daemon at :9741)
"""
import json, os, re, struct, sqlite3, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.expanduser("~/.sovereign")
DB = f"{DATA}/indexes/commonwealth-ai/scip_graph.db"
REPO = "/home/alexbryan/dev/commonwealth-ai"
CHAT_MODEL = "Qwen3.5-9B-UD-MTP-Q6_K_XL"
EMBED_MODEL = "Qwen3-Embedding-0.6B-Q8_0"
STOP = set("the a an is are of to for and or with as it its this that when where which how does do "
           "system model code path each via using use given into from on in by".split())

cur = sqlite3.connect(DB).cursor()


def post(path, payload, timeout=120):
    r = urllib.request.urlopen(urllib.request.Request(
        f"http://localhost:9741{path}", data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"}), timeout=timeout)
    return json.load(r)


def chat(system, user, max_tokens=400):
    out = post("/v1/chat/completions", {"model": CHAT_MODEL, "temperature": 0.1,
               "max_tokens": max_tokens,
               "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]})
    return out["choices"][0]["message"]["content"].strip()


def embed(text):
    return post("/v1/embeddings", {"model": EMBED_MODEL, "input": text})["data"][0]["embedding"]


# ── fn_vecs purpose index (subject resolution) ──
side = json.load(open(f"{DATA}/specs/_fn_vecs/commonwealth-ai.json"))
FNS, DIM = side["fns"], side["dim"]
raw = open(f"{DATA}/specs/_fn_vecs/commonwealth-ai.bin", "rb").read()
VECS = [struct.unpack_from(f"<{DIM}f", raw, i * DIM * 4) for i in range(len(FNS))]
VNORM = [sum(x * x for x in v) ** 0.5 or 1.0 for v in VECS]


def resolve_subject(desc, k=4):
    q = embed(desc)
    qn = sum(x * x for x in q) ** 0.5 or 1.0
    sims = sorted(range(len(FNS)),
                  key=lambda i: -sum(a * b for a, b in zip(q, VECS[i])) / (VNORM[i] * qn))[:k]
    return [FNS[i] for i in sims]


def fn_name(f):
    return f.get("name") or f.get("symbol") or ""


# ── graph recall: shallow call-neighborhood of the seed functions ──
def seeds_for(names):
    # substring-match substantial names (resolution lands in the right area but near-misses the
    # exact entry, e.g. 'code_query' should seed handle_code_query); over-seeding is safe — the
    # affirmative-contradiction rule guards precision. Short/generic names stay exact.
    out = []
    for n in names:
        pat = f"%{n}%()." if len(n) >= 6 else f"%]{n}()."
        rows = cur.execute("SELECT DISTINCT caller_qualified FROM refs WHERE caller_qualified LIKE ? LIMIT 30", (pat,)).fetchall()
        out += [q for (q,) in rows]
    return out


def neighborhood(seeds, depth=2):
    seen, frontier = set(seeds), list(seeds)
    for _ in range(depth):
        nxt = []
        for i in range(0, len(frontier), 900):
            c = frontier[i:i+900]; qm = ",".join("?" * len(c))
            for (cal,) in cur.execute(
                    f"SELECT DISTINCT callee_qualified FROM refs WHERE caller_qualified IN ({qm}) AND callee_qualified<>''", c):
                if cal not in seen:
                    seen.add(cal)
                    if cal.rstrip().endswith("()."):
                        nxt.append(cal)
        frontier = nxt
    short = lambda q: re.split(r"[\]/ ]", q.rstrip().rstrip(".").rstrip("()"))[-1]
    return sorted({short(s) for s in seen if s.rstrip().endswith("().")})


def fetch_slice(name, keys, max_lines=20):
    row = cur.execute("SELECT file_path,line_start,line_end FROM symbols WHERE name=? ORDER BY (line_end-line_start) DESC LIMIT 1", (name,)).fetchone()
    if not row:
        return None
    f, a, b = row
    path = f if os.path.isabs(f) else os.path.join(REPO, f)
    try:
        lines = open(path).read().splitlines()[a-1:b]
    except OSError:
        return None
    hit = [j for j, ln in enumerate(lines) if any(k in ln.lower() for k in keys)]
    if hit:
        lo, hi = max(0, hit[0]-2), min(len(lines), hit[-1]+3)
        return "\n".join(lines[:1] + ["    // …"] + lines[lo:hi][:max_lines])
    return "\n".join(lines[:max_lines])


# ── the four steps per claim ──
EXTRACT_SYS = ('Turn a spec claim into a code-checkable question. A claim is "wiring" when it asserts '
               'that a specific code PATH performs a specific behavior (exposes / uses / calls / routes / '
               'enables / invokes a capability) — the kind you verify by inspecting what that path DOES, '
               'not by finding a function whose name matches. Output JSON only: '
               '{"wiring": true|false, "subject": "<describe the code path by the REQUEST or SITUATION it '
               'handles (the claim\'s when/for/scope trigger), like a handler\'s one-line summary — i.e. the '
               'ENTRY POINT that would perform the behavior, NOT the behavior the claim asserts. Example: for '
               '\'the checkout flow emails a receipt\', subject=\'handles a checkout request\' (the entry), never '
               '\'emails a receipt\' (the asserted behavior)>", '
               '"question": "<a yes/no question about what that path does>", "expected": "YES"|"NO"}. '
               'expected = what the claim asserts the answer is. wiring=false for claims not about one path\'s behavior.')

JUDGE_SYS = ("You read several functions from ONE code path and answer what the path does, using ONLY the "
             "code shown.\n"
             "VERDICT=YES  + quote a line, if the code shows the path DOES it.\n"
             "VERDICT=CONTRADICTED + quote the EXACT line, only if the code AFFIRMATIVELY does the opposite "
             "(a flag/config/statement that prevents or negates it). Never for mere absence.\n"
             "VERDICT=ABSENT, if the shown code does not settle it either way.")


def extract(claim):
    raw = chat(EXTRACT_SYS, f"CLAIM: {claim['statement']}\nCONDITIONS: {claim.get('conditions', [])}", 300)
    m = re.search(r"\{.*\}", raw, re.S)
    return json.loads(m.group(0)) if m else None


def judge(question, blocks):
    ans = chat(JUDGE_SYS,
               f"The path calls these functions. Question: {question} "
               "Inspect how any function builds the model request / config that settles it.\n\n"
               + "\n\n".join(blocks) +
               "\n\nAnswer: VERDICT=<YES|CONTRADICTED|ABSENT>, then cite the function + exact line.", 400)
    m = re.search(r"VERDICT\s*=\s*(YES|CONTRADICTED|ABSENT)", ans, re.I)
    return (m.group(1).upper() if m else "ABSENT"), ans


def run_claim(claim):
    ex = extract(claim)
    if not ex or not ex.get("wiring"):
        return {"verdict": "ABSTAIN", "why": "not a wiring claim" if ex else "extract-fail"}
    cands = resolve_subject(ex["subject"], k=6)
    seeds = seeds_for([fn_name(f) for f in cands])
    if not seeds:
        return {"verdict": "ABSTAIN", "why": f"subject '{ex['subject']}' unresolved", "ex": ex}
    hood = neighborhood(seeds)
    keys = [w for w in re.findall(r"[a-z_]{4,}", ex["question"].lower()) if w not in STOP][:6] or ["request"]
    blocks, picked = [], []
    for n in hood:
        if len(picked) >= 10:
            break
        s = fetch_slice(n, keys)
        if s and s.count("\n") >= 4:
            picked.append(n); blocks.append(f"### fn {n}\n{s}")
    if not blocks:
        return {"verdict": "ABSTAIN", "why": "no bodies", "ex": ex}
    actual, raw = judge(ex["question"], blocks)
    if actual == "ABSENT":       # evidence not in the neighborhood — a recall miss, NOT drift
        return {"verdict": "UNVERIFIED", "ex": ex, "actual": actual, "cite": raw[:120], "fed": len(blocks)}
    does_it = actual == "YES"    # CONTRADICTED => affirmatively does the opposite
    match = does_it == (ex["expected"].upper() == "YES")
    v = "CORROBORATED" if match else "DRIFT"
    return {"verdict": v, "ex": ex, "actual": actual, "cite": raw.split("\n", 1)[-1][:160], "fed": len(blocks)}


# ── drive over the frozen claims + score vs the verified key ──
claims_doc = json.load(open(f"{DATA}/specs/commonwealth-ai/CODE_INTEL_CHAT/claims.json"))
if isinstance(claims_doc, dict) and "sections" in claims_doc:
    claims = [c for sec in claims_doc["sections"] for c in sec.get("claims", [])]
elif isinstance(claims_doc, dict):
    claims = claims_doc.get("claims", [])
else:
    claims = claims_doc
key = json.load(open(f"{HERE}/answer_key.CODE_INTEL_CHAT.json"))["labels"]

print(f"running the wiring-drift check over {len(claims)} claims…\n" + "=" * 72)
results = []
for c in claims:
    r = run_claim(c)
    gt = key.get(c["statement"], {}).get("expect", "?")
    results.append((c["statement"], r, gt))
    tag = r["verdict"]
    flag = ""
    if tag == "DRIFT" and gt not in ("drift",):
        flag = "  ⚠ FALSE-DRIFT"
    if tag == "DRIFT" and gt == "drift":
        flag = "  ✓ caught"
    print(f"[{tag:12}] key={gt:12} {c['statement'][:60]}{flag}")
    if "ex" in r and tag != "ABSTAIN":
        print(f"      subj={r['ex'].get('subject','')[:50]!r} Q={r['ex'].get('question','')[:60]!r} exp={r['ex'].get('expected')}")
    if r.get("cite"):
        print(f"      actual={r.get('actual','?')} cite={r['cite'][:100]}")

print("=" * 72)
opined = [(s, r, gt) for s, r, gt in results if r["verdict"] in ("DRIFT", "CORROBORATED")]
drift = [(s, r, gt) for s, r, gt in results if r["verdict"] == "DRIFT"]
false_drift = [x for x in drift if x[2] != "drift"]
caught = [x for x in drift if x[2] == "drift"]
key_drifts = [s for s, lab in key.items() if lab.get("expect") == "drift"]
print(f"opined (drift|corrob): {len(opined)}   abstained: {len(results)-len(opined)}")
print(f"DRIFT recall:   {len(caught)}/{len(key_drifts)} key drifts caught")
print(f"DRIFT precision: {len(caught)}/{len(drift)} reported drifts real   <-  FALSE-DRIFT (trust-killer) = {len(false_drift)}")
for s, r, gt in false_drift:
    print(f"   ⚠ false-drift: key={gt} | {s[:60]} | {r.get('cite','')[:80]}")
