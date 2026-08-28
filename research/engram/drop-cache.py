#!/usr/bin/env python3
"""Evict ONLY the model shards from page cache (no root needed).
Without this the engram is already resident and the major-fault count is a
warm-cache artifact, not a measurement of demand paging."""
import os, sys, glob
tot = 0
for p in sorted(glob.glob(sys.argv[1])):
    fd = os.open(p, os.O_RDONLY)
    try:
        sz = os.fstat(fd).st_size
        os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
        tot += sz
        print(f"dropped {os.path.basename(p)} ({sz/2**30:.1f} GiB)")
    finally:
        os.close(fd)
print(f"total {tot/2**30:.1f} GiB advised DONTNEED")
