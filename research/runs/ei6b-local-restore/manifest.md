# ei6b-local-restore — 4b's done-when, with zero egress

Staged 2026-09-08 by ei-6b, per the seat's ruling on 4b. **Pre-registered
below, before the run.**

## What this proves, and why it is not belt-and-braces

ei-6b's first read of 4b was that the restore path could not be proven locally,
because `CorpusEngine::try_restore_prebuilt` welds the probe to a
`huggingface.co` URL (`ingest_prebuilt.rs`, banked as
`snapshot-probe-unreachable-without-egress`). The seat read the same finding
from the user's chair and saw the bigger fault: **`svrn corpus snapshot restore
--archive <path>` installed whatever it was handed, with no compatibility
decision at all.** A mean-pooled local archive landed silently — the `sep`
failure shape, on the one path with no deadline to catch it.

Two restore paths, one decider missing (ARCH §10.6). Commit b8541d34c lifted
the decision into `snapshot::judge_restored_snapshot`, called by both. This run
drives the LOCAL path through all three of its verdicts on real bytes.

## Legs

| leg | what it does | expected verdict |
|---|---|---|
| **A** | `snapshot publish wessex-hoard --output <test-artifacts>` | manifest carries `embed_quirks` — the publisher wiring |
| **B** | restore under a DIFFERENT label for the same model (`qwen-embedding-0.6b`) | `NameMismatch` → **the probe runs** → Accepted, cosine printed |
| **C** | restore under the SAME label the manifest names | `Exact` → accepted, no probe needed |
| **D** | restore an archive whose manifest's pooling is FLIPPED to `mean` | `ConfigMismatch` → **REFUSED by name, nothing installed** |

Leg B is the arm that did not exist on this path before b8541d34c. Leg D is the
negative control: without an archive that CAN fail, `ConfigMismatch` is a gate
with no input that can make it fire (ARCH §18.1), and `flip_pooling.py` changes
the declared pooling and nothing else — same chunks, same vectors, same ids —
so a refusal can only have come from the config.

## Pre-registered pass condition

`VERDICT-LOCAL-RESTORE-JUDGED` requires ALL of:

- `A-manifest-declares-config` — a published manifest carries `embed_quirks`.
  If it does not, the publisher wiring did not land and legs C/D mean nothing,
  so the run refuses rather than reporting the rest.
- `B-probe-ran` — the log contains a probe cosine. An accept with no probe on a
  name mismatch would mean the decider was skipped.
- `B-index-present`, `C-index-present` — an accepted restore actually installs.
- `D-refused` (non-zero exit), `D-names-pooling` (the refusal says *pooling*,
  not a bare number), and `D-nothing-installed` — a refused restore must leave
  nothing behind. An archive that is refused but still on disk is the worst of
  both.

Anything short of all ten is `VERDICT-LOCAL-RESTORE-INCOMPLETE`, reported as
such rather than as a pass.

## Controls

`wessex-hoard` is READ — published FROM, never written to. Every restore lands
in a cold root under `test-artifacts/ei6b-local-restore/`, never `~/.svrnmesh`.
The archive is written under test-artifacts too (`--output`), not to the
default `~/.svrnmesh/snapshots/`. The binaries built and invoked are **this
worktree's** `target/debug/`, by absolute path, so the operator's installed
`sovereign-cli` is untouched — and the build carries `--features dev-tools`,
without which the dispatcher silently degrades to an end-user binary.

## Cost

One build leg (`sovereign-cli` + `sovereign-cli-llm`, cold-ish in this
worktree — the reason this is a unit and not a Bash call), then a publish of
`wessex-hoard` (small; ~a few hundred MB with its atlas) and four restores.
`SKIP_BUILD=1` if the binaries are already fresh. **No egress.** Needs the
daemon's embed slot for leg B's probe, and preflight refuses if it does not
answer 1024-d before the build starts.
