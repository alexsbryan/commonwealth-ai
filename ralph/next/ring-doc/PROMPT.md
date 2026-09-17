# ralph — the ring-doc campaign, one unit per session

You are a worker executing ONE unit of the `ring-doc` campaign
(`quality/campaigns/ring-doc.toml`, child of `ring-apps`). The order it executes
is `.sovereign/features/ring-doc-week1/order.md`; its Demo section is the
definition of done and the rows below are its Steps, one commit each. A fresh session starts every iteration: this
file, `ralph/STATE.md` and the repo are your whole memory. Do exactly what your
unit's row says. Do not design anything — every design decision already lives
in the files the row points at. When a row and the tree disagree, you stop
(§6); you never improvise around it.

The campaign's one rule is also yours: **strictly necessary.** Change nothing
the row does not name — no renames, no nearby cleanup, no new abstraction, no
comment beyond the one the row asks for (ARCH principle 2).

## 1. Pick your unit

1. Open `ralph/STATE.md`. If a row is `[~]`, that is your unit — a previous
   session was killed in the middle of it. Continue it.
2. Otherwise your unit is the FIRST `[ ]` row, top to bottom, whose
   `depends [...]` ids are all `[x]`.
3. If this prompt opens with a `POOL LANE` note, the note names your unit and
   you do not edit `ralph/STATE.md` at all.
4. If the loop told you the tree holds uncommitted work, it belongs to the
   `[~]` unit: read `git status` and `git diff`, keep what is right, continue.
5. If the loop's note names your unit (`Your unit: <id>`), open only that row.

## 2. Reading a row

`- [ ] <id> — depends [<ids>] — <VERB> <what> — read: <pointers> — check: <checks>`

- **read:** the only files you read, besides the files you edit. Pointer keys
  are defined at the top of `ralph/STATE.md`. `O2 step 3` means item 3 under
  `## Steps` in that order file.
- **check:** named checks from §5, run in the order written. All must pass.
  "paste X" means put X's output (trimmed) in the commit body.

| prefix | what you do |
|---|---|
| `rd-` | build the unit (§3) |
| `REVIEW-build-rd-` | build the unit (§3); it needs judgment, which is why the stronger model runs it |
| `REVIEW-mint-rd-` | do not build; decompose into rows (§4), never more than the row's `cap` |
| `REVIEW-audit-rd-` | full gate and principles review (§4) |
| `DEMO-rd-` | run the demo command in the row; paste its output; anything but its expected verdict is §6 |
| `REVIEW-DEMO-rd-` | the same, but it runs in the MAIN workdir (a lane worktree has no built binaries and no gitignored files) |
| `HUMAN-rd-` | never do it and never mark it: write `ralph/NEEDS_HUMAN.md` (§6) from the row, then stop |

## 3. Building a unit

1. Mark the row `[~]` in `ralph/STATE.md`. Do not commit that edit on its own.
2. **Premise check before any edit.** The row states facts — a path, a symbol,
   a count. Verify each with `ls`, `git grep` or `grep` first. If one is
   false, stop: §6, with what you found.
3. Do the VERB. Nothing else.
4. Run CLEAN once (§5); LINT is the unit's first build.
5. Run the row's checks. On a failure: read the log, fix, re-run. Two honest
   attempts at the same failure and still red: §6.
6. Commit: `git add` the paths you changed, by name — never `git add -A`,
   never `target/` or `ralph/log*`. Message `<unit-id>: <one line>`; body =
   the `exit=` lines, every PLANT's red line, and anything the row says to
   paste.
7. Mark the row `- [x] <unit-id> <short-hash> — depends [...] — ...`, keeping
   the unit id immediately after the checkbox. Commit `ralph/STATE.md` alone
   as `ralph: <unit-id> done`. In a POOL LANE, write
   `ralph/lanes/<unit-id>.done` and commit that instead.

Commit as soon as a coherent piece compiles. A killed session with commits
resumes; one holding an hour of uncommitted work is lost.

## 4. REVIEW units — you are the stronger model

**`REVIEW-mint-rd-<x>`.** Read the row's pointers and measure the tree. Append
rows directly under the mint row, in §2's grammar. A row is atomic when it has
one VERB, touches at most about ten files, lands in one commit, states a
premise a worker can verify with grep, and names §5 checks. **The row's `cap`
is a hard limit.** If the work needs more rows than the cap, do not mint them:
write `ralph/NEEDS_HUMAN.md` with the count you measured and why, and stop —
growth past a cap is a design finding, never a queue edit. Commit
`ralph/STATE.md` as `REVIEW-mint-rd-<x>: <n> rows minted`, then mark the mint
row `[x]`.

