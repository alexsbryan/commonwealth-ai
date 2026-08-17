#!/usr/bin/env python3
"""DRB subset selection — the pinned, reproducible method (order deep-research-t2b).

Selection method (frozen in pre-registration.md, T2b declaration):
  * population = the 50 English tasks of the DRB prompt set (query.full.jsonl,
    language == "en", id list in file order)
  * seed string "deep-research-t2b-drb-subset-2026-08-17"
  * seed = int(sha256(seed_string)[:8], 16)  # 556953489
  * rng = random.Random(seed); subset = sorted(rng.sample(en_ids, 10))

Content-blind: the population is the id list only; prompts are never read
by the selector.

Run: python3 select-subset.py   (writes query.subset.jsonl)
"""
import hashlib
import json
import random

SEED_STRING = "deep-research-t2b-drb-subset-2026-08-17"
N = 10
FULL = "query.full.jsonl"
OUT = "query.subset.jsonl"

def main() -> None:
    seed = int(hashlib.sha256(SEED_STRING.encode()).hexdigest()[:8], 16)
    rows = [json.loads(l) for l in open(FULL, encoding="utf-8")]
    en = [r for r in rows if r.get("language") == "en"]
    assert len(en) == 50, f"expected 50 English tasks, got {len(en)}"
    rng = random.Random(seed)
    subset = sorted(rng.sample([r["id"] for r in en], N))
    print(f"seed={seed}  subset={subset}")
    chosen = [r for r in en if r["id"] in subset]
    with open(OUT, "w", encoding="utf-8") as f:
        for r in chosen:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"wrote {OUT}: {len(chosen)} rows")

if __name__ == "__main__":
    main()
