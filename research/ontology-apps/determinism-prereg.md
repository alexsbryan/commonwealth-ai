# A repeatable reader (pre-registered 2026-10-16, before any read at the change)

Accepted by the operator 2026-10-16.

**Finding (note 9a25d5fa).** The window reader is the measurement noise and most of Bar R. On fixed records the
bank agrees with itself on 40.3 of 42 questions; fresh reads of one recipe at one code agree on 32.2 (five pairs,
29-34), two recipes on 29.2, the one-shot with itself on 37. DeepSeek ignores temperature in thinking mode, and the
reader runs with thinking on.

**Change (DIAGNOSTIC, uncommitted).** The window reader asks with thinking off and temperature 0
(`window.rs`: `THINKING_TOKENS` 0, `with_temperature(0.0)`). Nothing else.

**Reads.** `ASKER=daemon`, DeepSeek. saltgrass-asked twice (cut of structural-fresh-sg), saltgrass-teacher once (cut
of dev-teacher), the inner ring twice (world-s1, world-s3, ward-story; cut of structural-fresh). Bank judge stamp
77495a0bde1d unless the dump changes it, in which case the stamp is reported.

**Before, at HEAD's reader code (ebdf1eab5).** saltgrass-asked 29 and 30, agreeing on 33; saltgrass-teacher 32;
world-s3 24/27; ward-story 7/14. world-s1 has no read at this code (18/18 at 79ab8ad4a).

**Passes when both hold.**
1. The two saltgrass-asked reads agree on at least 36 of 42 questions.
2. Each saltgrass-asked read scores at least 27, saltgrass-teacher at least 29, and each inner-ring read serves at
   least 40 of 45 on world-s1 + world-s3 and at least 6 of 14 on ward-story.

Reported beside the bars, no bar: deal-level agreement between the two inner-ring reads, claims per window, and
answer tokens per window.

**If 1 passes and 2 fails,** the next step is reading each window twice with thinking on and folding the reads
(the plan of 2026-10-16, step 2). **If 1 fails,** temperature 0 without thinking does not make DeepSeek's reads
repeatable, and step 2 is next either way.

## Step 2: read twice, fold (pre-registered 2026-10-17, after step 1's reads, before any step-2 read)

**Change (prototype, no product code).** `loop/fold_reads.py` folds two recorded thinking reads of each window into
one answer: B's particulars map onto A's by a shared name or, failing that, by evidence (B's claims about it cite
what A's claims about one A particular of its type cite); B's claims are added unless A holds the same kind about
the same particular with an overlapping quote. The fold is replayed through the public commands (`ASKER=replay`).

**Reads.** HEAD's reader (thinking on, ebdf1eab5). saltgrass-asked: voice2-sg1 and voice2-sg2 (fold a) and two new
fresh reads (fold b). saltgrass-teacher: voice2-teacher and one new fresh read (fold t). Judge 77495a0bde1d.

**Passes when all three hold.**
1. Fold a and fold b agree on at least 36 of 42.
2. Each asked fold scores at least 31 (single reads at this code: 29, 30), and fold t at least 32 (single: 32).
3. Folds a and b each agree with fold t on at least 33 (single reads at this code: 31 and 30).

## Step 3: smaller windows (pre-registered 2026-10-17, after step 2's reads, before any step-3 read)

**Step 2's result.** Asked folds 31 and 34 agreeing on 35, teacher fold 34, cross 31 and 32: bar 2 passed, 1 and 3
failed. The folds disagree on facts only one of them holds, so a read's chance of writing a fact is the noise.

**Change (DIAGNOSTIC, uncommitted).** `window_manifest::WINDOW_CHARS` 20,000 (40,000), re-cut by `INIT=1`; the reader
is HEAD's (thinking on). Nothing else.

**Reads.** saltgrass-asked twice, saltgrass-teacher once, fresh (`ASKER=daemon`). Judge 77495a0bde1d.

**Passes when all three hold.**
1. The two asked reads agree on at least 36 of 42 (single 40k reads at this code: 33-34 over six pairs).
2. Each asked read scores at least 31 and teacher at least 32 (40k singles: asked 29, 30, 26, 32; teacher 32, 27).
3. The mean agreement of the two asked reads with teacher is at least 33.25 (40k singles: mean 33.25 over eight
   pairs).

Reported beside the bars, no bar: windows, claims per window and in all, answer tokens per window. Mail is not read
here; a pass is a candidate default only after the inner ring is read at 20k.

## Step 4: 20k windows on mail and on the held-out recipes (pre-registered 2026-10-17, before any read)

**Step 3's result.** All three bars passed: asked 37 and 37 agreeing on 36, teacher 36, cross 36.0.

**Change.** As step 3 (DIAGNOSTIC `WINDOW_CHARS` 20,000, `INIT=1` re-cut, HEAD's reader). Approved by the operator.

**Reads.** One fresh read of the inner ring (world-s1, world-s3, ward-story). saltgrass-ledger twice; detective, editor,
engineer once each. Judge 77495a0bde1d for the banks.

**The change is a candidate default when all three hold.**
1. Mail guard: world-s3 + ward-story serve at least 29 of 41 (HEAD's reader, fresh at 40k: 24 + 7). world-s1 is
   reported, no bar (it flips 7 of 18 between reads of one code).
2. Bar G (generality-prereg.md): saltgrass-ledger's mean is at least the one-shot's 25.5 minus 2.
3. The mean cross-recipe agreement over the ten pairs of asked, ledger, detective, editor and engineer is at least 32
   of 42 (40k: 29.2).

Reported beside them, no bar: Bar R as generality-prereg.md scores it, its yardstick pooled from the 20k asked and
ledger pairs (40k: 3 of 10 pairs pass); the largest call's prompt and answer tokens.
