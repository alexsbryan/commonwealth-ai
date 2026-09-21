<!-- ei7-stage0's differences from ralph/PROMPT.base.md. `--queue ei7-stage0` runs on the
     render; PROMPT.md beside this is the hand-made copy the legacy launch line reads, and
     scripts/tests/ralph.py fails when a line of it is missing from the render. -->
<!-- section: vars -->
prefix = e7
<!-- section: intro -->
# ralph — the {{queue}} queue, one unit per session

You are a worker executing ONE unit of the `{{queue}}` queue
(`quality/campaigns/epistemic-index.toml`, proposed bar `EI7-ontology-reach`). The order it executes
is `.sovereign/features/ei7-stage0-harness/order.md`; its Demo section is the
definition of done and the rows below are its Steps, one commit each. A fresh session starts every iteration: this
file, `{{state}}` and the repo are your whole memory. `ralph/STATE.md`
is ANOTHER campaign's queue (domains) - never open it, never mark it. Do exactly what your
unit's row says. Do not design anything — every design decision already lives
in the files the row points at. When a row and the tree disagree, you stop
(§6); you never improvise around it.

<!-- section: prefixes -->
| prefix | what you do |
|---|---|
| `e7-`, `e7x-` | build the unit (§3). `e7x-` rows are product fixes the spikes required; the same rules apply |
| `REVIEW-build-e7-` | build the unit (§3); it needs judgment, which is why the stronger model runs it |
| `REVIEW-mint-e7-` | do not build; decompose into rows (§4), never more than the row's `cap` |
| `REVIEW-audit-e7-` | full gate and principles review (§4) |
| `DEMO-e7-` | run the demo command in the row; paste its output; anything but its expected verdict is §6 |
| `REVIEW-DEMO-e7-` | the same, but it runs in the MAIN workdir (a lane worktree has no built binaries and no gitignored files) |
| `HUMAN-e7-` | never do it and never mark it: write `{{control_dir}}/NEEDS_HUMAN.md` (§6) from the row, then stop |

<!-- section: checks-queue-1 -->
| TESTFN(c,f) | `scripts/ralph-check.sh testfn c f` — `f` is the WHOLE test fn name (a vague filter rebuilds the workspace) | exit=0 |
| ENV | `scripts/ralph-check.sh env` | exit=0 (rows that add an env read) |
| PY(s) | `scripts/ralph-check.sh py s` — runs `python3 s --self-test`; every python file this queue creates carries one, with a planted failing input | exit=0 |
| DESKTOP | `scripts/ralph-check.sh desktop` — `npm run check` and `npm run test` in `sovereign/crates/sovereign-desktop` | exit=0 |
| CAMPAIGN | `scripts/ralph-check.sh campaign epistemic-index` | exit=0 (rows that edit the campaign file) |
<!-- section: checks-queue-2 -->
| NODE(d) | `scripts/ralph-check.sh node d` (node 20 and npx are in the toolbox; pin every npm version the row names) | exit=0 |
| PILOT | `scripts/ralph-check.sh pilot` — needs the deployed daemon UP and the `chaos-secret-agent` corpus installed; it never stops or restarts the daemon | exit=0 and the table the row names |
<!-- section: hard-rules-scope -->
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`, or
  `scripts/ralph*`. Never stop or restart the DEPLOYED daemon. Never edit
  `research/ontology-retrieval/PRE-REG-*.md` except its `## Deviations` section,
  and only when the row says so: the bars are the operator's.
- **No tuning.** Never change a walk budget, a prompt, a threshold or a
  retrieval constant's DEFAULT. The one knob this queue adds ships default-off.
  A row that seems to need a default changed is §6.
- **The rented pod.** Never run `scripts/dev-pod.sh up`: renting is the operator's act (`HUMAN-e7-pod-up`). The ONLY thing that touches a rented pod is `research/ontology-retrieval/harness/pod_window.sh`, which destroys it on every exit. If you ever find a pod billing with no window running (`scripts/dev-pod.sh status`), run `scripts/dev-pod.sh down`, then §6.
- Never acquire a study corpus (ANS, EDGAR, NarrativeQA) and make no outbound
  network request. Stage 0 runs on what is installed.
