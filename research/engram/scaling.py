"""Is the coverage ceiling a property of the table, or of my mining budget?

Mines hot sets from increasing prefixes of sep_mine and measures the ceiling
(fraction of held-out accesses whose row was seen AT LEAST ONCE while mining).
If the ceiling grows like log(mining tokens), the tail is fat and no
realistically-mined hot set closes it.

Also runs the pre-registered NULL: shuffled tokens destroy n-gram structure
while preserving unigram frequency.
"""
import numpy as np, sys
sys.path.insert(0, sys.argv[1])
from engram_sim import ngram_keys, rows_for_head, N_ROWS

S = sys.argv[1]
mine = np.load(f"{S}/sep_mine.npy")
hold = np.load(f"{S}/sep_holdout.npy")
rng  = np.random.default_rng(20260826)
shuf = rng.permutation(mine)          # null: same unigrams, no n-gram structure

SIZES = [500_000, 1_000_000, 2_000_000, 4_000_000, 8_000_000, len(mine)]

print(f"{'kind':8s} {'mine toks':>11s} {'ceiling':>8s} {'uniq mined':>12s} "
      f"{'@1M rows':>9s} {'@4M rows':>9s}   (eval = sep_holdout)")
print("-" * 88)
for kind, n, seed in (("bigram", 2, 0x1000), ("trigram", 3, 0xA000)):
    ke = ngram_keys(hold, n); re_ = rows_for_head(ke, seed)
    ce = np.bincount(re_, minlength=N_ROWS).astype(np.int32)
    tot = len(re_)
    for sz in SIZES:
        km = ngram_keys(mine[:sz], n); rm = rows_for_head(km, seed)
        cm = np.bincount(rm, minlength=N_ROWS).astype(np.int32)
        nz = np.flatnonzero(cm)
        order = nz[np.argsort(cm[nz])[::-1]]
        cum = np.cumsum(ce[order].astype(np.int64)) / tot
        g = lambda K: float(cum[min(K, len(cum)) - 1]) if len(cum) else 0.0
        print(f"{kind:8s} {sz:>11,} {cum[-1]:>8.3f} {len(nz):>12,} "
              f"{g(1_000_000):>9.3f} {g(4_000_000):>9.3f}")
        del km, rm, cm, nz, order, cum
    # null
    km = ngram_keys(shuf, n); rm = rows_for_head(km, seed)
    cm = np.bincount(rm, minlength=N_ROWS).astype(np.int32)
    nz = np.flatnonzero(cm); order = nz[np.argsort(cm[nz])[::-1]]
    cum = np.cumsum(ce[order].astype(np.int64)) / tot
    print(f"{kind:8s} {'NULL(shuf)':>11s} {cum[-1]:>8.3f} {len(nz):>12,} "
          f"{float(cum[min(1_000_000,len(cum))-1]):>9.3f} "
          f"{float(cum[min(4_000_000,len(cum))-1]):>9.3f}")
    print()
    del ke, re_, ce, km, rm, cm, nz, order, cum
