#!/usr/bin/env python3
"""Measure delta -- how detectable is damage caused by a refinement pass?

§9 established: a refinement loop converges iff the damage it causes is
detectable by the same verifier that causes it. Precision governs the first
pass; delta governs the limit. FA=20% turns destructive below delta ~= 0.5.
delta was SWEPT there, never measured. This measures it.

THE EXPERIMENT
  1. Take grounded claims the verifier FALSE-ALARMS on (rung-1000 score <= tau
     at FA=20%): 273 items on the decontaminated bank. These are exactly the
     claims a refinement loop would act on and could only damage.
  2. Revise each under the gate's OWN instruction ("not supported -- revise so
     it is supported"). Model-generated damage, not injected corruption. That
     distinction is the whole reason delta is not 1.0.
  3. Label damage MECHANICALLY, no judge: reuse control_mine.asserted_values --
     name runs, quoted strings and numbers asserted by the revision but absent
     (normalized) from the evidence. Sound, not complete.
  4. Re-judge every revision with rung-1000 under the SAME protocol the curve
     was measured with (no-think + grammar, p_grounded from branch_prob).
  5. delta = P(verifier flags the revision | revision is mechanically damaged).

PRE-REGISTERED (written before any revision was generated)
  Prediction: delta in [0.4, 0.7] -- revision damage is subtler than an
    injected entity swap, but rung-1000 catches injected corruption at 93%.
    This lands near the decision boundary, so the CI is expected to matter.
  Decision bars (n=273 gives ~+/-6pt at delta=0.5):
    CI lower bound > 0.5  -> FA=20% is SAFE with iteration depth. Aggressive
                             operating points are viable on a mesh that makes
                             passes cheap.
    CI upper bound < 0.5  -> FA=20% is DESTRUCTIVE even with depth. The gate
                             must run conservative regardless of node count.
    CI straddles 0.5      -> underpowered; report as could-not-judge (§18.1),
                             do not pick the side that flatters the story.
  Reported alongside, not folded in: p_damage (witness fire rate) and the
  unconditional re-flag rate, which needs no damage label at all.

BIAS, STATED: the mechanical witness sees value-anchored damage (names,
numbers, quotes) and is blind to relational damage (negation). Value-anchored
damage is plausibly EASIER for the verifier to catch too, so this delta is
likely an OVER-estimate -- which makes a low result decisive and a high result
soft. Named here so the asymmetry is not discovered later.
"""
import argparse, json, re, sys, unicodedata, urllib.request, concurrent.futures as cf
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
from control_mine import asserted_values, norm   # the EXISTING witness (§19)

REVISE_SYSTEM = "You revise claims to be strictly supported by the given evidence. Output ONLY the revised claim, one sentence, no preamble."
REVISE_PROMPT = (
    "EVIDENCE:\n{evidence}\n\n"
    "CLAIM (judged NOT SUPPORTED by the evidence):\n{claim}\n\n"
    "Revise the claim so that it is fully supported by the evidence above. "
    "Output only the revised claim."
)

# HEADROOM_STUDY's own denominator: FA is measured on grounded EXCLUDING
# multi_hop_conjunction (its "excl. multi_hop, n=965"), because the gate
# structurally fails cross-chunk synthesis (~99.5% FA) and folding that into
# the operating curve lets one known architectural defect set tau for
# everything else. Reproduced exactly: 965.
EXCLUDE_KINDS = {"multi_hop_conjunction"}

def load_items(fa_target):
    sc = {json.loads(l)["id"]: json.loads(l) for l in open(ROOT/"runs/headroom/scored.jsonl") if l.strip()}
    bk = {json.loads(l)["id"]: json.loads(l) for l in open(ROOT/"data/heldout-sep/bank.jsonl") if l.strip()}
    grounded = [i for i in sc if sc[i]["label"] == "grounded"
                and sc[i]["kind"] not in EXCLUDE_KINDS]
    gs = sorted(sc[i]["our_margin"] for i in grounded)
    tau = gs[max(0, int(round(fa_target*len(gs)))-1)]
    fa = [i for i in grounded if sc[i]["our_margin"] <= tau]
    out = []
    for i in fa:
        b = bk[i]
        out.append({"id": i, "kind": b["kind"], "claim": b["claim"],
                    "question": b.get("question", ""),
                    "evidence_chunks": b["evidence_chunks"],
                    "orig_margin": sc[i]["our_margin"]})
    return out, tau

def revise(item, url, model, timeout=180):
    ev = "\n\n".join(item["evidence_chunks"])[:6000]
    body = json.dumps({
        "model": model,
        "oicp": {"oicp_version": "0.4.0", "privacy": {"sharding": "local_only"}},
        "messages": [{"role": "system", "content": REVISE_SYSTEM},
                     {"role": "user", "content": REVISE_PROMPT.format(evidence=ev, claim=item["claim"])}],
        "max_tokens": 220, "temperature": 0.0, "think_budget": 0,
        "chat_template_kwargs": {"enable_thinking": False},
    }).encode()
    req = urllib.request.Request(f"{url}/chat/completions", data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        out = json.load(r)
    return out["choices"][0]["message"]["content"].strip()

def witness(revised, evidence_chunks, question):
    """Mechanically damaged iff some asserted value is absent from the evidence."""
    ev = norm(" ".join(evidence_chunks))
    vals = asserted_values(revised, question)
    absent = [v for v, nv in vals if nv not in ev]
    return {"n_values": len(vals), "absent": absent, "damaged": len(absent) > 0,
            "checkable": len(vals) > 0}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fa", type=float, default=0.20)
    ap.add_argument("--url", default="http://127.0.0.1:9741/v1")
    ap.add_argument("--model", default="Qwen3.5-4B-UD-MTP-Q6_K_XL")
    ap.add_argument("--concurrency", type=int, default=2, help="kept low: the box is shared")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--out", default=str(ROOT/"runs/delta/revisions.jsonl"))
    a = ap.parse_args()

    items, tau = load_items(a.fa)
    if a.limit: items = items[:a.limit]
    print(f"FA={a.fa:.0%} tau={tau:+.3f} -> {len(items)} false-alarm items", flush=True)
    Path(a.out).parent.mkdir(parents=True, exist_ok=True)

    done, fails = 0, 0
    with open(a.out, "w") as f, cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
        futs = {ex.submit(revise, it, a.url, a.model): it for it in items}
        for fut in cf.as_completed(futs):
            it = futs[fut]
            try:
                rev = fut.result()
            except Exception as e:
                fails += 1
                # an error is recorded, never defaulted to a success shape (§18.3)
                f.write(json.dumps({**it, "revised": None, "error": str(e)[:200]})+"\n")
                continue
            w = witness(rev, it["evidence_chunks"], it["question"])
            f.write(json.dumps({**it, "revised": rev, "witness": w})+"\n")
            done += 1
            if done % 25 == 0:
                print(f"  {done}/{len(items)} revised ({fails} failed)", flush=True)
    print(f"DONE revised={done} failed={fails} -> {a.out}", flush=True)
    if fails:
        print(f"NOTE: {fails} revision(s) errored and are recorded with error set; "
              f"they are excluded from any rate's denominator, never counted as undamaged.")

if __name__ == "__main__":
    sys.exit(main())
