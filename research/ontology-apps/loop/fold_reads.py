"""fold_reads.py <corpus> <i,j> [--put | --take]: two recorded reads of each window folded into one answer.

Prototype of reading a window twice (determinism-prereg.md, step 2). For every window question in the corpus's
answer store with at least j+1 recordings, the i-th and j-th (store order, 0-based) are parsed and folded: B's
particulars map onto A's by type and a shared name, B's claims are added unless A holds the same kind about the
same particular with an overlapping quote. --put appends the folded answers to the store (model "fold", so the
latest recording of each window, which a replay gives, is the fold); --take removes every fold line again.
Prints, per window, what each read held and what the fold added.
"""
import json, os, re, sys
from collections import defaultdict

corpus, pair = sys.argv[1], sys.argv[2]
STORE = os.path.expanduser(f"~/.svrnmesh/enrichment/{corpus}/answers.jsonl")
lines = [json.loads(l) for l in open(STORE) if l.strip()]

if "--take" in sys.argv:
    kept = [l for l in lines if l.get("model") != "fold"]
    with open(STORE, "w") as f:
        f.writelines(json.dumps(l) + "\n" for l in kept)
    sys.exit(print(f"removed {len(lines) - len(kept)} fold lines"))

i, j = map(int, pair.split(","))
by_key = defaultdict(list)
for l in lines:
    if l.get("phase") == "document_window" and l.get("model") != "fold":
        by_key[l["key"]].append(l["answer"])


def parse(text):
    text = re.sub(r"(?s)^.*</think>", "", text).strip()
    return json.loads(text[text.index("{"):text.rindex("}") + 1])


norm = lambda s: re.sub(r"\s+", " ", (s or "").lower()).strip()
words = lambda s: set(re.findall(r"[a-z0-9']+", norm(s)))


def names(p):
    return {norm(p.get("name"))} | {norm(n) for n in p.get("other_names") or []} - {""}


def same_quote(a, b):
    a, b = norm(a), norm(b)
    if not a or not b: return False
    if a in b or b in a: return True
    wa, wb = words(a), words(b)
    return len(wa & wb) / max(1, len(wa | wb)) >= 0.6


def by_evidence(p, A, B):
    """The A particular B's p is, by what both reads cite about it: every B claim about p that A also holds
    (same kind, overlapping quote) is about one A particular of p's type. None when no claim decides it."""
    hits = {a.get("about") for c in B.get("claims", []) if c.get("about") == p["ref"]
            for a in A.get("claims", []) if a.get("kind") == c.get("kind") and same_quote(a.get("quote"), c.get("quote"))}
    typed = {r for r in hits if any(x["ref"] == r and x.get("type") == p.get("type") for x in A.get("particulars", []))}
    return next(iter(typed)) if len(typed) == 1 else None


def fold(A, B):
    parts = [dict(p) for p in A.get("particulars", [])]
    refs = {p["ref"] for p in parts}
    remap, added_parts = {}, 0
    for p in B.get("particulars", []):
        hit = next((a for a in parts if a.get("type") == p.get("type") and names(a) & names(p)), None)
        if not hit and (r := by_evidence(p, A, B)):
            hit = next(a for a in parts if a["ref"] == r)
        if hit:
            remap[p["ref"]] = hit["ref"]
            hit["other_names"] = sorted(set(hit.get("other_names") or []) | (names(p) - names(hit)))
        else:
            new = f"b{p['ref']}"
            while new in refs: new += "b"
            remap[p["ref"]], refs = new, refs | {new}
            parts.append({**p, "ref": new}); added_parts += 1
    re_ref = lambda v: remap.get(v, v) if isinstance(v, str) else [remap.get(x, x) for x in v] if isinstance(v, list) else v
    claims = [dict(c) for c in A.get("claims", [])]
    added = 0
    for c in B.get("claims", []):
        c = {**c, "about": re_ref(c.get("about")), "fields": {k: re_ref(v) for k, v in (c.get("fields") or {}).items()}}
        if any(a.get("kind") == c.get("kind") and a.get("about") == c.get("about") and same_quote(a.get("quote"), c.get("quote"))
               for a in claims):
            continue
        claims.append(c); added += 1
    return {**A, "particulars": parts, "claims": claims}, added, added_parts


out = []
for key, answers in by_key.items():
    if len(answers) <= max(i, j): continue
    A, B = parse(answers[i]), parse(answers[j])
    F, added, added_parts = fold(A, B)
    print(f"{key[:12]}  claims A {len(A.get('claims', []))} B {len(B.get('claims', []))} fold {len(F['claims'])} (+{added} from B)"
          f"  particulars A {len(A.get('particulars', []))} fold {len(F['particulars'])} (+{added_parts})")
    out.append({"key": key, "port": "chat", "phase": "document_window", "model": "fold", "answer": json.dumps(F)})

if "--put" in sys.argv:
    with open(STORE, "a") as f:
        f.writelines(json.dumps(l) + "\n" for l in out)
    print(f"appended {len(out)} folded answers to {STORE}")
