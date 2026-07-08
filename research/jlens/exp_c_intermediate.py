"""Exp C — unverbalized reasoning intermediates: visible and swappable.

Two-hop questions whose answer requires an intermediate concept that is never
stated. Test (1) the intermediate is readable at mid layers from the prompt's
final position, and (2) steering the residual from the intermediate toward a
different concept (h += a*(v_swap - v_base)) redirects the final answer to the
swap-consistent one.
"""

import argparse
import sys

from jlens_common import (
    DEVICE, DTYPE, Injector, JLensPack, capture_resids, chat_generate,
    chat_prompt, load_model, mid_band, readout_z, save_json,
)

ITEMS = [
    {
        "q": "Think of the animal that spins webs to catch insects. How many legs does it have? Answer with just a number.",
        "base": "spider", "swap": "dog",
        "base_ok": ["8", "eight"], "swap_ok": ["4", "four"],
    },
    {
        "q": "What language do they speak in the country famous for the Eiffel Tower? Answer with one word.",
        "base": "France", "swap": "Japan",
        "base_ok": ["french"], "swap_ok": ["japanese"],
    },
    {
        "q": "What color is the curved fruit that monkeys famously eat? Answer with one word.",
        "base": "banana", "swap": "cherry",
        "base_ok": ["yellow"], "swap_ok": ["red"],
    },
    {
        "q": "What currency is used in the country where Mount Fuji is? Answer with one word.",
        "base": "Japan", "swap": "France",
        "base_ok": ["yen"], "swap_ok": ["euro"],
    },
    {
        "q": "On which continent does the flightless black-and-white bird that huddles in colonies live? Answer with one word.",
        "base": "penguin", "swap": "lion",
        "base_ok": ["antarctica"], "swap_ok": ["africa"],
    },
    {
        "q": "How many legs does the animal that purrs and chases mice have? Answer with just a number.",
        "base": "cat", "swap": "spider",
        "base_ok": ["4", "four"], "swap_ok": ["8", "eight"],
    },
]


def swap_vecs(pack, base, swap, layers, alpha):
    out = {}
    for l in layers:
        delta = pack.vec(l, swap) - pack.vec(l, base)
        out[l] = delta * (alpha * pack.resid_norm[l])
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--alphas", type=float, nargs="+", default=[0.1, 0.2, 0.4])
    ap.add_argument("--band", type=int, nargs=2, default=None)
    args = ap.parse_args()

    tok, model = load_model()
    pack = JLensPack.load()
    n_layers = len(pack.layers)
    band = (list(range(args.band[0], args.band[1] + 1)) if args.band
            else mid_band(n_layers))
    items = [it for it in ITEMS
             if it["base"] in pack.concepts and it["swap"] in pack.concepts]

    # (1) intermediate visibility at the prompt's last position
    print("intermediate visibility (rank of base concept, 0=best):")
    vis = []
    for it in items:
        text = chat_prompt(tok, it["q"])
        resids = capture_resids(model, tok, text, band)
        best_rank, best_layer = 10 ** 9, None
        for l in band:
            z = readout_z(pack, l, resids[l][-1])
            rank = int((z > z[pack.concept_index(it["base"])]).sum().item())
            if rank < best_rank:
                best_rank, best_layer = rank, l
        vis.append({"item": it["base"], "best_rank": best_rank,
                    "best_layer": best_layer})
        print(f"  {it['base']:>8}: rank {best_rank} @ layer {best_layer}")

    # (2) baseline answers + swap steering
    results = {"band": [band[0], band[-1]], "visibility": vis, "runs": []}
    base_correct = 0
    for it in items:
        reply = chat_generate(model, tok, it["q"], max_new_tokens=12)
        ok = any(a in reply.lower() for a in it["base_ok"])
        base_correct += int(ok)
        results["runs"].append({"item": it["base"], "mode": "baseline",
                                "reply": reply, "ok": ok})
        print(f"baseline {it['base']:>8}: {reply!r} ({'ok' if ok else 'MISS'})")

    for alpha in args.alphas:
        swapped = 0
        for it in items:
            vecs = swap_vecs(pack, it["base"], it["swap"], band, alpha)
            reply = chat_generate(model, tok, it["q"], max_new_tokens=12,
                                  layer_vecs=vecs)
            ok = any(a in reply.lower() for a in it["swap_ok"])
            swapped += int(ok)
            results["runs"].append({"item": it["base"], "mode": f"swap@{alpha}",
                                    "reply": reply, "ok": ok})
            print(f"swap a={alpha} {it['base']:>8}->{it['swap']:<8}: {reply!r} "
                  f"({'REDIRECTED' if ok else 'no'})")
        print(f"alpha={alpha}: {swapped}/{len(items)} redirected")
        results[f"redirect_rate@{alpha}"] = swapped / len(items)

    results["baseline_correct"] = base_correct / len(items)
    save_json("exp_c.json", results)
    return 0


if __name__ == "__main__":
    sys.exit(main())
