# M2 — Stream B corruption harness: built, validated, first batch (2026-07-31)

Implements STREAM_B_DESIGN.md's build order against VERIFIER_V0.md §3. The
corruption core lives in `sovereign-eval` proper (Alex's call, resolving the
design doc's open question): it inherits the flywheel fairness contract,
seeded reproducibility, and the registry's empty I2 slot, and the taxonomy is
written once.

## What shipped

**The production seam.** `extract_claim_list` is now `pub`
(`sovereign-core/src/runtime/grounding/mod.rs` wrapper over the gate's own
`judge::extract_claim_list`, re-exported at `sovereign_core::runtime`), so
offline claims are in the exact register the verifier sees at the gate.
`value_present_in_chunks` is re-exported alongside it for export-time
re-validation.

**The generator.** `sovereign-eval/src/flywheel/generators/adversarial.rs` —
registered as `i2_adversarial` (the slot `ProbeSource::I2Adversarial` named
since day one). Consumes a self-contained harvest artifact (`HarvestFile`,
claims + sealed evidence windows inline + optional entity-cluster and
distractor-doc side tables), stays pure (serde + std + rand), and is
`(n, seed)`-reproducible bit-for-bit. Ten kinds:

| Ungrounded (7) | Grounded (3) |
|---|---|
| entity_swap, number_perturb, negation_flip, cross_chunk_chimera, ocr_garble, distractor_absorption, unsupported_addition | verbatim, reframe, multi_hop_conjunction |

Labels are by construction: every case carries a typed `SiteWitness` and
**span offsets** (byte ranges into the claim — spec §10 lever 2, in the
schema from day one), and `validate_site` mechanically re-checks the
corruption at its site. Value-presence checks run against
`det_checks::value_present`, a pinned port of the production checker (parity
tests in det_checks.rs; the source-of-truth comment names the original).
Lowering to eval probes: corrupted → `AbsentAdjacent` (must not confirm),
grounded → `Present` with witness — `validate_fairness` and the chaos scorer
apply unchanged.

**The CLI verbs** (`svrn bench verifier …`, bench_cmd/verifier.rs):
- `extract-claims` — single-pair stdin/stdout seam (score-answer shape).
- `harvest --corpus <id>` — chunks → consecutive-chunk evidence windows
  (`--window`, default 2) → production claim extraction → `claims.json`.
- `export --harvest <path> --n N --seed S` — generate cases, then
  **re-validate every case with the production `value_present_in_chunks`**
  (the port generates, the genuine article gets the final word), then write
  Stream B JSONL. Any production-check failure aborts the whole export.

**Teacher labeling.** `scripts/teacher_label.py` — imports the HalluGuard
interface (prompt builder, `</think>`-aware verdict parser, chat helper) from
`eval_grounding.py` so the register exists once. chosen = teacher (default
`primary`, the 35B tier), kept only on binary verdict == constructed label;
mismatch/parse-fail → `<out>.discards.jsonl` with the teacher's class
(inspectable hard-negative signal), never a relabel. rejected = weak model,
unfiltered. Resumable by case id; run manifest beside the output.

## Measured

- **Saltgrass first batch** (this box, seed 17): 15 windows → **107 claims**
  (0 failed windows) → **313 validated cases** (157 ungrounded / 156
  grounded; by kind: unsupported_addition 59, reframe 55, verbatim 55,
  ocr_garble 54, multi_hop 46, negation_flip 17, chimera 16, number_perturb
  11). The generator's dedup space exhausts at ~3 cases/claim on a corpus
  this small — volume comes from more substrate, not a bigger `--n`.
  Artifacts: `data/stream_b/chaos-saltgrass/{claims.json,stream_b.jsonl}`
  (data/ is gitignored; regenerate with the commands above — deterministic).
- **Teacher-label smoke** (4 cases, teacher=primary 35B, rejected=fast): 3
  kept, 1 discarded (teacher disagreed with an ocr_garble construction).
  The discard path working on the very first smoke is the discipline doing
  its job.
- **Tests**: sovereign-eval 200/200 green, including bit-for-bit determinism,
  full 10-kind taxonomy coverage on the fixture, site-contract rejection of a
  grounded "injection", and value_present parity pins.

## Incident worth knowing

Mid-harvest, the daemon's Metal backend wedged: `kIOGPUCommandBufferCallback`
**ErrorOutOfMemory** left ggml "in error state from a previous command buffer
failure" and every subsequent decode failed (surface symptom: harvest windows
alternating between empty claim lists and hard failures — daemon RSS was only
7.2 GB, so this was a transient unified-memory spike, likely co-resident
cargo builds). Repair: daemon restart recreates the backend. If harvest
suddenly reports consecutive failed windows, check `~/.sovereign/logs/
daemon.err` for `ggml_metal` before suspecting the harness.

## Volume run (task 5 — belongs on the Halo box)

Per VERIFIER_V0.md §4, Stream B generation is the Strix Halo's job (35B
resident; generate first, then train). Recipe:

1. `svrn bench verifier harvest --corpus chaos-secret-agent --out …/claims.json`
   (~250 chunks → ~125 windows). Saltgrass likewise (already proven above).
2. Entity swaps need the side table: convert
   `out/chaos-secret-agent.named-clusters.json` → `EntityCluster[]`
   (`{etype, surfaces}`) and pass `--entities`. Distractor absorption needs
   `--distractors` (`DistractorDoc[]`): the meridian postmortem
   (`sovereign/bench/attached_doc/meridian_postmortem.toml`) is the intended
   adjacent-doc source. Without the tables those two kinds are silently
   skipped (by design — the smoke run shows the other eight carry on).
3. `svrn bench verifier export --n <big> --seed 17` per corpus; then
   `teacher_label.py --cases … --teacher-model primary --rejected-model
   <0.8B stem>` (rejected side MUST be the 0.8B for real pairs — the smoke
   used `fast` only because it was resident).
4. Contamination pass over the generated stream: the 13-gram machinery in
   `scripts/contamination_pass.py` against LLM-AggreFact + FaithBench test
   sets; collision count goes on the eval card.
5. 20–40k pairs, ~50/50 (the exporter alternates labels; the balance held
   157/156 on the first batch).

Claim-yield arithmetic from the first batch: ~7 claims/window ⇒ secret agent
alone ≈ 875 claims ≈ 2.6k cases. 20–40k pairs therefore needs the taxonomy
over MORE substrate and/or additional public bank corpora — plan the Halo
run against that arithmetic, not against `--n`.

> **CORRECTION (2026-07-31, volume run).** The parenthetical that stood here
> — "entity/distractor tables raise cases/claim toward 10" — is **wrong**.
> The side tables feed only `entity_swap` and `distractor_absorption`, both
> ungrounded, and `generate_cases` alternates labels (`adversarial.rs:455`)
> so the ungrounded side is capped by the grounded side: **total ≈ 2 ×
> grounded**, measured exactly on both chaos corpora. Side tables raise
> taxonomy COVERAGE, never volume. The lever is substrate that clears the
> grounded gate. See `M2_STREAM_B_VOLUME.md` §2 and note `1eb7ec59`.
>
> Two sourcing claims in step 2 above are also wrong and were corrected in
> the volume run: entities come from the corpus **atlas** (`atlas/atoms.json`
> Entity atoms), not `named-clusters.json` (thematic, no surface forms); and
> distractors should come from a **same-genre** document, not the meridian
> postmortem — cross-genre distractors are easy negatives that teach
> vocabulary rather than support. `M2_STREAM_B_VOLUME.md` §4–§5.
