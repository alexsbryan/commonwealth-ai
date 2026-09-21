<!-- ledger -->

**ledger-1 · 2026-09-21 · ralph/DECISIONS.md merge shape · seat (operator: "let's do something more robust")** — this commit
- Needed: Merging `origin` into `main` conflicted on four paths, and three of the four were one cause: two campaigns appending to the end of one file. `ralph/DECISIONS.md` collided on 2,262 lines and on the id space itself — ring-room/threat-gaps had minted A35-A68 while e7/ontology minted A35-A41 from the same global counter, so seven published entries had to be renumbered to A69-A75 by hand. This is the fourth time the file has conflicted this way (`701b67453`, `8fe3b48b4`, and the two appendix entries that record resolving those).
- Chose: One decision, one file, under `ralph/decisions/`, rendered into `ralph/DECISIONS.md` by `scripts/ralph-decisions.py`; new ids are `<campaign>-<n>`, and the A1-A75 series is frozen verbatim in the archive parts rather than reflowed. `ralph/REVIEW_FINDINGS.md` gets `merge=union` instead — it has no ids to corrupt. Both declared in `.gitattributes`, with the keep-ours driver registered by `scripts/install-git-hooks.sh` and a hard `ralph-decisions` row in `quality/instruments.toml` so a stale render cannot be pushed.
- Because: Two campaigns adding two files is not a conflict at all, and a campaign slug cannot collide across branches the way a global counter does (ARCH 10 — structural, not remembered). Renumbering the legacy series to fit a new scheme would have broken citations in `quality/campaigns/*.toml`, `research/`, and commit bodies for no gain, so the archive is frozen where it stands, the way published commit trailers are.

<!-- appendix -->

## ledger-1 · 2026-09-21 — one decision one file, and ids scoped to the campaign that mints them

<details><summary>the four conflicts, why three had one cause, and what each half is for</summary>

**What the merge actually contained.** `quality/conformance/sovereign-daemon.toml`
(one `line = ` field), `scripts/ralph-check.sh` (one usage string),
`ralph/REVIEW_FINDINGS.md` (620 lines against 88) and `ralph/DECISIONS.md`
(2,090 against 172). Neither side disagreed about anything in the last three.
Both were appending.

**The conformance toml was not a merge question.** Its `line` field is generated
from the source tags by `UPDATE_CONFORMANCE_TAGS=1 cargo test -p xtask --test
conformance_tags`. HEAD said 5402 and origin said 5382; the regenerator says
5420, so both sides were stale and the conflict had no correct answer in it.
Origin's own REVIEW_FINDINGS entry records the cause — "None of the eight rows
regenerated `quality/conformance/`, and none of their `check:` lists named a
gate that would have caught it."

**The id collision is the part that cost a session.** The A-series is one
counter in one file, so two branches minting at the same time produce two A35s
and git cannot tell that they are different decisions. Renumbering is not free:
`quality/campaigns/ring-room.toml` cites A36, `quality/campaigns/mesh-principal.toml`
cites A35, `research/ontology-retrieval/` cites A39 and A41, and the ledger
rows cross-reference each other. Seven entries moved to A69-A75 and their
cross-references moved with them (inside the entries, plus
`research/ontology-retrieval/PRE-REG-custom-ontology-and-raptor-2026-09-17.md`,
`research/ontology-retrieval/pilot/bank.src.toml`, and
`ralph/next/ei7-stage0/STATE.md`). Ours kept its numbers because it carries 34
entries with ledger rows against origin's 7 appendix-only ones.

**Why the archive is frozen rather than split.** The first attempt split all 77
entries out of the monolith. It mis-grouped: the appendix contains `##`
sub-headings inside `<details>` blocks (`## (d) Then`), and a whole era of
entries keyed by `## <date> · <row> · <title>` with no id at all. A faithful
split needs depth-tracking and invented ids for the date-keyed era — inventing
identity for history, which is what ARCH 8 says not to do. The archive is
instead cut at its three existing section anchors (`## Ledger`, `## Flags for
the operator`, `## Appendices`) into four verbatim parts, and the render is
byte-identical to the file it replaced apart from one doubled blank line and
the generated banner.

**What each mechanism is for, because they are deliberately different.**
`merge=union` on REVIEW_FINDINGS takes both sides' lines with no markers, which
is correct for append-only prose and wrong for a numbered ledger — union's
failure mode is a duplicated line, visible on the page, but a duplicated id is
not visible at all. DECISIONS gets a keep-ours driver plus a hard freshness
gate instead: the driver keeps the merge moving, and the gate is what makes
that safe, because after a keep-ours merge the render is missing the other
side's entries and `--check` refuses the push until `--write` rebuilds it from
the entry files, which git merged cleanly on their own.

**Watched failing, both inputs** (ARCH 5): a hand edit appended to the render
reads STALE with the two differing lines printed; a freshly minted entry that
has not been rendered reads STALE with its ledger row and appendix shown. The
validator's own refusals — uppercase id, legacy `A<n>` id, filename/id
mismatch, missing date, missing ledger block, appendix id mismatch — are the
`--self-test`, which `scripts/ralph-check.sh py` runs.

</details>
