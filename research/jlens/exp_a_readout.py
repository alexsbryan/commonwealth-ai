"""Exp A — derive J-lens vectors and check the readout sees implied concepts.

Each probe implies a concept that is never named and is not the likely next
token; a workspace-like representation should still make it readable at mid
layers. Chance top-1 is 1/|concepts| (~1.6%).
"""

import argparse
import sys

import torch

from jlens_common import (
    CONTEXTS, JLensPack, MODEL_ID, calibrate, capture_resids, contexts_hash,
    derive_jlens, load_model, readout_z, save_json, single_token_concepts,
)

# (implied concept, probe text). Concept word must not appear in the text.
PROBES = [
    ("elephant", "With its trunk raised high, the huge grey animal trumpeted across the savanna while the tourists"),
    ("spider", "It spun a silky web in the corner of the barn, waiting patiently for flies, and then"),
    ("dog", "It wagged its tail and fetched the stick, then growled at the mailman before the"),
    ("cat", "It purred on the windowsill and chased the laser dot around the living room until"),
    ("lion", "The maned beast roared across the savanna, scattering the antelope, while its pride"),
    ("whale", "The enormous marine mammal breached the surface and sprayed water from its blowhole before"),
    ("eagle", "Soaring on thermal currents, the majestic bird of prey scanned the valley for rabbits while"),
    ("snake", "It slithered through the grass, flicking its forked tongue, then coiled around a branch and"),
    ("banana", "The monkey peeled the long yellow fruit and ate it in three bites, tossing the"),
    ("lemon", "She squeezed the sour yellow citrus over the fish and the juice made her pucker as"),
    ("France", "Strolling past the Eiffel Tower with a baguette, he practiced ordering croissants in the local"),
    ("Japan", "They ate sushi near Mount Fuji and watched cherry blossoms fall over Tokyo before the"),
    ("Egypt", "The pyramids rose above the desert as boats drifted down the Nile toward Cairo, where"),
    ("piano", "She sat at the bench, lifted the lid over the black and white keys, and began"),
    ("hammer", "He drove the nail into the plank with three firm swings of the heavy tool, then"),
    ("bicycle", "She pedaled up the hill, shifting gears and ringing the little bell on the handlebars as"),
    ("money", "He counted the crumpled bills and coins from the jar, hoping there was enough for"),
    ("war", "The soldiers dug trenches as artillery thundered across the front lines, and the general ordered"),
    ("music", "The orchestra tuned their instruments as the conductor raised the baton and the first notes"),
    ("red", "The stop sign and the fire truck were painted the same bright warning color that"),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--smoke", action="store_true", help="tiny subsets, plumbing check")
    ap.add_argument("--batch-size", type=int, default=16)
    args = ap.parse_args()

    tok, model = load_model()
    words, token_ids = single_token_concepts(tok)
    dropped = [w for w in __import__("jlens_common").CONCEPT_WORDS if w not in words]
    print(f"concepts: {len(words)} single-token (dropped: {dropped})")

    contexts = CONTEXTS[:8] if args.smoke else CONTEXTS
    if args.smoke:
        words, token_ids = words[:10], token_ids[:10]

    print("deriving J-lens vectors ...")
    vectors = derive_jlens(model, tok, words, token_ids, contexts,
                           batch_size=args.batch_size)
    print("calibrating readout stats ...")
    mu, sd, rnorm = calibrate(model, tok, vectors, contexts,
                              batch_size=args.batch_size)

    n_layers = len(vectors)
    pack = JLensPack(model_id=MODEL_ID, concepts=words, token_ids=token_ids,
                     layers=list(range(n_layers)))
    pack.vectors, pack.calib_mu, pack.calib_sd = vectors, mu, sd
    pack.resid_norm, pack.ctx_hash = rnorm, contexts_hash()
    pack.save()
    print(f"pack saved ({n_layers} layers, {len(words)} concepts)")

    probes = PROBES[:4] if args.smoke else PROBES
    probes = [(w, t) for w, t in probes if w in words]
    layer_hits = {l: {"top1": 0, "top3": 0, "rank_sum": 0} for l in range(n_layers)}
    for target, text in probes:
        resids = capture_resids(model, tok, text, list(range(n_layers)))
        ti = pack.concept_index(target)
        for l in range(n_layers):
            z = readout_z(pack, l, resids[l][-1])
            rank = int((z > z[ti]).sum().item())  # 0 = best
            layer_hits[l]["top1"] += int(rank == 0)
            layer_hits[l]["top3"] += int(rank < 3)
            layer_hits[l]["rank_sum"] += rank

    n = len(probes)
    print(f"\nper-layer readout of implied concepts ({n} probes, {len(words)} concepts, chance top1={1/len(words):.1%}):")
    print(f"{'layer':>5} {'top1':>6} {'top3':>6} {'mean rank':>10}")
    results = []
    for l in range(n_layers):
        h = layer_hits[l]
        results.append({"layer": l, "top1": h["top1"] / n, "top3": h["top3"] / n,
                        "mean_rank": h["rank_sum"] / n})
        if l % 2 == 0 or h["top3"] / n >= 0.5:
            print(f"{l:>5} {h['top1']/n:>6.0%} {h['top3']/n:>6.0%} {h['rank_sum']/n:>10.1f}")

    best = max(results, key=lambda r: r["top3"])
    print(f"\nbest layer: {best['layer']} (top1 {best['top1']:.0%}, top3 {best['top3']:.0%})")
    save_json("exp_a.json", {"model": MODEL_ID, "n_probes": n,
                             "n_concepts": len(words), "per_layer": results,
                             "best_layer": best})
    return 0


if __name__ == "__main__":
    sys.exit(main())
