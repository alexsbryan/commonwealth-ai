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
