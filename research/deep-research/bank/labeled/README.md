# FR-6 labeled set — schema and provenance

**Bank v0 mint, 2026-08-14, order `deep-research-t0b`.** The ~100-claim
labeled set the FR-6 decorrelation measurement (red R-7) runs both strings
against.

## Schema (`claims.jsonl`, one JSON object per line)

```json
{
  "id": "item-01",
  "question": "the question the draft answer answers",
  "answer": "a draft answer; its sentences include every claim below VERBATIM",
  "evidence": ["chunk 1", "chunk 2", "..."],
  "claims": [
    {"text": "sentence that must be judged", "label": "supported|unsupported",
     "kind": null | "claim-shaped" | "specific-shaped" | "overlap"}
  ]
}
```

- **label** — ground truth: `supported` = entailed by the evidence chunks;
  `unsupported` = not entailed (fabricated or unverifiable detail).
- **kind** (unsupported only) — the failure shape the two strings differ
  on: `claim-shaped` (a plausible broad assertion with no support in the
  evidence), `specific-shaped` (an exact figure/date/name with no support
  — the shape the specifics scan exists to catch), `overlap` (both: an
  asserted specific that is also claim-like). Distribution: 14
  claim-shaped / 13 specific-shaped / 13 overlap = 40 unsupported; 60
  supported; 100 total.
- **Verbatim rule:** every claim text is a sentence that appears
  verbatim in `answer`, so the specifics-scan's output matches claims by
  **deterministic containment** — no semantic matching anywhere in the
  measurement.

## Authoring discipline

- All items authored from the writer's own knowledge, **before** any arm
  ran; no system output was consulted (same NWCI rule as the seeds).
- Evidence chunks are synthetic passages written to contain the supported
  claims' facts and to NOT contain the unsupported claims' facts. The
  topics are deliberately **not** bank-seed topics, so the measurement
  cannot be contaminated by either the seeds or the arms' retrieval.
- The supported/unsupported split is the author's ground truth; the FR-6
  driver does not tell the strings which label a claim carries — it only
  feeds (question, answer, chunks) and compares string verdicts to the
  labels.

## Measurement protocol (the FR-6 driver, `fr6_decorrelation.rs`)

- String A: `claim_violation_joint` — per-claim judge call per claim
  (production function, judge.rs; visibility bump per directive 13efc5dc).
  Verdict: unsupported iff violation prob >= `grounding_gate_threshold()`.
- String B: `scan_unsupported_specifics` — one call per item over the
  answer; flagged specifics containment-matched to claims; a claim is
  B-unsupported iff the scan flagged a specific contained in it.
- Agreement = fraction of claims where A and B agree (both supported /
  both unsupported). Joint-miss = unsupported claims missed by BOTH
  strings. Single-string miss = unsupported claims missed by exactly one.
- Never-ran (`None` from either string) is a verdict class, never a
  default (§18.1, §18.3) — recorded, counted, reported.
- Outputs: `fr6-report.json` (raw per-claim detail, incl. each item's
  raw flagged specifics) sits beside the set; the interpretation and
  posture recommendation are `research/deep-research/notes/fr6.md`.
  The label corrections journaled there (item-10:2 astrolabe,
  item-11:1 visible-from-space, item-02:3 rule violation) were made in
  the same commit as the measurement, before the headline numbers were
  taken.
