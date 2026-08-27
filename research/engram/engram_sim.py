"""Engram hot-set simulator.

Models the Qwen3.8-Flash n-gram embedding table addressing:
  8 bigram heads + 8 trigram heads, 20M rows/head, 160 dims/row.
Row id = splitmix64(ngram_key ^ seed_head) % N_ROWS.

Measures, per head, the coverage of a STATIC top-K-by-frequency hot set
mined on one corpus and evaluated on a disjoint one.
"""
import numpy as np, sys, json

N_ROWS   = 20_000_000
V_BITS   = 18            # vocab 248,069 < 2^18
N_BI, N_TRI = 8, 8
ROW_DIM  = 160

M1 = np.uint64(0xBF58476D1CE4E5B9)
M2 = np.uint64(0x94D049BB133111EB)

def splitmix64(z):
    z = z.copy()
    z ^= z >> np.uint64(30); z *= M1
    z ^= z >> np.uint64(27); z *= M2
    z ^= z >> np.uint64(31)
    return z

def ngram_keys(toks, n):
    """Packed n-gram keys, one per position with full history."""
    t = toks.astype(np.uint64)
    k = np.zeros(len(t) - (n - 1), dtype=np.uint64)
    for j in range(n):
        k |= t[j : len(t) - (n - 1) + j] << np.uint64(V_BITS * (n - 1 - j))
    return k

def rows_for_head(keys, seed):
    return (splitmix64(keys ^ np.uint64(seed)) % np.uint64(N_ROWS)).astype(np.uint32)

def coverage_curve(rows_mine, rows_eval):
    """Exact coverage of eval accesses by a top-K hot set mined on rows_mine.

    Returns cumulative coverage indexed by K (hot-set size in rows)."""
    cm = np.bincount(rows_mine, minlength=N_ROWS).astype(np.int32)
    ce = np.bincount(rows_eval, minlength=N_ROWS).astype(np.int32)
    nz = np.flatnonzero(cm)                       # rows the miner ever saw
    order = nz[np.argsort(cm[nz])[::-1]]          # ranked by mine frequency
    cum = np.cumsum(ce[order].astype(np.int64))
    return cum / len(rows_eval), len(order)

def at_k(curve, k):
    if k <= 0: return 0.0
    return float(curve[min(k, len(curve)) - 1])

def run(mine_name, evals, seeds_bi, seeds_tri, tag=""):
    mine = np.load(f"{S}/{mine_name}.npy")
    out = {}
    for kind, n, seeds in (("bigram", 2, seeds_bi), ("trigram", 3, seeds_tri)):
        km = ngram_keys(mine, n)
        for hi, seed in enumerate(seeds):
            rm = rows_for_head(km, seed)
            for ev_name in evals:
                ev = np.load(f"{S}/{ev_name}.npy")
                ke = ngram_keys(ev, n)
                re_ = rows_for_head(ke, seed)
                curve, n_mined = coverage_curve(rm, re_)
                out.setdefault((kind, ev_name), []).append(
                    dict(seed=seed, n_mined=n_mined,
                         uniq_eval=int(len(np.unique(re_))),
                         k=[at_k(curve, K) for K in KS],
                         ceiling=float(curve[-1])))
                del ke, re_, ev, curve
            del rm
        del km
    return out

KS = [10_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_000_000,
      4_000_000, 8_000_000, 16_000_000]

if __name__ == "__main__":
    S = sys.argv[1]
    # 2 heads per kind is statistically sufficient: heads differ only in
    # which collisions occur, not in the frequency distribution's shape.
    seeds_bi  = [0x1000, 0x2000]
    seeds_tri = [0xA000, 0xB000]
    evals = ["sep_mine", "sep_holdout", "repo_md", "rust_src"]
    res = run("sep_mine", evals, seeds_bi, seeds_tri)

    print(f"{'kind':8s} {'eval corpus':13s} {'uniq rows':>11s} {'ceil':>6s} " +
          " ".join(f"{k//1000}k".rjust(7) for k in KS))
    print("-" * 118)
    rows = {}
    for (kind, ev), lst in sorted(res.items()):
        k = np.mean([x["k"] for x in lst], axis=0)
        rows[(kind, ev)] = k
        ceil = np.mean([x["ceiling"] for x in lst])
        uq = int(np.mean([x["uniq_eval"] for x in lst]))
        star = " <- MINE" if ev == "sep_mine" else ""
        print(f"{kind:8s} {ev:13s} {uq:>11,} {ceil:>6.3f} " +
              " ".join(f"{v:7.3f}" for v in k) + star)
    json.dump({f"{a}|{b}": list(v) for (a, b), v in rows.items()},
              open(f"{S}/curves.json", "w"), indent=1)
    print(f"\nKS = {KS}")
