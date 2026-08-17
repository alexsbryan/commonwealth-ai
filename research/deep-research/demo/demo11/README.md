# DEMO-11 — the CLI journey: resume built, the compounding estate proven (order deep-research-t3a)

The six journey scenes run end-to-end through the shipped `svrn deep-research`
surface, with the ONE missing mechanism built under this order (scene 4's
`--resume`) and scene 6's write-path proven — the local cache the operator
made load-bearing (2026-08-17, verbatim): "a key part of the end user
feature is that the deep research session actually stores results in a
corpora that can be leveraged again in other deep research sessions (so we
don't always have to rebuild from web search and have a sort of local
cache)".

Everything here is verb-driven (the shipped CLI), mock-backed where the
banks apply (the FROZEN bank v1 deck — the bank is read, never edited),
with ONE real corpus leg: corpus `dr-demo11-s`, built once from
`source/` via the shipped corpus surface (`svrn corpus ingest`, real
LanceDB + real daemon embeddings) and frozen. The daemon model pin is the
t1h/t2c pin (local :9741, draft Qwen3.6-35B-A3B-MTP-UD-Q6_K, embed
Qwen3-Embedding-0.6B-Q8_0); no live web anywhere in the demo. The loop's
internals were FROZEN at t2c — this order changed nothing in
gym/deciders/gap-formation/scoring.

Pre-registered before any flight (ARCH §18.6): the seeded Q1/Q2, the
source corpus S, the resume flight's kill protocol and acceptance — in
`adversarial/pre-registration.md` (T3a DECLARATION). The red was watched
before the fix: flight `dr-1786976220` killed mid-run at HEAD with no
`--resume` surface, no checkpoint, no manifest (`red/`).

## The compounding pair (scenes 1, 2, 3, 5, 6)

- **Corpus S** — `dr-demo11-s`, built once from `source/` (five
  documents: the 1893 Act and construction, the Merrick Brae cable
  section, early ridership, the 1906 electrification, the municipal
  accounts — dense exact figures, authored for this demo, NWCI).
- **Run A** on **Q1** ("What is known about the Port Falkirk tramway —
  its construction history, its cost, and its early ridership in the
  decade after opening?") — `--search-source corpus --corpora
  dr-demo11-s --backend mock --mock-deck bank/v1/deck --max-rounds 3
  --search 12 --fetch 12 --consent personal`. At run close the verb
  INGESTS the run's fetched estate into a NEW corpus
  `dr-estate-<runA>` (create + insert + `build_indexes` +
  `mark_indexes_built` + `mark_ingestion_complete` — no manual
  ritual), and E lists and retrieves through the shipped corpus
  surface.
- **Run B** on **Q2** ("What were the Port Falkirk tramway's final
  construction cost and its opening-year passenger figures, which
  engineer oversaw the cable-haulage section, and what did
  electrification cost?") — Q2's value lives ONLY in E: the v1 deck
  carries none of Q2's specifics (final cost £223,400; opening-year
  passengers 2,314,807; the cable engineer Amelia Voss; electrification
  £88,500), so the web leg can honestly contribute nothing. Run B runs
  with `--corpora dr-estate-<runA>` and NO other corpus — the estate is
  the standing local cache. Its `survey-1.json` records estate hits
  BEFORE any acquisition; its passed claims cite
  `estate:dr-estate-<runA>:<chunk>` locators.

## The resume flight (scene 4)

The FROZEN v1 bank question + `bank/v1/deck`, budget 12/12,
`--max-rounds 3`. The process is SIGKILLed right after the post-round
checkpoint lands (`checkpoint.json`, `written_after_round = N >= 1`) —
the crash shape, not the abort shape. Then: a tampered COPY of the run
dir refuses ("tampered"), a conflicting re-passed flag refuses
("resume mismatch"), and `--resume <run-dir>` restores state, continues
at N+1, and the budget ledger appends with continuity — allowance ==
spent + remaining recomputed from the journal entries; the pre-kill
spends appear exactly once (a resume that can double-spend budget fails
the clause — the fail-closed refusal was never weakened).

Three reds were measured and fixed before the green (full records in
`adversarial/pre-registration.md`, appended as they executed):

1. **The flag gate refused a bare resume.** The gate compared CLI
   DEFAULTS for flags the operator did NOT pass against the frozen
   config — `--resume <dir>` with no flags refused (`--search 4
   differs from the checkpoint's 12`). Fixed: not-passed flags inherit
   the checkpoint's frozen values; only explicitly-passed flags are
   verified, and a conflict refuses naming the flag.
2. **The charter hash leaked the wall clock.** `hash_charter`
   serialized `created_at_unix`, so the launch-time hash and any later
   recompute differed whenever a second ticked — an honest resume
   ALWAYS refused as "tampered" (the unit tests only passed because
   their mock flights were same-second fast). Fixed: the timestamp is
   excluded from the identity hash; regression test
   `charter_hash_is_time_independent` pins it (6/6 resume tests green).
3. **The resume anchored on the checkpoint's LAUNCH dir, not the
   named `--resume` dir.** A resume of a COPY of the run closed the
   ORIGINAL — the tampered copy's deadbeef checkpoint was never read,
   so a full flight completed with exit 0 and the tamper went
   undetected. Fixed: the named dir IS the state home
   (`run_dir` is a LOCATION, not an identity field — removed from
   `config_mismatch`); regression test
   `resume_of_a_copy_anchors_at_the_named_dir` pins it.

The GREEN (kill flight `dr-1786981410`): SIGKILL after round 1, then
against the SAME dir — tampered copy refused (exit 1, "checkpoint
tampered"), conflicting `--max-rounds 5` refused (exit 1, names the
flag), and the bare honest `--resume` typed "continuing at round 2",
closed terminal `done-partial` with rounds [1, 2], and the budget
ledger appends with continuity — every pre-kill entry appears exactly
once, spent never decreases, remaining never increases, and
spent + remaining == allowance per meter (12/12 both meters).

Two instrument fixes in the strips (journaled, not hidden):
- **Strip 4 (constitution)**: the figure gate failed on claim-side
  ellipsis-truncated figures ("£214,0..." in run A's gap-list audit
  notes) even though the semantic figures trace to the evidence
  window. Fixed: `norm_figure` gutter-strips the trailing
  dots/commas NUMERIC_TOKEN's `[\d,.]*` class greedily consumes —
  the honest gate is "the figure's untruncated prefix must appear in
  the evidence", which a fabricated figure still fails.
- **Strip 5 (killed-run shape)**: the killed dir is TERMINAL after the
  green resume closed it, so the resumable shape is proven from the
  artifacts the resume left: checkpoint still `written_after_round =
  1`, round-1 artifacts intact, and the console's typed "continuing at
  round 2" (a terminal dir is refused, never continued).

## Layout

- `source/` — the five NWCI source documents (the corpus leg's originals)
- `red/` — the pre-fix red-watch: flight `dr-1786976220` (console log +
  run dir: no checkpoint, no manifest, stale lock)
- `runs/` — the flight artifacts (run A, run B, the resume pair)
- `verify-demo11.sh` — the artifact strips (exit non-zero iff any strip
  fails; measured failures accumulate)
- `bars.md` — the scene verdicts

Scene 3 (the gate's named gaps drive acquisition) is carried — measured
seven times (t1c..t2c); the demo records the gap-list artifacts without
re-measuring.
