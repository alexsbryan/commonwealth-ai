"""Price the engram access pattern on this host's NVMe.

Per decoded token the model gathers 16 rows (one per n-gram head). The row ids
are independent, so the 16 reads issue concurrently: cost is ONE device round
trip, not sixteen. Measures p50/p90/p99 of a 16-way concurrent random gather
over a 92 GB file, cold (page cache advised away) and warm (cache-resident).
"""
import os, sys, time, numpy as np
from concurrent.futures import ThreadPoolExecutor

PATH = sys.argv[1]
ROW  = 170          # Q8_0: 160 dims = 5 blocks x 34 B
HEADS= 16
N    = int(sys.argv[2]) if len(sys.argv) > 2 else 400

size = os.path.getsize(PATH)
fd   = os.open(PATH, os.O_RDONLY)
rng  = np.random.default_rng(7)
print(f"file {PATH.split('/')[-1]}  {size/2**30:.1f} GiB   rows/token={HEADS} row={ROW}B")

def gather(offs, pool):
    list(pool.map(lambda o: os.pread(fd, ROW, int(o)), offs))

def bench(label, cold):
    lat = []
    with ThreadPoolExecutor(max_workers=HEADS) as pool:
        base = rng.integers(0, size - ROW, size=HEADS) if not cold else None
        for _ in range(N):
            offs = rng.integers(0, size - ROW, size=HEADS) if cold else base
            if cold:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            t = time.perf_counter_ns()
            gather(offs, pool)
            lat.append((time.perf_counter_ns() - t) / 1000.0)   # us
    a = np.array(lat)
    print(f"  {label:22s} p50 {np.percentile(a,50):8.1f} us   "
          f"p90 {np.percentile(a,90):8.1f} us   p99 {np.percentile(a,99):8.1f} us")
    return float(np.percentile(a, 50))

print("\n16-way concurrent gather, one token's worth:")
cold = bench("COLD (fadvise away)", True)
warm = bench("WARM (page-cached)",  False)
os.close(fd)

print(f"\nAs a share of a decode step:")
for tps, name in ((39, "projected Flash 6B-active"), (19.34, "measured 122B-A10B")):
    budget = 1e6 / tps
    print(f"  {name:28s} {budget:7.0f} us/token -> "
          f"cold {100*cold/budget:5.2f}%   warm {100*warm/budget:5.2f}%")
