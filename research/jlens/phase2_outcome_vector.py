"""Phase 2 — outcome-contrast steering vector.

Contrast the model's internal state on trajectories that grounded correctly
vs ones that failed, ON THE SAME PROMPTS (within-item, sampled at
temperature). Unlike Phase 1's instruction contrast, the delta cannot
encode prompt style — both sides see identical prompts; only the sampled
outcome differs.

Stages (flags combine):
  --calibrate   difficulty sweep: per-item correct-rate distribution
  --derive      collect mixed-outcome items, build vector, export f32
  --readout     exploratory: does a gold-vs-distractor J-lens margin over
                answer tokens predict the sample's outcome within-item?
  --validate    held-out items, steered vs baseline, scale sweep

Speed: two-pass — batched cached generation (fast), then ONE no-grad
capture forward over each finished sequence for residuals.
"""

import argparse
import os
import sys
from collections import defaultdict

import torch

from jlens_common import (
    DEVICE, DTYPE, Injector, OUT_DIR, _Capture, chat_prompt,
    forward_with_capture, load_model, save_json,
)
from synth_items import make_item, render_user, score

BAND = list(range(20, 33))
DERIVE_SEEDS = range(1000, 1064)      # 64 candidate items
HELDOUT_SEEDS = range(5000, 5032)     # 32 validation items
HELDOUT_ABSENT_SEEDS = range(7000, 7010)
VEC_PATH = os.path.join(OUT_DIR, "outcome_contrast_qwen3-8b.f32")


def render_prompts(tok, items):
    return [chat_prompt(tok, render_user(it)) for it in items]


def batch_generate(model, tok, prompts, max_new=32, temperature=0.0,
                   seed=0, layer_vecs=None, batch_size=8):
    """Returns (texts, sequences list of 1-D tensors, prompt_lens list)."""
    torch.manual_seed(seed)
    texts, seqs, plens = [], [], []
    ctx = Injector(model, layer_vecs) if layer_vecs else _null()
    with torch.no_grad(), ctx:
        for start in range(0, len(prompts), batch_size):
            chunk = prompts[start:start + batch_size]
            enc = tok(chunk, return_tensors="pt", padding=True).to(DEVICE)
            do_sample = temperature > 0
            out = model.generate(
                **enc, max_new_tokens=max_new,
                do_sample=do_sample,
                temperature=temperature if do_sample else None,
                top_p=0.95 if do_sample else None,
                pad_token_id=tok.pad_token_id,
            )
            plen = enc.input_ids.shape[1]
            for row in out:
                gen = row[plen:]
                # trim at eos / pad
                keep = []
                for t in gen.tolist():
                    if t in (tok.eos_token_id, tok.pad_token_id):
                        break
                    keep.append(t)
                texts.append(tok.decode(keep, skip_special_tokens=True).strip())
                seqs.append(row[: plen + len(keep)])
                plens.append(plen)
            del enc, out
            if torch.backends.mps.is_available():
                torch.mps.empty_cache()  # MPS allocator never reuses odd-sized KV buffers
    return texts, seqs, plens


class _null:
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False


def answer_resids(model, seq, plen, layers):
    """One capture forward; residuals at answer-producing positions
    (plen-1 .. len-2). Returns {layer: [n_ans, H] float32 cpu}."""
    ids = seq.unsqueeze(0).to(DEVICE)
    with torch.no_grad(), _Capture(model) as cap:
        model(input_ids=ids, use_cache=False)
    lo, hi = plen - 1, ids.shape[1] - 1
    return {l: cap.resids[l][0, lo:hi, :].float().cpu() for l in layers}


def sample_items(model, tok, items, s_per_item, temperature, layer_vecs=None,
                 progress=print):
    """Generate S samples per item; returns list of
    (item, [(reply, verdict, seq, plen), ...])."""
    prompts = render_prompts(tok, items)
    results = [[] for _ in items]
    for s in range(s_per_item):
        texts, seqs, plens = batch_generate(
            model, tok, prompts, temperature=temperature, seed=1000 + s,
            layer_vecs=layer_vecs)
        for i, it in enumerate(items):
            results[i].append((texts[i], score(it, texts[i]), seqs[i], plens[i]))
        progress(f"  sample round {s + 1}/{s_per_item}")
    return list(zip(items, results))


