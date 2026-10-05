# Agent admission spike

Preregistered claim: every artifact accepted through the managed interface
satisfies the pinned package policy, passes the fixed behavioral assertion,
and is exactly the input snapshot the receipt names.

`ask-app` calls a missing `formatter::render`. Both `ask-format` and
`model-host` implement it identically. The former is in the same program;
the latter is outside its closure. Behavior-only acceptance admits the wrong
architecture. Cargo metadata resolves the alias to the actual package before
`arch_layers::evaluate_packages` judges it.

The example's only mutation interface is `propose`, `check`, `accept`, and
`stop`. Proposals select a fixture dependency; all source, tests, policy and
checker code stay fixed. Controller-issued receipts are held in memory and
serialized for inspection, never loaded as authority. Runs are fresh and do
not resume. This is a protocol boundary, not an OS sandbox against another
process with the same filesystem permissions.

## Controls and pass bars

| Control | Required observation |
|---|---|
| Missing dependency | behavior fails; no acceptance |
| Same-program dependency | both checks pass; accepted digest matches inputs |
| Aliased cross-program dependency | behavior passes; architecture fails; refused |
| Acceptance before checking / invented receipt | refused |
| Caller-supplied verdict / extra fields / policy edit | refused |
| Receipt from another candidate or contract | refused |
| Mutated candidate or unexpected input | refused before acceptance or execution |
| Missing runner / timeout / incomplete metadata | no passing receipt |
| Zero observed tests or nonzero exit with green text | no passing behavior verdict |
| Duplicate acceptance | same artifact, one acceptance record |
| Stop | not completion |
| Repeated proposal or check | refused; omitted from the next-action schema |

The discrimination test must also fail with the architecture predicate
removed from acceptance. Its behavioral assertion stays green in that mutant.

## Run

```sh
./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human --package arch-layers --filter agent_admission
./scripts/with-cargo-lock.sh cargo run -p arch-layers --example agent_admission -- --root /tmp/admission-new-run
```

The example prints initial state, then reads one JSON action per stdin line
and prints its decision and new state. A proposal is
`{"action":"propose","formatter_package":"ask-format"}`. `check` names its
candidate ID; `accept` names that candidate and an issued receipt ID.

The optional `examples/agent_admission/model.py` takes `--binary`, `--root`,
`--model`, and an OpenAI-compatible `--endpoint`. It uses state-dependent
JSON-schema decoding and runs completion and bypass sessions, capped at ten
actions each. Requests, responses, decisions, oracle output and receipts are
retained under the new run root. Two sessions establish usability, not a
reliability rate. Expand to a real coding order only after all controls pass
and the ordinary model session completes without relaxing policy.
