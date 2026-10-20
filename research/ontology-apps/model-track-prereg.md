# The model track, step by step (pre-registered 2026-10-19)

Plan: `REVIEW_BRIEF.md` step 4, "The plan (2026-10-19)". Each step's bars are written here before its read.

## Step 0: the baseline at HEAD (before any read)

**Reads.** `runs/loop/local35b-s4-base-a` and `-b`: world-s4 at 554a70a47 plus this file, `svrn-ingest` and the
daemon rebuilt at it, the daemon serving Qwen3.6-35B-A3B-UD-MTP-IQ4_NL (128k, the 2026-10-15 local config), the
shipped `window_chars` (20,000), `INIT=1`, `ASKER=daemon RESOLVE_ASKER=daemon`. Both askers are `daemon` because the
answer store stamps an answer with the routed alias (`commonwealth/primary`), not the model that served it, so under
`recorded` a local read reuses DeepSeek's stored answers (`asker.rs`, `AnsweredBy`). The reads run one after the
other, and nothing is built between them.

**A valid read.** Extract and resolve exit 0; every window returns an answer that parses (none lost to an empty
answer, a timeout or a malformed value); RESOLVE's questions are answered. A read that loses a window to a system
defect is could-not-judge, and the defect is the next fix, before step 1.

Checked before the reads, so not a reason to call one invalid: on this daemon `json_object` with thinking on still
returns `content: ""` (note 068f6b1f), but neither reader sends the pair. The window reader thinks with no schema;
RESOLVE asks under a schema with thinking off (`inference_client/wire.rs:35-45`), and a probe of that shape returns
its answer.

**Reported per read.** `window_recall.py` per window, deals served of 15, updates attached of 55, statements
written, minutes.

**Decisions.**
- Both reads at 8/15 served or more and 45/55 attached or more: world-s4's Done bar holds at HEAD. Steps 1 and 2
  are skipped and the inner ring is next.
- Otherwise step 1, on window 1. The two reads' window-1 counts are the band step 1's bar is set against.

**Prediction**, stated so it can be wrong: window 1 under 20/27 on both reads (12/27 on 2026-10-15, before
bb16808b5).

**Amended 2026-10-19 20:05, before any valid read.** The first read (`local35b-s4-base-a-deadline300`) lost windows
1-3 to the daemon's 300s default inference deadline (503 after 7-8k tokens each; phase 1 stops at three in a row), and
the second was stopped. The 2026-10-15 reads ran the daemon with `SOVEREIGN_INFERENCE_TIMEOUT_SECS=3600`, set by hand
(5800027c1); the local config swap does not carry it. Reads `-a` and `-b` run again on a daemon started with 3600,
nothing else changed.

**Amended 2026-10-19 21:25, after reads `-a` and `-b`, both could-not-judge.** `-a` parsed every window but filed
the stage under the answer shape's `grade` slot (an object), so no record kept one (0/15 served; 32/55 attached,
window 1 22/27). `-b` did the same as a string and lost window 1 whole to malformed JSON (6 of 21 particulars
missing a brace), so extract exited 1 and nothing was scored. Fixed: bbcccac14, c5825d1dd. Not fixed: one malformed
element still loses its window. Reads `-c` and `-d` at c5825d1dd, same daemon, same bars; a window lost to malformed
JSON is reported as such, and a read that loses one is could-not-judge as before.

**Step 0 result (2026-10-19 22:41).** `-c`: 4/15 served, 39/55 attached (windows 25/27, 11/18, 2/9), 52 statements,
37 min. `-d`: 4/15, 33/55 (19/27, 9/18, 5/9), 57 statements, 41 min. Every window parsed; RESOLVE answered. Below
Done, so step 1. The prediction failed for `-c` (window 1 at 25/27). Two identical reads differ by 6 updates on
window 1 and 6 in total, so one read cannot carry a step's verdict. Window 3 (`-c`): every document named, d7-d15
reasoned over 1-5 times each against DeepSeek's 11-114; the answer is what the reasoning drafted.

## The answer held to a schema (pre-registered 2026-10-19 23:20, before any read)