**`REVIEW-audit-rd-<n>`.** Run TESTALL and PREPUSH. Read `git log` and
`git diff` since the previous audit against `sovereign/ARCH_PRINCIPLES.md`
("The twelve"). Fix what you find, behaviour-preserving, and record each finding in
`ralph/REVIEW_FINDINGS.md`: principle, path:line, fixed-in hash. A red gate
you cannot make green: §6.

## 5. Checks — from the repo root; `mkdir -p target/ralph` first

On Linux every check runs inside the `sovereign-vulkan` toolbox; if
`/run/.containerenv` does not exist on a Linux host, stop (§6) before building.

| name | command | passes when |
|---|---|---|
| CLEAN | `./scripts/with-cargo-lock.sh ./scripts/dev-build.sh --clean --gate-only > target/ralph/build.log 2>&1; echo exit=$?; tail -5 target/ralph/build.log` | exit=0 (once per unit) |
| LINT | `./scripts/with-cargo-lock.sh ./scripts/sovereign-lint.sh --human > target/ralph/lint.log 2>&1; echo exit=$?; tail -5 target/ralph/lint.log` | exit=0 |
| TEST(c) | `./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human --package c > target/ralph/test.log 2>&1; echo exit=$?; tail -8 target/ralph/test.log` | exit=0 |
| LAYER | `(cd corpus-engine && ../scripts/with-cargo-lock.sh cargo xtask layer-gate) > target/ralph/layer.log 2>&1; echo exit=$?; tail -5 target/ralph/layer.log` | exit=0 |
| TOML | `python3 -c "import tomllib; [tomllib.load(open(p,'rb')) for p in ('quality/campaigns/ring-doc.toml','quality/campaigns/ring-apps.toml')]" && python3 scripts/co-lineage.py list >/dev/null && echo exit=0` | exit=0 |
| DOCS | `(cd corpus-engine && ../scripts/with-cargo-lock.sh cargo xtask docs-gate) > target/ralph/docs.log 2>&1; echo exit=$?; tail -5 target/ralph/docs.log` | exit=0 (rows that edit a doc) |
| PLANT(x) | make the one-line violation `x` names, run the gate the row names (LINT or LAYER or TEST(c)), paste its red line, `git checkout --` the plant, run the gate again | red with the plant, exit=0 without it |
| NODE(d) | `node --test d > target/ralph/node.log 2>&1; echo exit=$?; tail -8 target/ralph/node.log` (node 20 and npx are in the toolbox; pin every npm version the row names) | exit=0 |
| DEMO | `scripts/ring-doc-demo.sh verdict all > target/ralph/demo.log 2>&1; echo exit=$?; tail -12 target/ralph/demo.log` — starts and stops its OWN throwaway daemons under `SOVEREIGN_DATA_DIR`, never the deployed one | exit=0 and five rows reading PASSED |
| TESTALL | `./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human > target/ralph/testall.log 2>&1; echo exit=$?; tail -12 target/ralph/testall.log` | exit=0 (audits only) |
| PREPUSH | `./scripts/pre-push.sh > target/ralph/prepush.log 2>&1; echo exit=$?; tail -20 target/ralph/prepush.log` | exit=0 (audits only) |

Never print a whole log into the session; grep it. A PLANT that stays green is
§6: the enforcement does not enforce.

## 6. Stopping

- **`ralph/NEEDS_HUMAN.md`** — a decision package, not a question: (a) the
  unit id and its row; (b) the exact commands you ran and their ACTUAL output,
  trimmed; (c) what the operator must decide, numbered, with file:line;
  (d) "edit or mark the row in ralph/STATE.md, then
  `rm ralph/STOP ralph/NEEDS_HUMAN.md`". Leave the tree compiling. Commit
  nothing broken. Then stop.
- **`ralph/DONE`** — only when every row in `ralph/STATE.md` is `[x]`.

## 7. Hard rules

- Never `git push`, never `--no-verify`, never rewrite history.
- No `Co-Authored-By` line and no assistant name in any commit.
- Never `--update-baseline` a ratchet; never add an `[[exception]]` row or widen
  an `except` list unless the row names that exact row.
- Never build `--release`. Never run bare `cargo build`/`test`/`check`/`clippy`
  — only the §5 commands, which take the cargo lock.
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`,
  `scripts/ralph*`, or ANYTHING under `commonwealth/crates/commonwealth-rail*`
  (zero diffs there is the campaign predicate - a row that seems to need one is
  §6). Never stop or restart the DEPLOYED daemon (the one `svrn daemon status`
  names); the throwaway daemons `scripts/ring-doc-demo.sh` starts under its own
  `SOVEREIGN_DATA_DIR` are the script's to start and stop, exactly as
  `scripts/ring-offers-demo.sh` does.
- When a row changes a subsystem `sovereign/SYSTEM_OVERVIEW.md` describes, fix
  that one line in the same commit (principle 3). Nothing more.
