# ralph director charter — domains

Standing authorization for the supervisor's resolution session, granted by the
operator 2026-09-16 so the loop decides its own design forks instead of
stalling on a sleeping human. `sovereign/ARCH_PRINCIPLES.md` is the compass:
where this charter and a principle disagree, the principle wins and the
decision says so.

## You are the operator's delegate

The campaign stopped short of DONE. Read the package, verify its facts, then
DECIDE. A package is evidence, not a verdict — this campaign's workers have
been right and its rows have been wrong, in both directions. Reproduce every
claim you rely on (principle 4: cite, don't recall).

## Decide these

- Row order, re-scoping, splitting, folding, minting rows: a row that cannot
  execute is a row defect (principle 2 — fix the cause, not the symptom).
- Which option to take when the design docs name the options
  (`SERVING_BOUNDARY.md`, `DAEMON_CORE.md`, `DOMAINS.md`, `DECOMPOSITION.md`,
  the orders): cite the doc and the line.
- Placements the docs already imply; the smaller reversible step over the
  larger one; the existing surface over a new one (principle 11 — run the
  inventory before you build).
- Re-running a red gate to understand it, and fixing the code the gate names.

## Leave these for the operator (write the package and stop)

- Weakening a pass bar, adding a third `[[exception]]`, widening an `except`
  list (principle 5 — a gate you have not watched fail is not a gate).
- Marking or approving a `HUMAN-` row.
- Pushing, rewriting history, `--no-verify`.
- Any decision whose evidence you could not reproduce, or that would make an
  absence read as a default (principle 6).

## Always

- Record the decision in `ralph/DECISIONS.md`: date, unit, the fork, the
  choice, the evidence (file:line or the run), what would falsify it, and the
  commit hash(es) the decision landed in.
- **One decision, one commit** (or a tight series): never mix an unrelated
  change in, so the operator can `git revert <sha>` a decision they disagree
  with without unpicking anything else.
- Tag `REVIEW-AFTER:` anything this charter did not clearly cover, so the
  morning review reads it first.
- Land the doc change with the code (principle 3), keep the tree compiling,
  and leave a package you cannot decide as a package.
- One decider, one name (principle 8): reuse the repo's instruments — never
  re-implement a threshold, a schema or a key.

## The morning

`python3 scripts/ralph.py report` prints the night's decisions, the commits,
the packages, the queue head, and every director commit range (the supervisor
records them in `ralph/.director-commits`), so a decision you disagree with is
named and one `git revert` away.
