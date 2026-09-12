# climb-ostrom prompts — the v0 tight-loop workdir

This repo is the solve WORKDIR for the prompt-climb v0 pilot
(research/prompt-climb/PRE-REG-obsidian-2026-09-09.md in the
monorepo). `philosophy_atlas/` holds the pipeline's prompt files —
edits here, selected via `SOVEREIGN_PROMPT_DIR=$PWD` at enrich time,
change how the climb-ostrom corpus is extracted. `check.sh` is the
checker: it re-enriches under the CURRENT (candidate) prompts, scores
the subset golden, and emits PASS/FAIL lines.

Done means: every expected person/concept atom is matched and no
forbidden atom appears. The checker's failure output names exactly
which golden entries are missing.
