#!/usr/bin/env python3
"""P5.2a scorer (gate G9): ColModernVBERT page-score separation.

Embeds the 16 fixture pages (images) and their 16 paired sep-bench queries
(text), scores all pairs with the processor's late-interaction MaxSim, and
reports ranking metrics + separation margins + per-item embed timing.

Usage:
  .venv/bin/python scripts/p52a_score.py --fixture data/p52a --out runs/p52a/scores.json
"""

import argparse
import json
import time
from pathlib import Path

import torch
from colpali_engine.models import ColModernVBert, ColModernVBertProcessor
from PIL import Image

MODEL_ID = "ModernVBERT/colmodernvbert"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--fixture", default="data/p52a")
    ap.add_argument("--out", default="runs/p52a/scores.json")
    ap.add_argument("--device", default="mps" if torch.backends.mps.is_available() else "cpu")
    args = ap.parse_args()

    fixture = Path(args.fixture)
    labels = json.loads((fixture / "labels.json").read_text())
    pages = [Image.open(fixture / "pages" / f"page_{l['page']:02d}.png") for l in labels]
    queries = [l["query"] for l in labels]

    t0 = time.time()
    processor = ColModernVBertProcessor.from_pretrained(MODEL_ID)
    model = ColModernVBert.from_pretrained(
        MODEL_ID, torch_dtype=torch.float32, trust_remote_code=True
    ).to(args.device).eval()
    load_s = time.time() - t0
    n_params = sum(p.numel() for p in model.parameters())
    print(f"model loaded in {load_s:.1f}s, {n_params/1e6:.0f}M params, device {args.device}")

    with torch.no_grad():
        t0 = time.time()
        img_embs = []
        for img in pages:
            inputs = {k: v.to(args.device) for k, v in processor.process_images([img]).items()}
            img_embs.append(model(**inputs).cpu())
        img_s = time.time() - t0

        t0 = time.time()
        q_embs = []
        for q in queries:
            inputs = {k: v.to(args.device) for k, v in processor.process_texts([q]).items()}
            q_embs.append(model(**inputs).cpu())
        q_s = time.time() - t0

    print(f"embed: {img_s/len(pages):.2f} s/page, {q_s/len(queries)*1e3:.0f} ms/query")
    print(f"vectors/page: {img_embs[0].shape}, vectors/query: {q_embs[0].shape}")

    # Score matrix: MaxSim via processor.score (handles padding).
    q_cat = [e[0] for e in q_embs]
    d_cat = [e[0] for e in img_embs]
    scores = processor.score(q_cat, d_cat)  # (n_queries, n_pages)
    scores = scores.float()

    n = len(labels)
    ranks, margins, rows = [], [], []
    for qi in range(n):
        row = scores[qi]
        order = torch.argsort(row, descending=True).tolist()
        rank = order.index(qi) + 1
        target = row[qi].item()
        best_other = max(row[j].item() for j in range(n) if j != qi)
        ranks.append(rank)
        margins.append(target - best_other)
        rows.append(
            {
                "question_id": labels[qi]["question_id"],
                "slug": labels[qi]["slug"],
                "rank": rank,
                "target_score": round(target, 4),
                "best_other_score": round(best_other, 4),
                "margin": round(target - best_other, 4),
                "top_slug": labels[order[0]]["slug"],
            }
        )

    top1 = sum(1 for r in ranks if r == 1) / n
    mrr = sum(1 / r for r in ranks) / n
    mean_margin = sum(margins) / n
    print(f"top-1 accuracy: {top1:.3f}  MRR: {mrr:.3f}")
    print(f"margin (target - best other): mean {mean_margin:.3f}  min {min(margins):.3f}")
    for r in rows:
        flag = "" if r["rank"] == 1 else f"  <-- rank {r['rank']} (top: {r['top_slug']})"
        print(f"  {r['slug'][:40]:40s} margin {r['margin']:+.3f}{flag}")

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        json.dumps(
            {
                "model": MODEL_ID,
                "device": args.device,
                "params_m": n_params / 1e6,
                "load_s": load_s,
                "s_per_page": img_s / len(pages),
                "ms_per_query": q_s / len(queries) * 1e3,
                "top1": top1,
                "mrr": mrr,
                "mean_margin": mean_margin,
                "min_margin": min(margins),
                "rows": rows,
            },
            indent=2,
        )
    )
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
