"""Phase 3 — outcome-contrast vector derived from REAL chaos-bank prompts.

The synthetic generator couldn't make Qwen3-8B fail (100% over 576 samples
at every difficulty) — template-shaped evidence is table lookup. The chaos
failure mode lives in retrieval-fed narrative chunks. So derive on the real
substrate: the baseline arm's (question, retrieved_chunks) pairs, resampled
at temperature in PyTorch to restore the within-item contrast, scored with
the bank's own gold_keywords rule.

Split discipline: derivation uses EVEN-indexed answerable items only;
validation reports held-out ODD items (never seen during derivation).

Stages: --sample (collect + cache), --derive (vector from cache),
--validate (held-out temp-0 + sampled, scale sweep, absent honesty).
"""

import argparse
import gc
import json
import os
import random
import re
import subprocess
import sys
import tomllib
from collections import defaultdict

import torch

from jlens_common import DEVICE, Injector, OUT_DIR, chat_prompt, load_model, save_json
from phase2_outcome_vector import BAND, answer_resids, batch_generate, _null
from synth_items import ABSTAIN_MARKERS

BANK = "/Users/alexsbryan/dev/commonwealth-ai/sovereign/bench/chaos_monkey/secret_agent.toml"
TRANSCRIPTS = os.path.join(OUT_DIR, "chaos_baseline_transcripts.jsonl")
CACHE = os.path.join(OUT_DIR, "phase3_samples.pt")
VEC_PATH = os.path.join(OUT_DIR, "bank_outcome_qwen3-8b.f32")


def load_items():
    bank = tomllib.load(open(BANK, "rb"))
    gold = {q["id"]: q.get("gold_keywords", []) for q in bank["questions"]}
    items = []
    for line in open(TRANSCRIPTS):
        r = json.loads(line)
        items.append({
            "id": r["id"],
            "question": r["question"],
            "chunks": r["retrieved_chunks"],
            "expected": r["expected_action"],  # Answer | Abstain
            "gold_keywords": gold.get(r["id"], []),
        })
    return items


def render_user(item, max_chunks=12, perm_seed=None):
    # Evidence policy: first `max_chunks` chunks at FULL length, plus any
    # later chunk containing a gold alternative. Round one truncated to
    # 10x700 chars and cut witness content — the model's "abstentions" were
    # correct reads of starved prompts, poisoning the outcome labels. Full
    # 20-chunk prompts (~7k tokens) OOM MPS at batch 8; this keeps the
    # witness guaranteed-present at ~4k tokens.
    # perm_seed: shuffle passage ORDER (same set, witness still present) —
    # within-item outcome variance from witness position, not temperature.
    ps = list(item["chunks"][:max_chunks])
    alts = [a.strip().lower() for g in item.get("gold_keywords", [])
            for a in g.split("|")]
    for c in item["chunks"][max_chunks:]:
        cl = c.lower()
        if any(a in cl for a in alts):
            ps.append(c)
    if perm_seed is not None:
        random.Random(perm_seed).shuffle(ps)
    body = "\n\n".join(f"Passage {i + 1}: {p}" for i, p in enumerate(ps))
    return (f"{body}\n\nQuestion: {item['question']}\n"
            f"Answer briefly using only the passages. If they don't contain "
            f"the answer, say so.")


def gold_match(gold_keywords, reply):
    """Bench rule: every group matches via at least one pipe-alternative."""
    rl = reply.lower()
    if not gold_keywords:
        return False
    for group in gold_keywords:
        if not any(alt.strip().lower() in rl for alt in group.split("|")):
            return False
    return True


def verdict(item, reply):
    abstained = any(m in reply.lower() for m in ABSTAIN_MARKERS)
    if item["expected"] == "Abstain":
        return "correct" if abstained else "wrong"
    if gold_match(item["gold_keywords"], reply):
        return "correct"
    return "wrong"


def answerable(items):
    return [it for it in items if it["expected"] == "Answer"]


def witness_present(item):
    """Every gold group has at least one alternative literally in the
    rendered prompt. Items failing this can only produce 'wrong' labels
    regardless of model behavior — they poison the outcome contrast."""
    u = render_user(item).lower()
    if not item["gold_keywords"]:
        return False
    return all(any(a.strip().lower() in u for a in g.split("|"))
               for g in item["gold_keywords"])


def _rss_gb():
    # ps RSS is blind to MTLBuffers (run-3 OOM showed 36MB RSS while Metal
    # held 40GB) — the Metal driver allocation is the real number on MPS.
    kb = int(subprocess.check_output(
        ["ps", "-o", "rss=", "-p", str(os.getpid())]))
    rss = kb / 2**20
    if torch.backends.mps.is_available():
        rss = max(rss, torch.mps.driver_allocated_memory() / 2**30)
    return rss


