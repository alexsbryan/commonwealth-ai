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

## Run record (2026-09-08, session 955ed251)

Both arms now run on the CONVERTED stores (rung 3 run, commit 75b51cd70):
until that run 662 of the 1,770 siblings were on CSR v1, which the v2 reader
refuses with no fallback (`load skipped`, DEBUG), so an arm on the
pre-conversion stores walked 1,108 atlases while the July baseline saw 1,770.
Arm A therefore = the pre-rung-3 binary (`/home/alexbryan/dev/cw-armA` @
9d3a94831, target `cw-armA-target`) with every `sep-*/atlas/ontology.json`
renamed `.armA-hidden` for its duration (`run-arms.sh` hides and restores under
a trap; a pre-rung-3 binary reads a typed map as Declared, and the current
binary's loader fallback supplies philosophy's rows even with no file).

NOT RUN TO COMPLETION. Arm B was started four times and the kernel OOM-killed
the daemon (exit 137) each time — 16:42, 17:14, 17:41, 18:00 — at the lane's
first synthesis or within 12 questions. Kernel Mem-Info at the 17:41 kill:
anon 52 GB, file cache <0.1 GB, free 0.4 GB, swap 8 GB full; the rest of the
125 GB is GPU-pinned system memory (GTT read 72.6 GB with every slot resident:
35B 28.6 GB + 4B + FastShort 4B + embed + KV/compute), which process RSS and
MemAvailable do not show until the 35B loads. Resident beside the daemon at
the kills: two or three rust-analyzer instances (22-26 GB) and, at three of
four, a peer session's `cargo check --workspace` / `cargo test -p
sovereign-core`. The eval process itself was 1.8 GB. `armB.oom3.log` /
`armB.oom4.log` are the killed runs; `run-arms.out` the guard trace.

`run-arms.sh` warms the 35B before its guard (so its memory is counted),
waits for no cargo/rustc/scip and MemAvailable ≥ 12 GB, fails an arm on any
`turn: Inference error` line (eval run exits 0 over them — §18.3, open), and
retries up to 3 times. It still cannot finish while ~26 GB of rust-analyzer
is resident. To resume: free that memory (close the IDE's rust-analyzers, or
pause the peer session), then `setsid nohup $D/run-arms.sh > $D/run-arms.out
2>&1 &` and `python3 $D/compare.py` when both WALL lines land.
