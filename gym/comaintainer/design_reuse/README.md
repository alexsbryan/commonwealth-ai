# Design reuse — source evidence before extension

## Correction to the original results

The earlier 7/8 result was agreement with supplied `usable`/`serves` labels,
not independently verified technical-design quality. A consistent `USE`
header could accompany a delta explicitly building a redundant detector;
the old loop admitted it. The SEP prompt showed only the struct header,
not the `atom_counts` field previously described as visible to the model.
A was task-only, not normal repository discovery. Byte-identical repeats
establish identical received outputs, not a general zero-noise floor or
independent fresh generation. Original logs remain under `runs/`.

The label loop is retained only as an explicit `--legacy-label-ablation`.
It is not an artifact-admission path. Its fact booleans are curated,
mostly unrefereed task judgments; checking their types does not verify
their truth. None of these results establishes semantic adequacy.

## Source-selection experiment

The bank contains eleven **reconstructed** historical cases. BASE/outcome
commits and actual line windows are checked; nine labels require a referee
and two cite recorded operator corrections. These are not original-session
replays. The bank does not prove that a final commit was the only good design.

All three source-selection arms now have the same BASE-only `list`, `read`,
and literal `search` interface, the same lookup/generation budget, and the
same required final fields. A starts without a curated dossier; B receives
validated source windows; C additionally restricts evidence IDs. Source
windows include their commit, blob, coordinates, and content digest.
Source provenance and reference membership are separate from semantic support.

Each run freezes `inputs.json`, its digest, split membership, final schemas,
instrument files/hashes, and generation settings. Per-step prompts, schemas,
raw replies, and host lookup results are retained. Rescore reparses **raw**
against the frozen bank, ignores cached `parsed`, makes no model calls, and
refuses legacy inputs or changed instruments instead of silently substituting
the live bank or scorer. A scorer correction requires a named new version.
Transport model labels are recorded but are not independent served-engine
attestation. No fresh-source-search improvement rate has been measured yet.

Metrics are lexical selection/label agreement and protocol completion.
`home_match` does not prove that the selected home can serve the requirement;
`evidence_grounded` means ID membership, not entailment. Human refereeing or
an appropriate independent probe must establish task coverage and minimality.

## Core-read artifact episode

The concrete admission path extends the existing Rust example rather than
the label scorer:

```
lookup BASE interface
    -> probe_existing (fixed consumer, real compiler)
    -> propose_extension (typed operations, managed source artifact)
    -> check (compiler + observed Cargo edges + pinned package policy)
    -> accept (controller-issued receipt bound to exact artifact/contract)
```

The baseline is the actual historical `IndexSource` blob. The small compiler
projection preserves its two methods and requires six additional typed reads.
Missing-method diagnostics must be observed before the extension operation
is offered. The candidate can choose contract/engine placement, port/engine
binding, and registry-selected methods; prose labels, arbitrary source,
new services, and self-authored verdicts are not operative actions.

The fixed consumer compiles every required call. Engine placement can pass
the compiler while failing the dependency gate. An incomplete method set or
wrong return type fails the compiler. Acceptance requires both checks and a
fresh artifact digest; forged, mismatched, or stale receipts cannot authorize it.

**Scope:** source-bound projected compiler surface and package closure.
Nominal substitutions for heavy index/error/lease types are listed in
`quality/arch-layers/tests/fixtures/core-read/source-contract.toml`. This is
not a historical engine build, proof of caching/embedding/lease behavior,
whole-core migration, general architectural taste, or an OS sandbox against
another same-UID process. The consumer, policy and projection are host-owned.

### Controls

- No extension before complete source observation and a definite baseline gap.
- Complete contract-owned surface with port binding is accepted.
- Behaviorally green engine-owned surface with engine binding is refused.
- Missing read and wrong return type fail the compiler.
- Fake disposition/delta/new-service fields and duplicate method IDs are refused.
- Mutation after checking invalidates the receipt.
- Removing the architecture requirement from acceptance makes the engine-coupling
  test fail (`accepted` versus required `refused`); the guard was restored.

## Run

```sh
python3 gym/comaintainer/design_reuse/validate.py
python3 gym/comaintainer/design_reuse/test_design_reuse.py
python3 gym/comaintainer/design_reuse/replay.py --pin <advertised-model-id> --limit 3
python3 gym/comaintainer/design_reuse/replay.py --rescore <new-run-directory>
./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human --package arch-layers
./scripts/with-cargo-lock.sh cargo run -p arch-layers --example agent_admission -- --root /tmp/new-core-read-run --episode core-read
```

The example prints state and legal schema, then reads JSON actions on stdin.
Start with `{"action":"lookup","path":"corpus-index/src/source.rs","start":1,"end":20}`
and `{"action":"probe_existing"}`. Propose an extension with `target`, `binding`,
and a selected `methods` list from the offered schema; `check` names its candidate
digest and `accept` names that digest and the issued passing receipt.

Holdout requires explicit `--include-holdout`; archived split membership is
frozen per run. Referee the semantic labels and retain independent known-bad
and valid-new-capability controls before making a design-quality claim.
