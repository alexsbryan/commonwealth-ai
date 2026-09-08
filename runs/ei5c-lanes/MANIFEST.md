# runs/ei5c-lanes — ei-5c-seed-race, item 5's four lanes as one unit

Branch `ei-5c`, worktree `/home/alexbryan/dev/ei5c-wt`. Sequential by design:
(a), (b) and (d) want the embed slot, (c) wants the 35B, and one GPU lane at a
time is the box rule. The unit refuses at preflight if this worktree's
binaries are absent — a leg that ran main's `sovereign-cli-llm` would measure
main.

| leg | lane | what it answers | forecast |
|---|---|---|---|
| `0-reseed-bk` | — | `brothers-karamazov-book-1`'s seed-table marker reads schema **1**, so `ann_table_is_fresh` is already false and the daemon would rebuild it silently at next boot. This does it deliberately, from this branch's population logic, with the Entity-era table copied to `atlas/atoms_ann.pre-ei5c.<date>/` first — one `mv` restores it. Seat-authorised; nothing else under that corpus is written. | < 1 min |
| `a-sep-run{1,2}` | (a) EI2 | Bare `sep`, walk SOLE, injector gone. Bar: facts ≥ 0.9557 (the 2026-08-10 floor, 151/158). | ~12 min |
| `b-{off,on}-run{1,2}` | (b) displacement | The ei-7a fixture, both arms, both directions. `raptor-subset-off` has no Summary atoms, `-on` has 2,004; otherwise byte-identical. NO rebuild: the per-kind budget is a WALK-time policy and changes no seed table, and preflight prints both markers and embedded counts to show it. | ~25 min |
| `c-ei1-run{1,2,3}` | (c) EI1 | `thematic-bk-book-1`, `--synth --isolate` on the 35B, the floor's own flags. Bar: cited-with-evidence ≥ 0.4091, reported against target 0.50. n=3 because the floor is n=2. | ~22 min |
| `d-acceptance` | (d) theme bar | `corpus-mcp/acceptance.sh` whole. Its own llama-server is the embed gguf only. | ~5 min |

Total forecast 60-70 min. Memory: the 35B in leg c is the peak (~33 GB
resident); everything else is the embed slot.

## The instrument checks, and why each is here

- **`--limit 30`, never the default 10.** Note `b0cf07e8`: a limit-10 run read
  against a limit-30 baseline reported a 21-fact regression that did not exist
  and survived four hours of bisecting. Both eval legs pin it and the artifact's
  own `limit` field is printed on every row.
- **`--prod-pipeline`, and the mode line grepped back.** It is the only mode
  that reaches `apply_atlas_grounding`; an A/B in the default raw mode is one
  arm run twice, and nothing in the artifact would say so.
- **ANSI stripped before every ledger grep** (`yield.py`). `tracing` writes
  escapes BETWEEN a field name and its `=`, so a naive `summary_seeds=(\d+)`
  matches nothing and returns a clean, plausible ZERO — which is exactly the
  finding these lanes measure. That trap produced two false readings in ei-7a.
- **One scorer** (`score.py`) for every lane, so two rows in the report cannot
  have been computed two ways; a field a run did not produce prints `absent`,
  never 0.
- **The absence line** (`yield.py`'s `ABSENCE_*`). ei-5c deleted the RAPTOR
  injector, so a corpus with RAPTOR rows and no `Summary` atoms has no
  whole-work summaries at all. That is expected on `sep` today and it must show
  up as a named line rather than as a quiet number.

## Reading the results

`out/<leg>.rc` per leg, `out/<leg>.txt` (the scorer's rows), `out/*.json` +
`out/*.log` (the artifacts), `out/preflight.txt`, `out/box-{before,after}.txt`,
`out/log.txt`, and a terminal `out/DONE` written even on SIGTERM.

Gate on the rc files. Every leg returns 0 from the runner regardless, so a red
leg never aborts the unit and the later lanes still report — the rc file is
where the verdict is.
