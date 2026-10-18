# Lean D step 1: RESOLVE decides what the window reader bound across documents (pre-registered 2026-10-17)

Approved by the operator 2026-10-17 ("lean D ... and a rigorous test of validity"). Design:
`svrn/docs/specs/ONTOLOGY_METHOD.md` §Identity, "What the window reader bound is RESOLVE's to decide". Diagnosis:
`diagnosis/README.md`.

**Change.** The window's reference (`CHAPTER_SUBJECT_KEY`) stays sufficient within one document and becomes evidence
across documents: a statement whose window particular already holds a statement of another document is asked, its
window-mate's record offered first and marked as the reader's reading; a record wholly of the window's other
particulars is offered, marked, instead of hidden (`resolve_records/apart.rs`, the drive's link-or-ask branch).
Nothing else. Expected size ~80-120 lines and ~150 of tests, no file in the 800-1200 band touched.

**Code first.** A test that fails before the change on the same input it is named for: two documents of one window
whose reader bound one particular across them, the second of which is another deal's, are asked and kept apart; a
window split (two particulars of one deal in two documents) is offered and joined. The contract tests C1-C5 pass,
C4 (any document order) included.

## Validity, before the result (each step's verdict is passed, failed, could-not-judge or never-ran)

1. **The instrument reproduces.** At HEAD without the change, the saved inner-ring reads replayed (window answers
   checked identical to the reference arm's, RESOLVE `--asker recorded`) give the reference arms' score files and the
   diagnosis tables exactly (`diagnosis/identity_where.py`, `new_thread.py`, `self_desc.py`). A difference is an
   instrument defect, fixed before anything is measured.
2. **The noise band.** At HEAD without the change, RESOLVE re-asked fresh (no recorded answer reused) twice on the
   same windows: the per-corpus range of deals served and of identity errors that RESOLVE's own variation makes. A
   result inside the band is reported as no effect, whichever way it points.
3. **The change, replayed.** Same windows, the change built, RESOLVE `--asker recorded` (questions asked before reuse
   their answers, the new ones go to the daemon). Reads: the inner ring's current reads, beside-s1 (world-s1),
   w20-inner (world-s3), nest-w1 and nest-w2 (ward-story). Budget: about 100-150k answer tokens; printed before
   launch.

## Passes when all hold

1. Identity errors among statements the reader bound across documents fall by at least half, pooled over the inner
   ring, and by more than the noise band.
2. Deals served fall on no corpus by more than its noise band.
3. Statements that fully describe their particular are placed no worse than the noise band allows.
4. C1-C5 pass; the window answers of every replay are identical to their reference arm's.

Reported beside the bars, no bar: the partial-statement cells of the diagnosis (shared subject, new thread, no
history, first message), the stable-gap deals, RESOLVE questions added and their answer tokens, decisions judged
right and wrong by `decisions.py`, and every deal that changed, with its chain.

**After the bars.** Passed: the five worlds and saltgrass-asked are replayed the same way (development set), and only
then the held-out saltgrass recipes. Failed: the result and its diagnosis by class are reported, and the next ring
(the two-pass re-read, `diagnosis/README.md` family 2) is priced against it.

**Result (446136f33 replayed on the inner ring; ~201k answer tokens over 658 RESOLVE questions, counted as
characters / 4, thinking not metered). Fails: bar 2 fails, bar 1 could not be judged, bars 3 and 4 pass.**
Validity 1 passed with one named substitution: `leand-0` reproduces beside-s1 (world-s1), nest-w2 and nest-w1
(`leand-0-w1`, nest-w1's read made latest by `loop/latest.py`) in windows, atlas, misses and score, and their
decisions as today's `decisions.py` judges them. world-s3 reproduces itself (`leand-0r`, no question asked) but not
w20-inner, which was read before 3175c3f6c and scored by an older `score.py` (87 against 86 of 88 updates matched,
identity errors 0 and 0), so world-s3's control is `leand-0`. Validity 2, deals served under the control and two fresh
RESOLVE passes (`leand-nz1`, `leand-nz2`): world-s1 12/14/14, world-s3 27/27/27, Ward nest-w2 6/6/8, nest-w1 7/7/7.
The change (`leand-1`, `leand-1-w1`): 13, 26, 9, 7, pooled 55 of 73 against 52, 54 and 56 without it.
Bar 1: the inner ring's reads hold one identity error the reader bound (coyoteville-d2, a rollover), before and after,
in every pass. The diagnosis's 78 are 53 from reads of 2026-10-14, 13 from the local 35B reads and 12 from the 17
world reads since 2026-10-16 (`ARMS=<arm>/<corpus> identity_where.py`). Bar 2: world-s3 falls from 27 to 26 against a
band of 0 (velica-d1). Bar 3: 24 fully described updates, 0% wrong and 0% unplaced in every arm. Bar 4: C1-C6 8 of 8;
all 12 replays' windows are identical to their reference's. Beyond RESOLVE's own variation lean D moves two deals.
pasadena-d5 is joined: a window split (p9 and p6 of one window, in two documents), the first's record offered, marked,
and chosen. velica-d1 is split: the rollover "The Sumas piece with Vleica is being replaced by a new Cal 01 deal", with
the old deal's record offered first, which the model read as the new deal. kestrel_ridge-d2, pasadena-d2 and
pasadena-d4 change with fresh answers alone. 145 decisions are added, judged wrong 4 to 5. The noise passes' answers
are now the latest in the three stores, so a recorded replay picks its read with `latest.py`.
