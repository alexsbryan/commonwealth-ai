# Plan Alignment — case study and rationale

> Every plan written into `~/.claude/plans/` answers four
> alignment questions before any code is generated. A
> `PreToolUse` hook on `ExitPlanMode` enforces it. Here's why,
> what it catches, and a worked example from this codebase.

## The four questions

A complete plan answers:

1. **What this extends.** What existing files / functions /
   patterns is this building on? Forces visibility-of-existing
   so we don't accidentally reinvent.
2. **What this removes.** What's being deleted alongside the
   addition? Forces engagement with deletion — the question
   nobody asks by default.
3. **Restraint patterns.** Which `ARCH_PRINCIPLES.md` sections
   and archaeology-eval inquiries govern the touched files?
   Forces engagement with codified norms, not just intuition.
4. **Could this be done with less?** What's the smallest viable
   shape? When the answer is "no, this is the minimum," answer
   honestly with reasoning.

These four (plus a `## Context` framing) are the **required**
H2 headings every plan file must contain.

## The tooling, end-to-end

| Component | Role |
|---|---|
| `~/.claude/plans/_TEMPLATE.md` | Starter scaffold with the five required sections. Copy when seeding a fresh plan. |
| `feedback_plan_alignment_sections.md` (memory) | Cultural reinforcement — fires every session-start so the model knows the shape. |
| `sovereign plan validate <path>` | CLI linter. Exit 0 = all sections present, 1 = missing list to stderr. |
| `.claude/hooks/validate-plan.sh` | `PreToolUse` hook keyed on `ExitPlanMode`. Picks the most-recent plan under `~/.claude/plans/` (excluding the template), runs the validator, exit 2 to hard-block the tool call when sections are missing. |
| `.claude/settings.json` `hooks.PreToolUse[matcher: ExitPlanMode]` | The wiring that mounts the hook. |

Together they make the rule **structurally enforced** at plan
exit time, not just culturally encouraged.

## Worked example: today's plan

The Claude Code plugin v0 plan was authored this morning before
this rule existed. The original is preserved as a write-up
here; the rewritten (aligned) version is at
[`docs/examples/plan_v0_brief_aligned.md`](./examples/plan_v0_brief_aligned.md).

### What the original plan said

Original plan headings, in order:

```
## Context
## Scope
   ### Ships in v0
   ### Deferred to v1+
## Architecture
   ### Module layout
   ### Reuse map (do not reinvent)
   ### Critical paths in existing code
## Verification
## Open implementation questions
## Out of scope — flag explicitly
```

Total: 252 lines. **0 of 4 alignment questions explicitly
labelled.** The substance was scattered:

| Question | Where the answer lived in the original | Quality |
|---|---|---|
| What this extends | "Reuse map" table (11 entries) + "Critical paths" | Strong — but 2 of 11 entries were for v1 work that wouldn't pay back in v0 |
| What this removes | One sentence in "User decisions on scope" + one bullet under "Replacement hook" | Weak — never confronted as a theme |
| Restraint patterns | Implicit in "Reuse map" ("don't reinvent") | Weak — no `ARCH_PRINCIPLES` section cited by number |
| Could this be done with less? | "Deferred to v1+" + "Out of scope" lists | Partial — caught most cuts, missed the daemon HTTP endpoint |

### What the rule would have caught

The original plan specified a daemon HTTP endpoint
(`POST /v1/brief/working_set`) across three sections of the doc
(~30 plan lines + an estimated ~150 lines of Rust + a new
`install_*_router` daemon plumbing path).

**During implementation we cut it as deferred-to-v0.5** — the
CLI binary was self-contained and the hook could shell out
directly. The cut was real. It happened mid-implementation,
when scope was already in flight.

If the four-question rule had been live at plan time, **q4
("could this be done with less?") would have generated the cut
upfront**, saving:

- ~30 lines of plan content,
- the cognitive cost of a mid-implementation U-turn,
- the daemon-side plumbing that didn't ship.

The rewritten plan names this explicitly under "Could this be
done with less?":

