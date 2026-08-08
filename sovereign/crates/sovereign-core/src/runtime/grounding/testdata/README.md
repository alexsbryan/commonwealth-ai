# Grounding-gate test fixtures — real, not synthesized

Every file here is lifted verbatim from a committed chaos-monkey transcript.
Nothing in this directory was written by hand. If you need to regenerate one,
name the transcript and the turn id in this file at the same time.

## `polluted_answer.md`, `polluted_scan_items.txt`, `polluted_holdings.txt`

The defect: the specifics scan's judge commentary reached the epistemic
ledger as `failed_once` holdings, and the user-visible verification note.

| | |
|---|---|
| transcript | `sovereign/bench/chaos_monkey/results/saltgrass_compound_gv_shadow_20260808.transcripts.jsonl` |
| turn id | `compound-killer-and-lugger` |
| bank | `saltgrass_compound` (dev) |
| harvest | 2026-08-08, BeefyMac, `--gv-shadow` |
| gate action | `rewrite_annotated` |
| answer sha256 | `f2019ac2a2ee369b…` (1547 bytes) |

- **`polluted_answer.md`** — the released answer with the appended verification
  note stripped, i.e. the draft body the specifics scan actually audited.
- **`polluted_scan_items.txt`** — one judge line per line, the specifics scan's
  raw output. Reconstructed by inverting the pre-fix `normalize_scan_item`
  fallback (`trim().trim_matches(['"','“','”']).trim()`) over the recorded
  holdings, then **verified**: replaying these three lines through the pre-fix
  function reproduces `polluted_holdings.txt` byte-for-byte.
- **`polluted_holdings.txt`** — the three prose rows as the ledger recorded
  them, verbatim from `epistemic_state.holdings[4..7]`. These are what the user
  saw labelled as their answer's failed claims.

Of the turn's five `failed_once` holdings, these three are judge prose; the
other two ("Corwin Pellow was murdered by Severin Quenholt." and "The murder
took place at The Cold Lantern inn on a summer evening.") are real per-claim
judgements and must survive any fix. That 3-of-5 split is the
"negative class is 60% judge-commentary artifact" finding in
`sovereign/bench/calibration/h4/FINDINGS.md`.
