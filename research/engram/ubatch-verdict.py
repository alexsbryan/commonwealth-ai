#!/usr/bin/env python3
"""Apply the PRE-REGISTERED bars to the n_ubatch sweep. Written before the data.
Bars: research/engram/PRE-REGISTRATION-ubatch.md"""
import json, sys, pathlib
R = pathlib.Path("/home/alexbryan/dev/commonwealth-ai/research/engram")
try:
    rows = json.loads((R/"ubatch-sweep.json").read_text())
except Exception as e:
    print(f"VOID — could not read sweep json: {e}"); sys.exit(3)

def pick(rows, kind):
    out = {}
    for r in rows:
        ub = r.get("n_ubatch")
        npr, ngen = r.get("n_prompt", 0), r.get("n_gen", 0)
        if kind == "pp" and npr and not ngen: out[ub] = (r.get("avg_ts"), r.get("stddev_ts"), r.get("samples_ns") or [])
        if kind == "tg" and ngen and not npr: out[ub] = (r.get("avg_ts"), r.get("stddev_ts"), r.get("samples_ns") or [])
    return out
pp, tg = pick(rows,"pp"), pick(rows,"tg")
if not pp: print("VOID — no prefill (pp) rows in output"); sys.exit(3)

gtt = {}
try:
    lines = (R/"ubatch-sweep.gtt.tsv").read_text().splitlines()[1:]
    peak = max(int(l.split("\t")[1]) for l in lines if l.strip())
    base = min(int(l.split("\t")[1]) for l in lines if l.strip())
    gtt = {"peak": peak, "base": base, "delta_gib": (peak-base)/1024}
except Exception: gtt = None

print("=== n_ubatch sweep ===")
print(f"{'ub':>6} {'prefill tok/s':>16} {'gen tok/s':>14}")
for ub in sorted(pp):
    p = pp[ub][0]; g = tg.get(ub,(None,))[0]
    print(f"{ub:>6} {p:>16.1f} {('%.2f'%g) if g else '—':>14}")

base_ub = min(pp)
best_ub = max(pp, key=lambda u: pp[u][0])
gain = pp[best_ub][0]/pp[base_ub][0]
print(f"\nbest ub={best_ub}  gain vs ub={base_ub}: {gain:.2f}x")

# P2 control: generation must not move >10%
verdict = None
if tg and len(tg) > 1:
    gs = [v[0] for v in tg.values() if v[0]]
    spread = (max(gs)-min(gs))/min(gs) if gs else 0
    print(f"CONTROL tg spread across ub: {100*spread:.1f}% (bar: <=10%)")
    if spread > 0.10:
        verdict = f"VOID — generation moved {100*spread:.1f}% across ubatch; n_ubatch should not affect single-token decode, so the run is confounded."
if gtt: print(f"GTT: base {gtt['base']} MiB, peak {gtt['peak']} MiB, delta {gtt['delta_gib']:.2f} GiB")

if verdict is None:
    d = gtt["delta_gib"] if gtt else 99
    if gain >= 2.0 and d <= 4.0:
        verdict = f"WIN — {gain:.2f}x prefill at ub={best_ub}, GTT +{d:.2f} GiB. Recommend SOVEREIGN_N_UBATCH={best_ub}; DECLARE it in quality/env-flags.toml (currently an undeclared env read)."
    elif gain >= 1.5 and d <= 8.0:
        verdict = f"PARTIAL — {gain:.2f}x prefill at ub={best_ub}, GTT +{d:.2f} GiB. Opt-in only, never a default; name the memory cost."
    elif gain < 1.5:
        verdict = (f"NO-GO — only {gain:.2f}x. The tall-skinny-matmul hypothesis is WRONG: prefill is bounded by "
                   f"something else (sparse indexer at top_k=2048, the CPU-resident engram sync, or Vulkan MoE kernel "
                   f"efficiency). Ship the negative result, ship NO flag.")
    else:
        verdict = f"PARTIAL/NO-GO boundary — gain {gain:.2f}x, GTT +{d:.2f} GiB exceeds the 8 GiB usability bar on a box with ~11.4 GiB headroom."
print("\nVERDICT: " + verdict)