def cmd_calibrate(model, tok, args):
    for n_d, two_hop in [(4, True), (6, True), (4, False)]:
        items = [make_item(s, n_distractors=n_d, two_hop=two_hop)
                 for s in list(DERIVE_SEEDS)[:16]]
        sampled = sample_items(model, tok, items, args.samples, args.temp,
                               progress=lambda *_: None)
        rates = []
        for it, runs in sampled:
            ok = sum(1 for _, v, _, _ in runs if v == "correct")
            rates.append(ok / len(runs))
        mixed = sum(1 for r in rates if 0.01 < r < 0.99)
        print(f"distractors={n_d} two_hop={two_hop}: "
              f"mean correct {sum(rates)/len(rates):.2f}, "
              f"mixed-outcome items {mixed}/{len(rates)}, "
              f"rates={[round(r,2) for r in rates]}")


SAMPLES_CACHE = os.path.join(OUT_DIR, "phase2_samples.pt")


def cmd_derive(model, tok, args):
    items = [make_item(s, n_distractors=args.distractors, confusable=True)
             for s in DERIVE_SEEDS]
    sampled = sample_items(model, tok, items, args.samples, args.temp)
    deltas = {l: [] for l in BAND}
    cache = []
    used = 0
    for it, runs in sampled:
        good = [(seq, plen) for _, v, seq, plen in runs if v == "correct"]
        bad = [(seq, plen) for _, v, seq, plen in runs if v == "wrong"]
        if not good or not bad:
            continue
        used += 1
        acc_g = {l: [] for l in BAND}
        acc_b = {l: [] for l in BAND}
        item_rows = []
        for _, v, seq, plen in runs:
            if v not in ("correct", "wrong"):
                continue
            r = answer_resids(model, seq, plen, BAND)
            if not r[BAND[0]].shape[0]:
                continue
            means = {l: r[l].mean(dim=0) for l in BAND}
            item_rows.append({"verdict": v, "means": means})
            for l in BAND:
                (acc_g[l] if v == "correct" else acc_b[l]).append(means[l])
        for l in BAND:
            if acc_g[l] and acc_b[l]:
                deltas[l].append(torch.stack(acc_g[l]).mean(dim=0)
                                 - torch.stack(acc_b[l]).mean(dim=0))
        cache.append({"item": it, "rows": item_rows})
    torch.save(cache, SAMPLES_CACHE)
    print(f"mixed-outcome items used: {used}/{len(items)} "
          f"(sample cache -> {SAMPLES_CACHE})")
    if used < 8:
        print("TOO FEW mixed items — adjust difficulty (--distractors/--temp)")
        return
    n_layers = model.config.num_hidden_layers
    n_embd = model.config.hidden_size
    buf = torch.zeros((n_layers - 1) * n_embd, dtype=torch.float32)
    norms = {}
    for l in BAND:
        v = torch.stack(deltas[l]).mean(dim=0)
        norms[l] = round(float(v.norm()), 2)
        buf[(l - 1) * n_embd: l * n_embd] = v
    buf.numpy().tofile(VEC_PATH)
    print(f"delta norms by layer: {norms}")
    print(f"exported {VEC_PATH}")
    save_json("phase2_derive.json", {
        "used_items": used, "n_items": len(items), "samples": args.samples,
        "temp": args.temp, "distractors": args.distractors,
        "delta_norms": norms,
    })


def _first_tok(tok, value):
    return tok.encode(" " + value, add_special_tokens=False)[0]


def _auc(pos, neg):
    """Mann-Whitney AUC: P(pos > neg) with tie=0.5."""
    if not pos or not neg:
        return None
    wins = ties = 0
    for p in pos:
        for n in neg:
            if p > n:
                wins += 1
            elif p == n:
                ties += 1
    return (wins + 0.5 * ties) / (len(pos) * len(neg))


