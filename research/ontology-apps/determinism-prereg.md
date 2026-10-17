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
