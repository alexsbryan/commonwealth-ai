# map-conversion rung 1 (2026-09-08): row admissibility from inventory

Same banks, same binary lineage and gates (0.34 / 0.05) as `../map-conversion-baseline/`.

- `kind-sep-rung1.{txt,err}` — `svrn atlas kind --corpus sep`: census over 1,771
  atlases in 10 s; every row but enumeration fits; 6/21 classified, all admitted.
- `kind-wiki-rung1.{txt,err}` — `--corpus wikipedia`: wiki-class store opened for
  its census (12.7 s); only lookup fits; the 4 classified tension winners run the
  unfiltered row (runner-ups below the floor).
- `wiki-rung1.json`, `wiki-rung1-ledger.log` — the wikipedia lane: facts 111/130
  (baseline 113/130), sources 40/58 (unchanged), n=2 bit-identical. The four
  `row-inert` walks reach 593–1,465 nodes where the baseline reached 0; the two
  facts lost are both on `contested_globalization_effects` (7/8 → 5/8).
