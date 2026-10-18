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
