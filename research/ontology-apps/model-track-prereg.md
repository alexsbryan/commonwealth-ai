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

**Step 1 result (2026-10-20 04:40): neither size recovers windows 2-3; plan step 2.** Windows 2-3 attached
(written) of 27, window 1, total of 55, served: `-w10k-a` 11 (13), 21, 32/55, 4/15, 47 min; `-w10k-b` 11 (15), 17,
28/55, 1/15, 48 min; `-w5k-a`, recovered (`-w5k-a-retry`: window 17 answered on the retry, 6 claims) 12 (18), 16,
29/55, 2/15, 50 min plus 20; `-w5k-b` 14 (14), 21, 35/55, 4/15, 70 min, no window lost. Three of four reads fall
under the no-harm floor. The prediction held for 10k and failed for 5k. The 20k reads wrote 16 and 18, so the
reader writes no more of windows 2-3 on smaller calls. The recovery first found `--retry-failed` refusing a
checkpoint journal never reset since before a schema change (fixed, a7c6213ee).

## Step 2: a second look at the same window (pre-registered 2026-10-20 04:45, before any read)

**Why.** Step 1 recovers neither size: `-w5k-b` attaches 14 of windows 2-3's 27 and wrote 14, under 20 whatever
`-w5k-a` gives, and 10k writes 13 and 15. A call over fewer documents does not write more, so the step 1 assumption
is wrong. What the reads say instead (`windows/`, no model call): the misses vary by read. Eight local reads
each write about 17 of the 27 (11-18 attached), and together they write 24. Only 3 are never written, one of
which DeepSeek also misses. The misses read are considered and declined, not overlooked: a forwarded "signed off,
book it" judged a quote (`basaltville/1`), a credit markup judged no stage (`bitter_bend/3`). So a second look at the
same window is a second draw at its margin. As independent draws at the eight reads' rates, two looks write
about 20 of 27, before RESOLVE's loss of 2-3.

**The change.** `window_rest.rs`: each window is asked twice. The second prompt is the first plus the first
answer's statements, asking for the rest. Code folds the second answer in (the first's particular named again is
the first's, a ref reused for another is renamed apart, and a claim already written is dropped). A second answer
that does not parse leaves the first's, traced. It runs on every window read (DeepSeek's too), and it is reverted
if it does not keep.

**Reads.** `local35b-s4-rest-c` and `-rest-d`, paired: each restores `-c`'s or `-d`'s 20k cut and answers
(`RESTORE`), so the first look replays that read's recorded answer (the reader's prompt is unchanged since
76236b761), and only the second look and RESOLVE are asked fresh (`ASKER=recorded RESOLVE_ASKER=daemon`). A
difference on a window is then the second look's alone. `run.json`'s store count checks the pairing: one new
answer per window plus RESOLVE's. `svrn-ingest` is built at the change without the working tree's alias-stamp diff.

