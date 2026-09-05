# ei-7c A/B — arm 0 rows ("today": the INSTALLED wikipedia store, untouched)

Store read: `~/.svrnmesh/indexes/wikipedia/atlas` (v1: atoms.json + edges.json +
a v1 articles.lance with no `atom_id`/`chunk_id`). Nothing written to it.

Caveat carried on every row, per the seat's ruling: atom ids are corpus-qualified,
so a store rebuilt as `wikipedia` and later mounted under a fixture id has correct
STORED ids and wrong PEER-MINTED ones. Arm 0 is the control and is unaffected;
the caveat binds arms 1 and 2.

| arm | mode | atlas in the loop? | fact_recall | source_recall | n |
|---|---|---|---|---|---|
| 0 run1 | retrieval (default) | NO - negative control | 0.7071 | 0.5458 | 20 |
| 0 run2 | retrieval (default) | NO - negative control | 0.7071 | 0.5458 | 20 |
| 0 run1 | --prod-pipeline --isolate | attempted, REFUSED | 0.6946 | 0.6167 | 20 |
| 0 run2 | --prod-pipeline --isolate | attempted, REFUSED | 0.6946 | 0.6167 | 20 |

Run-to-run spread: **exactly zero** in both modes — 20/20 questions identical,
delta 0.0000 on both metrics. For a HARD exact-band lane that is the useful
property: any delta an arm shows is real, not noise (ARCH 18.5).