def _release_mps():
    gc.collect()
    if torch.backends.mps.is_available():
        torch.mps.empty_cache()


def cmd_sample(model, tok, args):
    # 2026-07-07 incident: the un-guarded version of this loop grew to a
    # ~94GB phys footprint (MPS allocator retains every odd-sized KV
    # buffer across generate calls; nothing ever released it), exhausted
    # 64GB RAM + ~60GB swap, and kernel-panicked the machine (watchdogd
    # starved 90s). Hence: per-item empty_cache, incremental save with
    # resume, and an RSS abort threshold.
    items = answerable(load_items())
    derive_items = items[0::2]
    kept = []
    for it in derive_items:
        if witness_present(it):
            kept.append(it)
        else:
            print(f"  SKIP {it['id']}: witness not in prompt (label unusable)")
    print(f"{len(items)} answerable; deriving on {len(kept)}/{len(derive_items)} "
          f"even-indexed witness-present items")
    done = {}
    if os.path.exists(CACHE):
        for e in torch.load(CACHE, weights_only=False, map_location="cpu"):
            done[e["item"]["id"]] = e
        print(f"resume: {len(done)} items already cached"
              + (" (permute: appending permuted-order rows)" if args.permute else ""))
    for it in kept:
        entry = done.get(it["id"])
        if entry is not None and not args.permute:
            continue
        n_perm = sum(1 for r in entry["rows"] if r.get("perm")) if entry else 0
        if args.permute and entry is not None:
            # perm-rounds caps total permuted rows so re-runs don't double-append
            if n_perm >= args.samples * args.perm_rounds:
                continue
            if args.only_mixed:
                av = [r["verdict"] for r in entry["rows"]]
                if not (0 < av.count("correct") < len(av)):
                    continue
        idx = derive_items.index(it)  # stable across resumes/skips
        if args.permute:
            # offset seeds by rows already present so each round is distinct
            prompts = [chat_prompt(tok, render_user(it, perm_seed=101 * idx + n_perm + s + 1))
                       for s in range(args.samples)]
            seed = 4000 + idx + 7 * n_perm
        else:
            prompts = [chat_prompt(tok, render_user(it))] * args.samples
            seed = 2000 + idx
        texts, seqs, plens = batch_generate(
            model, tok, prompts, max_new=48, temperature=args.temp,
            seed=seed, batch_size=args.batch)
        rows = []
        for t, seq, plen in zip(texts, seqs, plens):
            rows.append({"reply": t, "verdict": verdict(it, t),
                         "seq": seq.cpu(), "plen": plen, "perm": args.permute})
        if entry is None:
            entry = {"item": {k: it[k] for k in ('id', 'question', 'expected', 'gold_keywords')},
                     "rows": []}
            done[it["id"]] = entry
        entry["rows"].extend(rows)
        cache = [done[x["id"]] for x in kept if x["id"] in done]
        torch.save(cache, CACHE)  # incremental: a crash loses at most one item
        _release_mps()
        rss = _rss_gb()
        vs = [r["verdict"] for r in rows]
        av = [r["verdict"] for r in entry["rows"]]
        print(f"  {it['id']}: {vs.count('correct')}✓ {vs.count('wrong')}✗ new "
              f"({av.count('correct')}✓ {av.count('wrong')}✗ total) | rss {rss:.1f}GB")
        if rss > args.rss_limit:
            print(f"ABORT: rss {rss:.1f}GB > --rss-limit {args.rss_limit}GB; "
                  f"cache saved through {it['id']} — rerun --sample to resume")
            return
    cache = [done[x["id"]] for x in kept if x["id"] in done]
    mixed = sum(1 for e in cache
                if 0 < sum(r['verdict'] == 'correct' for r in e['rows']) < len(e['rows']))
    print(f"cached {len(cache)} items -> {CACHE}; mixed-outcome items: {mixed}")


