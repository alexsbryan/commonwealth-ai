# Archived docs

Frozen status writeups — closed experiments, gap audits, completed
calibration plans. Kept in-tree as forensic trail; the durable
lessons live in the NoteStore (`sovereign notes --query <topic>`),
and each writeup below has a companion `decision` note pointing
here.

If you want the live state of a feature these touched, check
[`../README.md`](../README.md) or `git log` on the relevant code.

## Lifecycle

When a writeup lands here:

1. Per-lesson notes go to NoteStore (kinds: `decision` / `attempt`
   / `invariant` / `todo` / `commitment` / `follow_up` / `goal`).
   Each is independently queryable.
2. One pointer note (`decision` kind) names this archived path
   and lists the companion notes.
3. The markdown moves here verbatim — no edits. It's the forensic
   record.

Querying past experiments: `sovereign notes --query <topic>`.

## Contents

- [`PHASE_7_GAP_CLOSURE_PLAN.md`](PHASE_7_GAP_CLOSURE_PLAN.md) —
  2026-04-29 audit closing five gaps (A–E) the two demo docs
  flagged as "wired but partial." Closed.
- [`ATOS_SELF_HOST_EXPERIMENT.md`](ATOS_SELF_HOST_EXPERIMENT.md) —
  overnight test-and-iterate plan using ATOS to drive opencode +
  K2.6 against `oicp-types`. Calibration past.
- [`RERANK_EXPERIMENT.md`](RERANK_EXPERIMENT.md) — cross-encoder
  reranker experiment (2026-05-11 → 2026-05-12). SEP +28% rel
  source recall; dedup default-on for SEP; slot stays
  experimental. Per-corpus dedup filter shipped.
- [`SD_EXPERIMENT.md`](SD_EXPERIMENT.md) — speculative-decoding
  experiment (closed 2026-05-12). Net-negative on A3B-class MoE
  targets; bench harness preserved for future MTP measurement.
- [`URL_CONSTRAINT_INTEGRATION.md`](URL_CONSTRAINT_INTEGRATION.md)
  — URL-allowlist constraint integration (shipped 2026-05-19).
  Six-step plan executed; EOS-bypass fix + fast-slot alias fix
  captured during validation.
- [`LLGUIDANCE_MIGRATION_AUDIT.md`](LLGUIDANCE_MIGRATION_AUDIT.md)
  — full feature-surface comparison + 11-site schema inventory +
  risk register that drove the D-full migration (shipped
  2026-05-22). `json_constraint.rs` deleted; single grammar
  engine.
