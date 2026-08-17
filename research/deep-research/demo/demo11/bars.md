# DEMO-11 — the deep-research journey bar, measured (order deep-research-t3a)

The bar `dr-journey` (quality/initiative-bars.toml) and its six scenes,
measured through the SHIPPED `svrn deep-research` surface. Verdicts by
the measured outcome, never by intent (Amendment C — measured failures
are the measurement). All numbers artifact-derived (verify-demo11.sh
re-derives them; nothing hand-typed).

## The runs

| flight | run id | shape | terminal |
|---|---|---|---|
| run A (Q1) | `dr-1786978547` | mock deck, budget 12/12, 3 rounds, consent personal | `done-partial` |
| run B (Q2) | `dr-1786979346` | `--corpora dr-estate-dr-1786978547 --search-source corpus`, budget 12/12, 3 rounds | `done-partial` |
| resume kill | `dr-1786981410` | mock deck, budget 12/12, SIGKILLed after round 1 (`written_after_round = 1`), resumable shape verified at kill time | (interrupted) |
| resume | `dr-1786981410` | `--resume` (bare), continues at round 2, closes the SAME dir | `done-partial`, rounds [1, 2] |

## Scene 1 — question + budget + consent — MET

Run A asked the frozen v1 bank question with an explicit budget
(`--search 12 --fetch 12 --max-rounds 3`) and a typed consent grant
(`--consent personal`); the manifest records the grant
(`consent.release-floor: personal`). The budget ledger journals every
decision to the run dir (the ledger arithmetic is re-verified per meter
in scene 4). Default-deny remains: a flight without `--consent` refuses
non-public-web payloads (measured on the resume reds' console lines —
"no consent grant — the web leg is default-deny").

## Scene 2 — prior corpora surveyed before any network call — MET

Run B's `--corpora dr-estate-dr-1786978547` (run A's estate, E) was
surveyed BEFORE any acquisition: survey-1's `estate_precondition`
asserts `estate_searchable: true`, and every round-1 hit carries
`corpus_id = dr-estate-dr-1786978547` with
`estate:dr-estate-dr-1786978547:<chunk>` locators — the survey artifact
is round 1's first write, before any fetch.

## Scene 3 — the gate's named gaps drive acquisition — MET

Run B's round-2 gap-list exists with its named gaps, and round-2's
queries show the strip-3c figure-stripping on the gap queries (the
frozen instrument space — nothing changed in this order). Acquisition
continued from the gap list with the frozen admission decider.

## Scene 4 — resume: interrupted at N, continues at N+1 with ledger continuity — MET (measured over two reds and a green)

The kill shape: flight `dr-1786981410` SIGKILLed after round 1 —
checkpoint.json present (`written_after_round = 1`), NO
manifest/verdict-set/report (the resumable shape), the stale `lock`
left by the kill (F19: a live flock refuses, a dead process's file is
acquirable — the resume re-acquires it). The resumable shape is pinned
post-resume too, from the artifacts the resume left behind: the
checkpoint still reads `written_after_round = 1`, the round-1
artifacts (draft-1, gap-list-1, survey-1, evidence-window-1,
fetch-list-1) are intact, and the resume console typed "continuing at
round 2" — a terminal dir is refused with a typed refusal, so the
continuation line is the instrument's own proof the dir was resumable
at resume time (verify-demo11.sh strip 5).

The typed refusals (each exit 1, console in runs/resume/):
- tampered copy (charter_hash → `deadbeef`) — "checkpoint tampered …
  refused, never silently restored";
- conflicting re-pass (`--max-rounds 5`) — "resume mismatch …
  --max-rounds 5 differs from the checkpoint's 3".

