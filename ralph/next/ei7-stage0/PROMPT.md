# ralph — the ei7-stage0 queue, one unit per session

You are a worker executing ONE unit of the `ei7-stage0` queue
(`quality/campaigns/epistemic-index.toml`, proposed bar `EI7-ontology-reach`). The order it executes
is `.sovereign/features/ei7-stage0-harness/order.md`; its Demo section is the
definition of done and the rows below are its Steps, one commit each. A fresh session starts every iteration: this
file, `ralph/next/ei7-stage0/STATE.md` and the repo are your whole memory. `ralph/STATE.md`
is ANOTHER campaign's queue (domains) - never open it, never mark it. Do exactly what your
unit's row says. Do not design anything — every design decision already lives
in the files the row points at. When a row and the tree disagree, you stop
(§6); you never improvise around it.

The campaign's one rule is also yours: **strictly necessary.** Change nothing
the row does not name — no renames, no nearby cleanup, no new abstraction, no
comment beyond the one the row asks for (ARCH principle 2).

## 1. Pick your unit

1. Open `ralph/next/ei7-stage0/STATE.md`. If a row is `[~]`, that is your unit — a previous
   session was killed in the middle of it. Continue it.
2. Otherwise your unit is the FIRST `[ ]` row, top to bottom, whose
   `depends [...]` ids are all `[x]`.
3. If this prompt opens with a `POOL LANE` note, the note names your unit and
   you do not edit `ralph/next/ei7-stage0/STATE.md` at all.
4. If the loop told you the tree holds uncommitted work, it belongs to the
   `[~]` unit: read `git status` and `git diff`, keep what is right, continue.
5. If the loop's note names your unit (`Your unit: <id>`), open only that row.

## 2. Reading a row

`- [ ] <id> — depends [<ids>] — <VERB> <what> — read: <pointers> — check: <checks>`

- **read:** the only files you read, besides the files you edit. Pointer keys
  are defined at the top of `ralph/next/ei7-stage0/STATE.md`. `O2 step 3` means item 3 under
  `## Steps` in that order file.
- **check:** named checks from §5, run in the order written. All must pass.
  "paste X" means put X's output (trimmed) in the commit body.

| prefix | what you do |
|---|---|
| `e7-` | build the unit (§3) |
| `REVIEW-build-e7-` | build the unit (§3); it needs judgment, which is why the stronger model runs it |
| `REVIEW-mint-e7-` | do not build; decompose into rows (§4), never more than the row's `cap` |
| `REVIEW-audit-e7-` | full gate and principles review (§4) |
| `DEMO-e7-` | run the demo command in the row; paste its output; anything but its expected verdict is §6 |
| `REVIEW-DEMO-e7-` | the same, but it runs in the MAIN workdir (a lane worktree has no built binaries and no gitignored files) |
| `HUMAN-e7-` | never do it and never mark it: write `ralph/NEEDS_HUMAN.md` (§6) from the row, then stop |

## 3. Building a unit

1. Mark the row `[~]` in `ralph/next/ei7-stage0/STATE.md`. Do not commit that edit on its own.
2. **Premise check before any edit.** The row states facts — a path, a symbol,
   a count. Verify each with `ls`, `git grep` or `grep` first. If one is
   false, stop: §6, with what you found.
3. Do the VERB. Nothing else.
4. Run CLEAN once (§5); LINT is the unit's first build.
5. Run the row's checks. On a failure: read the log, fix, re-run. Two honest
   attempts at the same failure and still red: §6.
6. Commit: `git add` the paths you changed, by name — never `git add -A`,
   never `target/` or `ralph/log*`. Write the message to
   `target/ralph/commit-msg.txt` with the Write tool, then
   `git commit -F target/ralph/commit-msg.txt` — not a heredoc, not `git -c`
   (neither matches the allowlist; both ask the operator). Message
   `<unit-id>: <one line>`; body = the `exit=` lines, every PLANT's red line,
   and anything the row says to paste.
7. Mark the row and commit the queue with ONE command:
   `scripts/ralph-mark.sh <unit-id> <short-hash>` — it rewrites the row to
   `- [x] <unit-id> <short-hash> — depends [...] — ...` and commits
   `ralph/next/ei7-stage0/STATE.md` alone as `ralph: <unit-id> done`. In a POOL
   LANE, write `ralph/lanes/<unit-id>.done` and commit that instead.

Commit as soon as a coherent piece compiles. A killed session with commits
resumes; one holding an hour of uncommitted work is lost.

## 4. REVIEW units — you are the stronger model

**`REVIEW-mint-e7-<x>`.** Read the row's pointers and measure the tree. Append
rows directly under the mint row, in §2's grammar. A row is atomic when it has
one VERB, touches at most about ten files, lands in one commit, states a
premise a worker can verify with grep, and names §5 checks. **The row's `cap`
is a hard limit.** If the work needs more rows than the cap, do not mint them:
write `ralph/NEEDS_HUMAN.md` with the count you measured and why, and stop —
growth past a cap is a design finding, never a queue edit. Commit
`ralph/next/ei7-stage0/STATE.md` as `REVIEW-mint-e7-<x>: <n> rows minted`, then mark the mint
row `[x]`.

**`REVIEW-audit-e7-<n>`.** Run TESTALL and PREPUSH. Read `git log` and
`git diff` since the previous audit against `sovereign/ARCH_PRINCIPLES.md`
("The twelve"). Fix what you find, behaviour-preserving, and record each finding in
`ralph/REVIEW_FINDINGS.md`: principle, path:line, fixed-in hash. A red gate
you cannot make green: §6.

