# map-conversion rung 6 (2026-09-08): the judged answer comparison

Pre-registered BEFORE the first run (bars must exist before the data).

Instrument: `svrn eval run --bank sovereign/bench/sep/questions.toml --synth --format json
--isolate --output <arm>.json`, judge on (default instructor-mode pass). ISOLATED to `sep`:
the first un-isolated attempt (`armB-unisolated-aborted.log`, killed after question 1) drew
18 of 31 evidence chunks from `raptor-subset-*` experiment copies created 2026-09-05, which
did not exist for the July baseline — so an un-isolated arm measures the experiment copies,
not SEP. The committed baseline is un-isolated: `sovereign/bench/sep/baselines/questions-synth/2026-07-06.json`
(judge ratio mean 0.916, keyword-facts-in-answer 0.796, 46 min wall); the isolated and
un-isolated May baselines read the same judge mean (0.897), so the comparison holds with
that caveat named. Arm A vs arm B is the paired comparison and needs no caveat. Ledger captured at
`RUST_LOG=warn,retrieval_audit=debug` so rows fired per question is readable.

Arms:
- **B** — binary at 14:49 2026-09-08 (rungs 3+4 uncommitted): SEP siblings walk under
  philosophy's declared rows via the loader's config.json fallback; 10/21 classify
  (`../map-conversion-rung3/kind-sep-philosophy.txt`).
- **A** — same lane at HEAD a10093594 (pre-rung-3 binary from a worktree): pre-registered
  rows, 6/21 classify, 15 unfiltered.

Bars (per arm, judge ratio mean over 21):
- B ≥ 0.916 (the July baseline) — else the declared rows are a regression on judged answers.
- B ≥ A on the six questions named before the run: consequence_argument, gettier_lottery,
  kripke_reference, berlin_liberty, aristotle_hylomorphism, bioethics_principlism.
- Both directions reported per question; n=1 first, n=2 only if the first run's verdict
  is within 0.02 of a bar.

Reading rule: judge ratio is the headline (answer conveys the fact); keyword-in-answer is
secondary; retrieval facts/sources are NOT the bar here — they already read 152/159.