def cmd_readout(model, tok, args):
    """Does a gold-vs-distractor J-lens margin over the answer tokens
    predict the sample's outcome, within-item? Uses the derive cache."""
    cache = torch.load(SAMPLES_CACHE, weights_only=False)
    per_layer_aucs = defaultdict(list)
    for entry in cache:
        it, rows = entry["item"], entry["rows"]
        if not rows:
            continue
        text = chat_prompt(tok, render_user(it))
        enc = tok(text, return_tensors="pt").to(DEVICE)
        logits, resids = forward_with_capture(model, enc.input_ids,
                                              torch.ones_like(enc.input_ids))
        logprobs = torch.log_softmax(logits.float(), dim=-1)
        dirs = {}  # value -> {layer: unit vec}
        for val in [it["gold"]] + list(dict.fromkeys(it["wrong"])):
            tid = _first_tok(tok, val)
            grads = torch.autograd.grad(logprobs[0, tid], [resids[l] for l in BAND],
                                        retain_graph=True)
            dirs[val] = {l: torch.nn.functional.normalize(
                g[0, -1, :].float().cpu(), dim=0) for l, g in zip(BAND, grads)}
        del logits, resids, logprobs
        for l in BAND:
            pos, neg = [], []
            for row in rows:
                h = row["means"][l]
                margin = float(dirs[it["gold"]][l] @ h) - max(
                    float(dirs[w][l] @ h) for w in it["wrong"])
                (pos if row["verdict"] == "correct" else neg).append(margin)
            a = _auc(pos, neg)
            if a is not None:
                per_layer_aucs[l].append(a)
    summary = {l: round(sum(v) / len(v), 3) for l, v in per_layer_aucs.items()}
    print("within-item AUC (gold-vs-distractor J-lens margin -> outcome):")
    for l in BAND:
        if l in summary:
            print(f"  layer {l}: {summary[l]:.3f} (n={len(per_layer_aucs[l])} items)")
    best = max(summary, key=summary.get)
    print(f"best layer {best}: AUC {summary[best]:.3f}  (0.5 = chance)")
    save_json("phase2_readout.json", {"per_layer_auc": summary,
                                      "n_items": len(cache)})


def cmd_validate(model, tok, args):
    items = [make_item(s, n_distractors=args.distractors, confusable=True)
             for s in HELDOUT_SEEDS]
    absent = [make_item(s, n_distractors=3, confusable=True, absent=True)
              for s in HELDOUT_ABSENT_SEEDS]
    import numpy as np
    buf = np.fromfile(VEC_PATH, dtype="<f4")
    n_embd = model.config.hidden_size
    base_vecs = {l: torch.from_numpy(
        buf[(l - 1) * n_embd: l * n_embd].copy()) for l in BAND}

    results = {}
    for scale in args.scales:
        vecs = None if scale == 0 else {l: v * scale for l, v in base_vecs.items()}
        # temp-0 accuracy
        texts0, _, _ = batch_generate(model, tok, render_prompts(tok, items),
                                      temperature=0.0, layer_vecs=vecs)
        acc0 = sum(score(it, t) == "correct" for it, t in zip(items, texts0)) / len(items)
        # sampled accuracy (matches derivation temp)
        sampled = sample_items(model, tok, items, args.samples, args.temp,
                               layer_vecs=vecs, progress=lambda *_: None)
        flat = [v for _, runs in sampled for _, v, _, _ in runs]
        accS = flat.count("correct") / len(flat)
        # absent-item honesty
        texts_a, _, _ = batch_generate(model, tok, render_prompts(tok, absent),
                                       temperature=0.0, layer_vecs=vecs)
        abst = sum(score(it, t) == "correct" for it, t in zip(absent, texts_a)) / len(absent)
        results[str(scale)] = {"acc_temp0": acc0, "acc_sampled": accS,
                               "abstain_absent": abst,
                               "examples": texts0[:3]}
        print(f"scale {scale}: temp0 acc {acc0:.0%} | sampled acc {accS:.0%} "
              f"| abstain-on-absent {abst:.0%}")
    save_json("phase2_validate.json", results)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--calibrate", action="store_true")
    ap.add_argument("--derive", action="store_true")
    ap.add_argument("--readout", action="store_true")
    ap.add_argument("--validate", action="store_true")
    ap.add_argument("--samples", type=int, default=6)
    ap.add_argument("--temp", type=float, default=0.9)
    ap.add_argument("--distractors", type=int, default=4)
    ap.add_argument("--scales", type=float, nargs="+",
                    default=[0.0, 0.5, 1.0, 2.0])
    args = ap.parse_args()

    tok, model = load_model()
    if args.calibrate:
        cmd_calibrate(model, tok, args)
    if args.derive:
        cmd_derive(model, tok, args)
    if args.readout:
        cmd_readout(model, tok, args)
    if args.validate:
        cmd_validate(model, tok, args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
