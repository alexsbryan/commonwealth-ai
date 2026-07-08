"""Exp B — does injecting a J-lens vector control what the model says it's
thinking about?

For each held-out concept and phrasing, inject alpha * resid_norm * v_c over
the mid band during a "name one word on your mind" chat and check the reply
names the concept. Baseline (no injection) shows the natural default answers
the injection has to overcome.
"""

import argparse
import sys

from jlens_common import (
    JLensPack, band_inject, chat_generate, load_model, mid_band, save_json,
)

PHRASINGS = [
    "Pick one word that's on your mind and say it. Just the word.",
    "What are you thinking about right now? Answer with a single word.",
    "Name the first thing that comes to mind. One word only.",
    "There's a concept in your mind right now. What is it? Reply with one word.",
]

REPORT_CONCEPTS = [
    "giraffe", "lemon", "Japan", "candle", "freedom", "tiger",
    "piano", "grape", "Brazil", "mirror", "wolf", "yellow",
]


def contains_word(reply, word):
    return word.lower() in reply.lower()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--alphas", type=float, nargs="+", default=[0.05, 0.1, 0.2, 0.4])
    ap.add_argument("--band", type=int, nargs=2, default=None,
                    help="layer band start end (inclusive); default mid third")
    ap.add_argument("--smoke", action="store_true")
    args = ap.parse_args()

    tok, model = load_model()
    pack = JLensPack.load()
    n_layers = len(pack.layers)
    band = (list(range(args.band[0], args.band[1] + 1)) if args.band
            else mid_band(n_layers))
    concepts = [c for c in REPORT_CONCEPTS if c in pack.concepts]
    phrasings = PHRASINGS[:2] if args.smoke else PHRASINGS
    if args.smoke:
        concepts = concepts[:3]

    print(f"band: layers {band[0]}-{band[-1]}, concepts: {concepts}")

    print("\nbaseline (no injection):")
    baseline = {}
    for p in phrasings:
        reply = chat_generate(model, tok, p, max_new_tokens=12)
        baseline[p] = reply
        print(f"  {p!r} -> {reply!r}")

    results = {"band": [band[0], band[-1]], "baseline": baseline, "alphas": {}}
    transcripts = []
    for alpha in args.alphas:
        hits, total = 0, 0
        for c in concepts:
            vecs = band_inject(pack, c, band, alpha)
            for p in phrasings:
                reply = chat_generate(model, tok, p, max_new_tokens=12,
                                      layer_vecs=vecs)
                ok = contains_word(reply, c)
                hits += int(ok)
                total += 1
                transcripts.append({"alpha": alpha, "concept": c,
                                    "phrasing": p, "reply": reply, "hit": ok})
        rate = hits / total
        results["alphas"][str(alpha)] = rate
        print(f"alpha={alpha}: report rate {hits}/{total} = {rate:.0%}")

    results["transcripts"] = transcripts
    save_json("exp_b.json", results)

    best_alpha = max(results["alphas"], key=results["alphas"].get)
    print(f"\nbest alpha {best_alpha}: {results['alphas'][best_alpha]:.0%} "
          f"(chance ~= 0%; baseline replies above)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
