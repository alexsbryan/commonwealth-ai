import numpy as np, glob, os, sys, re
S = sys.argv[1]
for c in ["sep_mine","sep_holdout","repo_md","rust_src"]:
    parts = []
    for f in sorted(glob.glob(f"{S}/chunks/{c}.*.ids")):
        raw = open(f, "r").read().strip()
        if not raw: continue
        raw = raw.strip().lstrip("[").rstrip("]")
        a = np.fromstring(raw, dtype=np.int64, sep=",")
        parts.append(a.astype(np.uint32))
    toks = np.concatenate(parts)
    np.save(f"{S}/{c}.npy", toks)
    print(f"{c:14s} {len(toks):>12,} tokens   max_id={toks.max():,}")
