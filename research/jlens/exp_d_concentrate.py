"""Exp D — directed modulation: can the model hold an instructed concept in
the workspace during an unrelated task, and can that state be distilled into
an additive vector that works WITHOUT the instruction?

This is the Phase 1 bridge: the distilled per-layer mean-difference vector is
exactly what we would ship as a llama.cpp control vector (there the concept
is "the provided evidence" instead of a giraffe).
"""

import argparse
import sys

import torch

from jlens_common import (
    Injector, JLensPack, generate_with_resids, load_model, mid_band,
    readout_z, save_json,
)

TASKS = [
    "Briefly explain how yeast makes bread rise.",
    "What causes ocean tides? Two sentences.",
    "Explain how a zipper works in two sentences.",
]

CONCEPTS = ["giraffe", "lemon", "Japan", "candle", "freedom", "tiger"]

INSTRUCTION = ("While you answer, silently keep thinking about {c}. "
               "Do not mention it in your answer.")


def mean_z(pack, layer, resids):
    if resids.shape[0] == 0:
        return 0.0
    zs = torch.stack([readout_z(pack, layer, h) for h in resids])
    return zs.mean(dim=0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--band", type=int, nargs=2, default=None)
    ap.add_argument("--max-new", type=int, default=40)
    ap.add_argument("--smoke", action="store_true")
    args = ap.parse_args()

    tok, model = load_model()
    pack = JLensPack.load()
    n_layers = len(pack.layers)
    band = (list(range(args.band[0], args.band[1] + 1)) if args.band
            else mid_band(n_layers))
    concepts = [c for c in CONCEPTS if c in pack.concepts]
    tasks = TASKS[:1] if args.smoke else TASKS
    if args.smoke:
        concepts = concepts[:2]

    results = {"band": [band[0], band[-1]], "concepts": {}}
    distilled = {}  # concept -> {layer: [H] delta}

    for c in concepts:
        ci = pack.concept_index(c)
        z_instr, z_base, leaked = [], [], 0
        deltas = {l: [] for l in band}
        for t in tasks:
            _, r_base = generate_with_resids(
                model, tok, t, layers=band, max_new_tokens=args.max_new)
            text_i, r_instr = generate_with_resids(
                model, tok, t, system=INSTRUCTION.format(c=c), layers=band,
                max_new_tokens=args.max_new)
            leaked += int(c.lower() in text_i.lower())
            for l in band:
                z_instr.append(float(mean_z(pack, l, r_instr[l])[ci]))
                z_base.append(float(mean_z(pack, l, r_base[l])[ci]))
                n = min(r_base[l].shape[0], r_instr[l].shape[0])
                if n > 0:
                    deltas[l].append(
                        (r_instr[l][:n].mean(dim=0) - r_base[l][:n].mean(dim=0)))
        zi, zb = sum(z_instr) / len(z_instr), sum(z_base) / len(z_base)
        distilled[c] = {l: torch.stack(v).mean(dim=0) for l, v in deltas.items()}

        # distillation sanity: inject the delta (no instruction), re-measure
        z_dist = []
        for t in tasks:
            _, r_d = generate_with_resids(
                model, tok, t, layers=band, max_new_tokens=args.max_new,
                layer_vecs=distilled[c])
            for l in band:
                z_dist.append(float(mean_z(pack, l, r_d[l])[ci]))
        zd = sum(z_dist) / len(z_dist)

        results["concepts"][c] = {
            "z_instructed": zi, "z_baseline": zb, "z_distilled": zd,
            "hold_delta": zi - zb, "distill_delta": zd - zb,
            "leaked_mentions": leaked,
        }
        print(f"{c:>8}: z base {zb:+.2f} | instructed {zi:+.2f} "
              f"(hold {zi-zb:+.2f}) | distilled-vector {zd:+.2f} "
              f"(distill {zd-zb:+.2f}) | leaked {leaked}/{len(tasks)}")

    holds = [v["hold_delta"] for v in results["concepts"].values()]
    dists = [v["distill_delta"] for v in results["concepts"].values()]
    results["mean_hold_delta"] = sum(holds) / len(holds)
    results["mean_distill_delta"] = sum(dists) / len(dists)
    print(f"\nmean hold delta {results['mean_hold_delta']:+.2f}, "
          f"mean distill delta {results['mean_distill_delta']:+.2f} "
          f"(both >> 0 = GO for Phase 1)")
    save_json("exp_d.json", results)
    return 0


if __name__ == "__main__":
    sys.exit(main())