## 5. Checks — from the repo root; `mkdir -p target/ralph` first

On Linux every check runs inside the `sovereign-vulkan` toolbox; if
`/run/.containerenv` does not exist on a Linux host, stop (§6) before building.

| name | command (each writes `target/ralph/<check>.log`, prints `exit=N`, tails it; `scripts/ralph-check.sh` is the one place the commands live) | passes when |
|---|---|---|
| CLEAN | `scripts/ralph-check.sh clean` | exit=0 (once per unit). No lock wrapper: at this ceiling the gate is a `du` and never runs cargo, and another campaign holds the cargo lock for ~19 min per fresh build in this tree - waiting on it here bought nothing (e7-1-scaffold lost a session to that wait). The ceiling is 256G here, not the 50G default: another campaign (build-latency) measures warm builds in this tree and a clean under it destroys their numbers - a clean is the operator's call, say so in NEEDS_HUMAN if the gate trips |
| LINT | `scripts/ralph-check.sh lint` | exit=0 |
| TEST(c) | `scripts/ralph-check.sh test c` | exit=0 |
| TESTFN(c,f) | `scripts/ralph-check.sh testfn c f` — `f` is the WHOLE test fn name (a vague filter rebuilds the workspace) | exit=0 |
| ENV | `scripts/ralph-check.sh env` | exit=0 (rows that add an env read) |
| PY(s) | `scripts/ralph-check.sh py s` — runs `python3 s --self-test`; every python file this queue creates carries one, with a planted failing input | exit=0 |
| DESKTOP | `scripts/ralph-check.sh desktop` — `npm run check` and `npm run test` in `sovereign/crates/sovereign-desktop` | exit=0 |
| LAYER | `scripts/ralph-check.sh layer` | exit=0 |
| CAMPAIGN | `scripts/ralph-check.sh campaign epistemic-index` | exit=0 (rows that edit the campaign file) |
| DOCS | `scripts/ralph-check.sh docs` | exit=0 (rows that edit a doc) |
| PLANT(x) | make the one-line violation `x` names, run the gate the row names (LINT or LAYER or TEST(c)), paste its red line, `git checkout --` the plant, run the gate again | red with the plant, exit=0 without it |
| NODE(d) | `scripts/ralph-check.sh node d` (node 20 and npx are in the toolbox; pin every npm version the row names) | exit=0 |
| PILOT | `scripts/ralph-check.sh pilot` — needs the deployed daemon UP and the `chaos-secret-agent` corpus installed; it never stops or restarts the daemon | exit=0 and the table the row names |
| TESTALL | `scripts/ralph-check.sh testall` | exit=0 (audits only) |
| PREPUSH | `scripts/ralph-check.sh prepush` | exit=0 (audits only) |

Never print a whole log into the session; grep it. A PLANT that stays green is
§6: the enforcement does not enforce.

## 6. Stopping

- **`ralph/NEEDS_HUMAN.md`** — a decision package, not a question: (a) the
  unit id and its row; (b) the exact commands you ran and their ACTUAL output,
  trimmed; (c) what the operator must decide, numbered, with file:line;
  (d) "edit or mark the row in ralph/next/ei7-stage0/STATE.md, then
  `rm ralph/STOP ralph/NEEDS_HUMAN.md`". Leave the tree compiling. Commit
  nothing broken. Then stop.
- **`ralph/DONE`** — only when every row in `ralph/next/ei7-stage0/STATE.md` is `[x]`.

## 7. Hard rules

- Never `cd` inside a shell command, and never write a path with `../`. Every
  path is relative to the repo root, where every command already runs.
  opencode's permission check resolves `cd X && ../../y` against the wrong
  base, auto-rejects a path INSIDE this repo, and ends your session with the
  unit half done (e7-1-scaffold lost a session to `/home/sovereign/apps/...`).
  For a scratch build dir use `target/ralph/bundle/` by its repo-relative path
  in every argument (`npx esbuild target/ralph/bundle/entry.js --outfile=sovereign/apps/...`).
- Change files with the Edit and Write tools, never with a shell heredoc
  (`cat >> f <<EOF`, `python3 - <<EOF`): edits inside the repo are accepted
  outright, a heredoc asks the operator and is denied after 600 s unattended.
- Never `git push`, never `--no-verify`, never rewrite history.
- No `Co-Authored-By` line and no assistant name in any commit.
- Never `--update-baseline` a ratchet; never add an `[[exception]]` row or widen
  an `except` list unless the row names that exact row.
- Never build `--release`. Never run bare `cargo build`/`test`/`check`/`clippy`
  — only the §5 commands, which take the cargo lock.
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`, or
  `scripts/ralph*`. Never stop or restart the DEPLOYED daemon. Never edit
  `research/ontology-retrieval/PRE-REG-*.md` except its `## Deviations` section,
  and only when the row says so: the bars are the operator's.
- **No tuning.** Never change a walk budget, a prompt, a threshold or a
  retrieval constant's DEFAULT. The one knob this queue adds ships default-off.
  A row that seems to need a default changed is §6.
- Never acquire a study corpus (ANS, EDGAR, NarrativeQA) and make no outbound
  network request. Stage 0 runs on what is installed.
- When a row changes a subsystem `sovereign/SYSTEM_OVERVIEW.md` describes, fix
  that one line in the same commit (principle 3). Nothing more.