**Smoke.** First, 20k window 3 (`sec_00003`, 9 updates) with `-c`'s cut and answers through `enrich extract
--chapters sec_00003 --asker recorded`. The first look replays (no new store line for it), the second is asked,
folds and adds claims, nothing is lost. Minutes reported. A failed smoke stops the step and carries no count.

**Bars.** Valid as before: no window lost (a lost second look is reported, the window read on its first), RESOLVE
answered. The second look keeps when both reads attach at least 20 of windows 2-3's 27 (`-c`/`-d`: 13, 14).
No harm: each read at least 33/55 attached and 4/15 served. Window 1 is reported against 20/27. Written (score.json
`matches`) is reported beside attached, against `-c`/`-d`'s 16 and 18.

**Decisions.**
- Both reach 20 with no harm: the second look stays. Next, two fully fresh reads against Done (8/15, 45/55). How it
  serves DeepSeek is decided before it ships, and a hosted read waits for the operator.
- Exactly one reaches 20: one fully fresh read decides.
- Neither reaches 20: the second look is reverted, and the plan's pivot is next (Qwen3.8-27B on windows 2-3).

**Reported per read.** Windows 2-3 and window 1 on the 20k cut, written and attached, attached of 55, served of
15, statements, claims the second look added and dropped as repeats per window, minutes.

**Prediction**, stated so it can be wrong: the second look adds at least 3 written updates on windows 2-3 on each
read, and neither read reaches 20 attached (RESOLVE loses 2-3 of what is written).

**Step 2 result (2026-10-20 05:30): the second look does not keep** (reverted, ea8c8607c). Both reads valid, paired
as registered (extract asked only second looks; `-rest-c`'s window 3 second look was the smoke's, recorded).
Windows 2-3 attached (written): `-rest-c` 15 (17) against `-c`'s 13 (16); `-rest-d` 13 (20) against `-d`'s 14 (18).
Window 1 25 and 21; 41/55 and 34/55; 5/15 and 3/15 served; 23 and 21 min. The second look added 9 and 6 claims in
six windows, and 31 rewritten claims were dropped as repeats. The prediction's gain failed (+1, +2 written), and
its "neither reaches 20" held. The wrong assumption: a second look is a second draw. Shown its own answer, the
model re-reads every document and repeats its declines (`-rest-c` window 2: d1's forwarded "it's off" judged a
quote again). The eight reads' spread comes from the first look's reasoning path, not from a look at the same
frame. Next, per the plan, the pivot: Qwen3.8-27B on windows 2-3.

## Pivot: Qwen3.8-27B (pre-registered 2026-10-20 05:45, before any read)

**Why.** Steps 1-2 leave windows 2-3 under 20/27 on the 35B-A3B by every narrow call tried (10k, 5k, a second look).
The plan's pivot reads them with the dense 27B to tell the model from the design.

**Reads.** `local27b-s4-a` and `-b`: world-s4 at 40188f8cd (the one-look reader, `svrn-ingest` rebuilt at it,
without the alias-stamp diff), `-c`'s 20k cut restored. The daemon's primary is `Qwen3.8-27B-UD-Q6_K_XL.gguf`
(context 131072, the fast and embed slots unchanged, `SOVEREIGN_INFERENCE_TIMEOUT_SECS=3600`), so both the window
reader and RESOLVE run on it; daemon askers, fresh; `RETRY=1` per the step 1 amendment.

**Smoke.** First, 20k window 3 (`sec_00003`) through `enrich extract --chapters`: the answer parses, nothing is
lost, minutes and memory (the 27B at 26 GB beside the fast slot on 64 GB). A failed smoke stops the pivot and
carries no count.

**Bars.** Valid as before. The model is the lever when both reads attach at least 20 of windows 2-3's 27 (the
35B's best local read: 17). No harm: at least 33/55 and 4/15. Window 1 against 20/27, written beside attached,
minutes per read.

**Decisions.**
- Both reach 20: the dense windows are model-bound on the 35B, not design-bound. These two reads are also judged
  against Done (8/15, 45/55). Step 3 (minutes, a 1M-token corpus) is then priced on the 27B.
- Exactly one: one more read decides.
- Neither: per the plan's stop, the track is model-bound today. The numbers are recorded and engineering on this
  track stops for the operator.

**Prediction**, stated so it can be wrong: both reads attach at least 18 of windows 2-3, at least one reaches 20,
and a read takes over two hours.

**Smoke result (2026-10-20 13:35).** Failed, so the pivot stops and carries no count. `sec_00003` on the 27B hit
the daemon's 3600s deadline with 42,924 tokens generated (11.9 tok/s; the 35B decodes the same reader at about
77) and no answer; swap 18.8 of 19.5 GB. The smoke was mis-sized before it ran: the reader's thinking budget alone
(48,000) takes 67 minutes at that rate, so no window that thinks to budget fits the hour. Windows 2-3 on the 27B
are could-not-judge, not failed. To measure them needs a deadline of at least 2 hours per call and, at 3 windows a
read, an estimated 8-10 machine hours for both reads. That spend is the operator's call. Until then the plan's stop
stands as written: the 35B's best local read is 17 of 27, and engineering on this track stops.

**Amendment (2026-10-20 07:36, before any read).** The operator directed the pivot on: the smoke and both reads run at f55e4aeb6, whose window
reader reasons up to 22,000 tokens (was 48,000), just above the 35B's longest reasoning on these windows (21,241
over 47 calls). The daemon closes the 27B's think block there and it answers, so the 27B is never held tighter
than the 35B was. Nothing else changes: same cut, askers, bars and decisions. Price at 11.9 tok/s: at most about
35 minutes per window, so a read of 6 windows plus RESOLVE runs to about 4 hours and both to about 8. The smoke is
repeated on `sec_00003` under the same bar, and a second failed smoke stops the pivot.

**Amendment, minimal (2026-10-20 07:55, before any read).** On the operator's direction, one comparison instead of
two full reads. The 27B reads windows 2-3 alone (the smoke's `sec_00003`, then `sec_00002`). Its two answers are
spliced into `-c`'s read (`questions-full-061`, the 35B's other windows untouched), and RESOLVE runs fresh on the 35B,
so only the reader of windows 2-3 differs from `-c`. Against the 35B's seven scored reads of this cut (attached on
windows 2-3: 10-17, median 13; written: 10-20, median 17):
- At least 20 attached, above every 35B read, means the model is the lever, and a full read on the 27B is worth its hours.
- At most 17, inside the 35B's range, means the model is not the lever on this design, and the stop stands.
- 18-19 is a lean, not a verdict.
One read, so this extrapolates and does not judge.

**Result, minimal (2026-10-20 09:13).** The model is the lever. Out of 27, windows 2-3 attached **24** (the 35B's
seven reads: 10-17), and the reader wrote 24 (the 35B: 10-20). Whole read with the 35B's other windows: **8/15
served, 50/55 attached**, at Done's bar (8/15, 45/55; DeepSeek 9/15, 52/55); `-c` alone was 39/55. The 27B wrote 28
claims on window 2 and 16 on window 3 (the 35B: 11-17 and 2-8). Nothing was lost, and both reads hit the
22,000-token reasoning budget before answering.

Cost: 39 and 40 minutes a window at 10-11 tok/s, against the 35B's 4-5. RESOLVE ran on the 35B in 15 minutes.
Swap peaked at 22.2 of 23.5 GB. This is one spliced read, so it extrapolates. Next is a full 27B read, priced at
about 4 hours of windows plus RESOLVE.

Data: `runs/loop/local27b-s4-w23/`, `runs/loop/model-track-pivot/`.
