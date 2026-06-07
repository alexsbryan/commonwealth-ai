#!/usr/bin/env python3
"""Repair duplicate chunk-ids in the wikipedia corpus index.

Root cause (fixed in corpus-engine/src/index/write.rs): the chunk-id
allocator derived new ids from `chunk_count()` (the row count) instead of
`max(id) + 1`. After delta appends the row count fell below the max id, so
the next ingest REUSED existing ids. `neighbors(id)` (the citation
read-back path) then became ambiguous — "2026 Lebanon war" read back as
"Gold".

This script makes every `id` unique again WITHOUT re-embedding and WITHOUT
rebuilding the 444M vector index from scratch:

  1. Read every row in a duplicated-id group (full schema, incl. embedding).
  2. Keep the FIRST row of each group at its original id; reassign the rest
     to a fresh contiguous range above the current max id.
  3. Delete the old group rows; append the corrected rows.
  4. `optimize_indices()` folds the appended rows into IVF-PQ for speed
     (Lance already searches unindexed rows transparently, so coverage is
     never lost — verified on a fixture).
  5. Stamp `_corpus_meta.json.next_chunk_id` so the (now fixed) allocator
     continues above the repaired range.

Reversible: Lance keeps the pre-repair version; restore with
`lance.dataset(PATH).restore(<version>)` (printed below before any write).
"""
import collections
import json
import os
import sys

import lance
import pyarrow as pa

PATH = os.path.expanduser("~/.sovereign/indexes/wikipedia/chunks.lance")
META = os.path.expanduser("~/.sovereign/indexes/wikipedia/_corpus_meta.json")


def log(msg):
    print(f"[repair] {msg}", flush=True)


def main():
    ds = lance.dataset(PATH)
    snapshot = ds.version
    total = ds.count_rows()
    log(f"dataset version (ROLLBACK TARGET) = {snapshot}; rows = {total}")

    # 1. Identify duplicated ids.
    id_tbl = ds.to_table(columns=["id"]).column("id").to_pylist()
    freq = collections.Counter(id_tbl)
    dup_ids = sorted(k for k, v in freq.items() if v > 1)
    dup_rows = total - len(set(id_tbl))
    cur_max = max(id_tbl)
    log(f"duplicated ids = {len(dup_ids)}; duplicated rows = {dup_rows}; current max id = {cur_max}")
    if not dup_ids:
        log("no duplicates — nothing to do.")
        return 0

    # 2. Read full rows for every duplicated-id group (one IN predicate).
    in_list = ",".join(map(str, dup_ids))
    group = ds.to_table(filter=f"id IN ({in_list})")
    log(f"read {group.num_rows} group rows (full schema, {len(group.schema.names)} cols)")

    # 3. Assign corrected ids in scan order: keep-first, extras -> fresh range.
    old_ids = group.column("id").to_pylist()
    seen = set()
    new_ids = []
    nxt = cur_max + 1
    for oid in old_ids:
        if oid in seen:
            new_ids.append(nxt)
            nxt += 1
        else:
            seen.add(oid)
            new_ids.append(oid)
    reassigned = sum(1 for o, n in zip(old_ids, new_ids) if o != n)
    new_max = nxt - 1
    log(f"reassigned {reassigned} rows to ids {cur_max + 1}..{new_max}; "
        f"kept {len(dup_ids)} group-leaders at original ids")

    # Safety: corrected group ids must be internally unique, and the fresh
    # ids must sit strictly above every pre-existing id (so they can't
    # collide with the ~1.86M untouched rows).
    assert len(set(new_ids)) == len(new_ids), "corrected group ids not unique"
    assert min(n for o, n in zip(old_ids, new_ids) if o != n) > cur_max, "fresh id not above max"
    assert reassigned == dup_rows, f"reassigned {reassigned} != duplicated rows {dup_rows}"

    id_idx = group.schema.get_field_index("id")
    corrected = group.set_column(id_idx, "id", pa.array(new_ids, pa.int64()))

    # 4. Delete old group rows, append corrected (window between the two is
    # just the two commits; rollback target recorded above).
    log("deleting old group rows...")
    ds.delete(f"id IN ({in_list})")
    log("appending corrected rows...")
    lance.write_dataset(corrected, PATH, mode="append")

    ds = lance.dataset(PATH)
    after = ds.count_rows()
    log(f"row count after = {after} (was {total}); delta = {after - total}")
    assert after == total, "row count changed — ABORT (restore the snapshot)"

    # 5. Verify global uniqueness.
    ids2 = ds.to_table(columns=["id"]).column("id").to_pylist()
    uniq = len(set(ids2))
    log(f"unique ids after = {uniq} / {len(ids2)}  ->  {'OK' if uniq == len(ids2) else 'STILL DUP'}")
    assert uniq == len(ids2), "ids still not unique"

    # 6. Fold appended rows into the IVF-PQ index for query speed.
    log("optimize_indices() (incremental — folds appended rows in)...")
    ds.optimize.optimize_indices()

    # 7. Stamp the allocator high-water in the meta.
    meta = json.load(open(META))
    meta["next_chunk_id"] = new_max + 1
    json.dump(meta, open(META, "w"), indent=2)
    log(f"meta.next_chunk_id = {new_max + 1}")

    log(f"DONE. Rollback if needed: lance.dataset('{PATH}').restore({snapshot})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