> **Drop the daemon HTTP endpoint.** The CLI binary
> (`sovereign code brief`) is self-contained: it opens the
> NoteStore, walks git, reads the atlas. The hook can shell
> out directly. Saves: 1 new module (~150 lines), 1 daemon
> `install_*_router` method, NoteStore wiring through the
> daemon's state. Re-add in v0.5 if a real cross-process use
> case emerges.

That's the value claim of the four-question discipline in one
concrete bullet.

### Before/after, by the numbers

| Metric | Original | Aligned rewrite |
|---|---|---|
| Total lines | 252 | 197 (-22%) |
| Required H2 sections | 1 of 5 | 5 of 5 |
| Implicit content under right heading | scattered | grouped |
| Daemon HTTP endpoint scope | included | cut at q4 |
| `ARCH_PRINCIPLES` sections cited | 0 | 5 (§3.1, §3.2, §5.3, §10.2, §14.2) |
| Validator exit code | 1 (4 missing sections) | 0 |

**Smaller AND more structured.** That's the rule earning its
keep.

## Excerpts from the aligned rewrite

Three sections worth lifting because they show what the rule
produces that the original lacked:

### What this removes

```markdown
- Unwiring `inject-notes.sh` from `.claude/settings.json`'s
  UserPromptSubmit block. The script file itself is kept as
  historical reference and as a fallback during the brief's
  bake-in period; it's the wiring that goes.

Explicitly NOT removed:
- The read_notes MCP tool — still used by other surfaces.
- KnowledgeViewManager and its digest renderer — the brief
  forks the pattern, not the code.
```

The original mentioned the unwiring once in passing. The
rewrite *also* lists what's NOT being removed — preventing
overzealous deletion, the inverse failure mode.

### Restraint patterns

```markdown
- §3.1 File-size ceilings — both new files target <500 lines.
- §3.2 Single concern per file — working_set is only detection;
  brief is only rendering. Token estimation stays in
  knowledge_view::tokens (don't fork it; reuse).
- §5.3 Don't widen single-method traits — we don't introduce
  any new trait. assemble_brief is a free function.
- §10.2 Touch one dimension at a time — this PR adds the brief
  assembler. No collateral refactors.
- §14.2 Notes at the moment of decision — the brief surfaces
  decision/invariant notes; the reflection capture writes
  them. The principle is being operationalized, not just
  honored.
- archaeology-eval baseline — currently 8/8 inquiries passing.
  The PR must not regress this. (Pre-push ratchet enforces.)
```

The original had zero principle citations. The rewrite cites
five sections by number. That's not pedantic decoration —
it's the cross-reference that lets a future reviewer audit
"did this PR honor §3.2?" mechanically.

### Could this be done with less?

```markdown
Yes — three minimizations identified at plan time:

1. Drop the daemon HTTP endpoint. [...]
2. Defer the inquiry coverage. [...]
3. Skip "Explicit gaps" section in v0 brief. [...]

Minimum viable v0: working_set module + brief module + CLI +
UserPromptSubmit hook + Stop hook + snapshot fixture.
Estimate after minimization: ~1.5 weeks (was ~2 weeks).
Implementation result: shipped in one session.
```

Three cuts named upfront. Two of them (1 and 3) we DID make
during implementation — the rule would have surfaced them
earlier.

## When the rule pays back, when it doesn't

**Pays back:**

- Plans that grow scope as you write them. The four questions
  bite the writer at exactly the moment of accidental sprawl.
- Plans where deletion is non-obvious. Q2 forces it.
- Plans that touch principle-governed files. Q3 makes the
  implicit explicit.
- Sessions where the model would otherwise generate against
  the path of least resistance.

**Doesn't pay back:**

- Trivial work (typo fix, single-line rename, <50-line edit).
  Plan mode usually isn't entered for these; the rule self-
  skips.
- Pure-research plans where there's no diff to align against.
  These should be marked clearly so the validator's heuristic
  doesn't fire.

## Tooling reference

```bash
# Validate a plan manually
sovereign plan validate ~/.claude/plans/<your-plan>.md

# Bypass the hook (use sparingly; pair with a memory note)
SOVEREIGN_NO_PLAN_VALIDATE=1 # in env; OR
git push --no-verify          # for the related ratchet

# Read the aligned-rewrite case study
cat docs/examples/plan_v0_brief_aligned.md
```