The honest bare `--resume dr-1786981410` (no flags re-passed —
not-passed flags inherit the checkpoint's values) continued at round 2
and closed terminal `done-partial` with rounds [1, 2]. Ledger
continuity (verify-demo11.sh recomputes from the journal, against the
pre-resume snapshot the driver captured): the pre-kill entries appear
exactly once in the final ledger, spent never decreases, remaining
never increases, and per-meter `spent + remaining == allowance` holds
(identical budget arithmetic across the resume).

**The three reds that made this honest** (full records in
adversarial/pre-registration.md):
1. The flag gate compared CLI defaults for not-passed flags against the
   frozen config — a bare `--resume` refused (`--search 4 differs from
   the checkpoint's 12`). Fixed: not-passed flags inherit the
   checkpoint's values; only explicitly-passed flags are verified.
2. `hash_charter` serialized `created_at_unix`, so the launch-time
   charter hash and any later recompute differed whenever a second
   ticked — an honest resume always refused as "tampered". Fixed: the
   timestamp is excluded from the identity hash (regression test
   `charter_hash_is_time_independent`, 6/6 resume tests green).
3. The resume anchored on the checkpoint's LAUNCH `run_dir` instead of
   the operator's named `--resume` dir — a resume of a COPY of the run
   closed the ORIGINAL (`dr-1786980365`, measured red: the deadbeef
   tamper copy completed a flight with exit 0 because the tampered
   checkpoint was never read). Fixed: the named dir IS the state home —
   `run_dir` is a location, not an identity field (the charter never
   included it; removed from `config_mismatch`); regression test
   `resume_of_a_copy_anchors_at_the_named_dir` (a faithful copy resumes
   into the COPY, the original stays untouched; a deadbeef-tampered
   copy refuses and writes no state).

## Scene 5 — checked report, zero untraced figures in [passed] position — MET

The constitution strip (verify-demo11.sh, the demo10 decider: the
scorer's own NUMERIC_TOKEN, citation tails cut at the earliest citation
marker, presence = substring of the joined evidence window) across run
A (136 claims, 1 passed-position) and run B (27 claims, 0 passed):
zero untraced figures in [passed] position.

## Scene 6 — the estate write-path + the compounding value — MET, with the measured boundary

Write-path (run A): every fetched source is stamped
`ingested_into = dr-estate-dr-1786978547` on the manifest, and the
estate corpus exists with `_corpus_meta.json indexes_built: true` —
listing AND retrieval-visible through the shipped surface (`svrn
corpus search dr-estate-dr-1786978547 "electrification cost"` returns
hits) with NO manual ritual.

Compounding value (run B): draft-1.json — the survey's estate_answer,
synthesized from E ALONE — carries all four pre-registered Q2
specifics (£223,400 final cost; 2,314,807 opening-year passengers;
cable-haulage engineer Amelia Voss; £88,500 electrification). The
local cache answered a question the deck cannot.

**The measured boundary (journaled, not smoothed)**: run B's checked
verdict-set is 27/27 could-not-judge with ZERO passed claims — the
frozen admission decider (quantized-bucket triage, threshold 0.03333)
admitted only 2 of E's chunks into round 1's evidence window, and the
frozen corroboration floor (dr-corroboration, F22) caps single-origin
support at could-not-judge: the dr-compass structural cause, measured
seven times (t1c..t2c). The report cites 24 `[Source: estate-N]`
labels; the full `estate:dr-estate-<runA>:<chunk>` locator links
render in passed position only (measured zero). The pre-registered
"on passed claims" citation sub-clause is NOT met, cause named; the
estate-first retrieval and the estate-synthesized draft ARE met — the
compounding mechanism works, and the estate's chunks become
passed-position evidence when a multi-origin corpus gives the
corroboration floor a second origin (the banked dr-compass re-cut).

## Verdict

| scene | verdict |
|---|---|
| 1 question + budget + consent | met |
| 2 prior corpora surveyed before network | met |
| 3 gate gaps drive acquisition | met |
| 4 resume at N+1, ledger continuity, typed refusals | met (over three reds) |
| 5 constitution, zero untraced figures in [passed] | met |
| 6 estate write-path + compounding value | met; "on passed claims" sub-clause not met (cause named) |
