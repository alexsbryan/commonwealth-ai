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