## Mesh-replicated alignment workspace

Plans (`~/.claude/plans/`), auto-memory entries (`~/.claude/projects/-*/memory/`), and the ATOS NoteStore (`~/.svrnmesh/notes.db`) ride a built-in `alignment` corpus that mesh-replicates between the user's own daemons. Newest mtime wins per logical key, so two machines that edit the same plan converge on the newer copy after a mesh tick. The post-merge projector materializes received chunks back to disk on the receiving daemon — fresh machines reach parity in one ingest.

### Operator flow (run on both machines)

```bash
# Optional: preview what's in scope (no writes, no daemon traffic).
sovereign alignment migrate --dry-run

# Real run: tar a backup to ~/.svrnmesh/backups/, then submit a
# corpus install for the alignment recipe. The daemon completes the
# ingest and any peer pulls in the background.
sovereign alignment migrate

# At any time:
sovereign alignment status
```

Order doesn't matter — each machine's `alignment migrate` lands its current state on the corpus, and the existing daemon hooks (`auto_recover` after a stranded-partition merge, `index_transfer` after a peer pull) fire the projector automatically once the OTHER side has caught up.

### Recovery

The backup tar at `~/.svrnmesh/backups/alignment-pre-migrate-<ts>.tar` restores the original state with `tar -xf <path> -C $HOME` (the archive uses `~/`-relative paths). The migration is idempotent: re-running converges, doesn't compound.

## Replicating drift atlases between mesh peers

`drift_cmd_orchestrator.rs::ensure_recipe` now stamps narrative recipes with `mesh_sharing = true`. The auth boundary is Tailscale-IP — a corpus only reaches peers already in the mesh — so the flip exposes drift output to the user's own machines and nobody else.

**Future drift runs** ride the new flag automatically. No action required.

**Existing on-disk corpora** — recipes already stamped before the flip, plus `*-self-atlas` and source-code corpora produced before mesh sharing was wired — need a one-time `_corpus_meta.json` edit. Pick one of:

```bash
# Selective flip — review each corpus, skip license-restricted ones (e.g. SEP).
for f in ~/.svrnmesh/indexes/*/_corpus_meta.json; do
  echo "--- $f ---"
  jq '{id, license, mesh_sharing}' "$f"
done

# Bulk flip — only after confirming none of your installed corpora are
# under restrictive licenses (SEP, anything copyrighted). Skips already-true.
for f in ~/.svrnmesh/indexes/*/_corpus_meta.json; do
  jq '.mesh_sharing = true' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
done
```

Once flipped, `sovereign mesh status` on a peer should list the corpus in the gossiped catalog within a tick or two. `sovereign corpus install <id>` on the peer pulls the partition tar — atlas sidecars (`atoms.json`, `edges.json`, `git_archaeology.json`) are inside the tar, so the receiving daemon gets the full drift output without re-running the LLM.

## See also

- [`feedback_plan_alignment_sections.md`](../../.claude/projects/-Users-user-dev-commonwealth-ai/memory/feedback_plan_alignment_sections.md)
  (lives under `~/.claude/projects/.../memory/`) — the memory rule.
- [`docs/ARCHAEOLOGY_EVAL.md`](./ARCHAEOLOGY_EVAL.md) — pre-push
  ratchet that catches inquiry regressions at git push time. The
  plan-alignment rule is its plan-time analogue.
- [`docs/GIT_ARCHAEOLOGY.md`](./GIT_ARCHAEOLOGY.md) — atlas
  provenance the brief reads, and the inquiries can witness.
- [`crates/sovereign-cli/src/plan_cmd.rs`](../crates/sovereign-cli/src/plan_cmd.rs)
  — the validator. `cargo test plan_cmd` for the unit tests.
- [`.claude/hooks/validate-plan.sh`](../.claude/hooks/validate-plan.sh)
  — the PreToolUse hook.
- [`.claude/plans/_TEMPLATE.md`](../../.claude/plans/_TEMPLATE.md)
  (lives under `~/.claude/plans/`) — starter scaffold.
- [`docs/examples/plan_v0_brief_aligned.md`](./examples/plan_v0_brief_aligned.md)
  — the worked example, mirror of the
  `~/.claude/plans/` plan.