def cmd_derive(model, tok, args):
    cache = torch.load(CACHE, weights_only=False)
    deltas = {l: [] for l in BAND}
    used = 0
    for entry in cache:
        rows = entry["rows"]
        good = [r for r in rows if r["verdict"] == "correct"]
        bad = [r for r in rows if r["verdict"] == "wrong"]
        if not good or not bad:
            continue
        used += 1
        acc = {"correct": {l: [] for l in BAND}, "wrong": {l: [] for l in BAND}}
        for r in good + bad:
            res = answer_resids(model, r["seq"].to(DEVICE), r["plen"], BAND)
            if res[BAND[0]].shape[0] == 0:
                continue
            for l in BAND:
                acc[r["verdict"]][l].append(res[l].mean(dim=0))
        for l in BAND:
            if acc["correct"][l] and acc["wrong"][l]:
                deltas[l].append(torch.stack(acc["correct"][l]).mean(dim=0)
                                 - torch.stack(acc["wrong"][l]).mean(dim=0))
        _release_mps()
        print(f"  contrasted {entry['item']['id']} "
              f"({len(good)}✓ vs {len(bad)}✗) | rss {_rss_gb():.1f}GB")
    print(f"mixed-outcome items used: {used}/{len(cache)}")
    if used < 5:
        print("TOO FEW mixed items — top up with --sample --permute (order variance "
              "flips outcomes; temperature alone does not on this bank)")
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
    print(f"delta norms: {norms}")
    print(f"exported {VEC_PATH}")
    save_json("phase3_derive.json", {"used_items": used, "delta_norms": norms})


def cmd_validate(model, tok, args):
    all_items = load_items()
    ans = answerable(all_items)
    heldout = ans[1::2]  # odd-indexed: never used in derivation
    absent = [it for it in all_items if it["expected"] == "Abstain"]
    import numpy as np
    buf = np.fromfile(VEC_PATH, dtype="<f4")
    n_embd = model.config.hidden_size
    base_vecs = {l: torch.from_numpy(buf[(l - 1) * n_embd: l * n_embd].copy())
                 for l in BAND}

    results = {}
    for scale in args.scales:
        vecs = None if scale == 0 else {l: v * scale for l, v in base_vecs.items()}
        # held-out answerable, temp 0
        prompts = [chat_prompt(tok, render_user(it)) for it in heldout]
        texts, _, _ = batch_generate(model, tok, prompts, max_new=48,
                                     temperature=0.0, layer_vecs=vecs,
                                     batch_size=args.batch)
        acc = sum(verdict(it, t) == "correct" for it, t in zip(heldout, texts)) / len(heldout)
        # held-out answerable, sampled (matches derivation temp);
        # --samples 0 skips this pass (temp-0 + abstain are the headline)
        s_correct = s_total = 0
        for i, it in enumerate(heldout if args.samples > 0 else []):
            p = [chat_prompt(tok, render_user(it))] * args.samples
            ts, _, _ = batch_generate(model, tok, p, max_new=48,
                                      temperature=args.temp, seed=3000 + i,
                                      layer_vecs=vecs, batch_size=args.batch)
            s_correct += sum(verdict(it, t) == "correct" for t in ts)
            s_total += len(ts)
        # absent honesty, temp 0
        prompts_a = [chat_prompt(tok, render_user(it)) for it in absent]
        texts_a, _, _ = batch_generate(model, tok, prompts_a, max_new=48,
                                       temperature=0.0, layer_vecs=vecs,
                                       batch_size=args.batch)
        abst = sum(verdict(it, t) == "correct" for it, t in zip(absent, texts_a)) / len(absent)
        results[str(scale)] = {
            "heldout_acc_temp0": acc,
            "heldout_acc_sampled": (s_correct / s_total) if s_total else None,
            "abstain_absent": abst,
            "examples": texts[:2],
        }
        _release_mps()
        sampled = f"{s_correct/s_total:.0%}" if s_total else "skipped"
        print(f"scale {scale}: held-out temp0 {acc:.0%} | sampled "
              f"{sampled} | abstain-on-absent {abst:.0%} "
              f"| rss {_rss_gb():.1f}GB")
    save_json("phase3_validate.json", results)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sample", action="store_true")
    ap.add_argument("--derive", action="store_true")
    ap.add_argument("--validate", action="store_true")
    ap.add_argument("--samples", type=int, default=8)
    ap.add_argument("--batch", type=int, default=2)
    ap.add_argument("--temp", type=float, default=1.0)
    ap.add_argument("--rss-limit", type=float, default=40.0,
                    help="abort sampling cleanly past this process RSS (GB)")
    ap.add_argument("--permute", action="store_true",
                    help="append samples with shuffled passage order per sample")
    ap.add_argument("--perm-rounds", type=int, default=1,
                    help="target rounds of permuted samples per item")
    ap.add_argument("--only-mixed", action="store_true",
                    help="permute top-up only items that already have both outcomes")
    ap.add_argument("--scales", type=float, nargs="+", default=[0.0, 0.5, 1.0, 2.0])
    args = ap.parse_args()
    tok, model = load_model()
    if args.sample:
        cmd_sample(model, tok, args)
    if args.derive:
        cmd_derive(model, tok, args)
    if args.validate:
        cmd_validate(model, tok, args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