**Reads.** `local35b-s4-schema-e` and `-f`: world-s4 at dd81fc4da (the window call carries its answer's schema;
the daemon holds the answer to it after the think block, 2b9c1b527), otherwise as `-c`/`-d`.
**Bars.** Valid: no window lost, RESOLVE answered. No harm: each read at least 33/55 attached and 4/15 served
(the `-c`/`-d` floor; their spread is 6). A read under the bar is the schema costing recall: examined before it
stays, never kept on its structural merit alone. Reported: windows lost, statements, minutes (the mask's cost).

**Result (2026-10-20 00:25): the schema is not kept** (reverted, b836d3b00). Both valid, no window lost. `-e`:
4/15, 42/55 (25/27, 14/18, 3/9), 43 min. `-f`: 1/15, 29/55 (17/27, 9/18, 3/9), 30 min, under the bar. Examined:
`-f`'s window 1 wrote 21 stage updates (26, 24, 25 in `-c`, `-d`, `-e`), so the gold updates its claims match fell
to 35/55 (44, 41, 42), and RESOLVE lost 6 more (3/9 decided right). The RESOLVE part is outside the window call;
the reader part is not explained.

## Step 1: window size over the dense windows (pre-registered 2026-10-20 00:50, before any read)

**Why windows 2-3.** Step 0 moved the gap: window 1 reached 25/27 and 19/27, while windows 2-3 attached 13 and 14 of
their 27 updates (`-c`, `-d`) against DeepSeek's 25 at 20k (`w20-s245`; 26 at 40k, `w40-head`). In `-c`'s window 3
every document is named and d7-d15 are reasoned over 1-5 times each, so the plan's assumption stands as stated:
the loss is per call, and a call over fewer documents writes more of what each holds. It is wrong if these
documents read at 5k still leave most of their updates unattached.

**Reads.** `local35b-s4-w10k-a`, `-w5k-a`, `-w10k-b`, `-w5k-b`, in that order, one launchd job
(`runs/loop/model-track-step1/`): world-s4 at b836d3b00 (the reader `-c`/`-d` read with), `svrn-ingest` rebuilt
at it and nothing after, the daemon and askers as for `-e`/`-f`.
The cut is the product's: `enrich init` over world-s4's index from a copy of its recipe with `window_chars` set,
under a diagnostic id since removed, saved with the recipe copy at `runs/loop/cut-s4-w{10,5}k/` and put back with
`RESTORE`. World-s4's own recipe is untouched. 10k cuts 11 windows (20k windows 2-3 become its 3-6 and the first
document of 7) and 5k cuts 24 (its 5-14; its 5 opens with window 1's last document), so every count is taken on the
20k cut, each message by its date (`window_recall.py --by local35b-s4-base-c`).

**Smoke.** First, one window at each size through `enrich extract --chapters` (10k `sec_00005`, 9 window-3
updates; 5k `sec_00010`, 4): the answer parses, nothing is lost, minutes. A failed smoke stops the step and
carries no count.

**Bars.** Valid: no window lost, RESOLVE answered. The band is the 20k reads at this reader, `-c` and `-d` (windows
2-3: 13 and 14 of 27), with `-e`/`-f` (17, 12) and the 2026-10-15 reads (15, 19) beside them. A size
recovers windows 2-3 when both its reads attach at least 20 of the 27, above every local 20k read on any code and
5 under DeepSeek. No harm, as in step 0: each read at least 33/55 attached and 4/15 served. Window 1 is reported
against the plan's 20/27.

**Decisions.**
- One size recovers windows 2-3 with no harm: size is the lever for the dense windows, and that size is read
  against Done (8/15, 45/55, two reads). How it would ship, the shipped default or a size bound to the serving
  model, is decided against DeepSeek at the same size first (DeepSeek attaches 52 at 20k); never a recipe edit.
- Both sizes recover: 10k goes forward (half the calls), 5k's minutes reported beside it.
- Neither recovers: size is not the lever, and the plan's step 2 (a second pass over the same window) is next.

**Amended 2026-10-20 02:40, after `-w10k-a` (11/27 on windows 2-3, 32/55, 4/15) and `-w5k-a`, before any other
result.** `-w5k-a` lost 5k window 17 (inside 20k window 4, outside windows 2-3): the model broke off its JSON
mid-array to correct a typo in an address, restarted and looped, so the answer held no object (the case the
reverted schema closed). As read it is could-not-judge. Without the schema 1 window in 47 has been lost (`-c`,
`-d`, `-w10k-a`, `-w5k-a`), so about 4 in 10 reads of 24 calls lose one. A read that loses windows is recovered
through the product's own path and reported as recovered: its cut and answers restored, extract replayed (the lost
window fails again as it did), `enrich extract --retry-failed --asker daemon` asks the lost windows once more,
RESOLVE fresh (arm `<read>-retry`). A window lost twice leaves the read could-not-judge.

**Reported per read.** Windows 2-3 and window 1 on the 20k cut, attached of 55, served of 15, statements, windows
read and lost, minutes.

**Prediction**, stated so it can be wrong: 5k recovers windows 2-3 on both reads and 10k on neither (10k's first
window over them holds 10 documents, past the six `-c` reasoned over closely).
