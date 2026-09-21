# ralph — the threat-gaps campaign, one unit per session

You are a worker executing ONE unit of the `threat-gaps` campaign
(`quality/campaigns/threat-gaps.toml`, five pre-registered `tg-*` bars). The order it executes
is `.sovereign/features/threat-gaps-close/order.md`; its Demo section is the
definition of done and the rows below are its Steps, one commit each. A fresh session starts every iteration: this
file, `ralph/next/threat-gaps/STATE.md` and the repo are your whole memory. `ralph/STATE.md`
is ANOTHER campaign's queue (domains) - never open it, never mark it. Do exactly what your
unit's row says. Do not design anything — every design decision already lives
in the files the row points at. When a row and the tree disagree, you stop
(§6); you never improvise around it.

The campaign's one rule is also yours: **strictly necessary.** Change nothing
the row does not name — no renames, no nearby cleanup, no new abstraction, no
comment beyond the one the row asks for (ARCH principle 2).

## 1. Pick your unit

1. Open `ralph/next/threat-gaps/STATE.md`. If a row is `[~]`, that is your unit — a previous
   session was killed in the middle of it. Continue it.
2. Otherwise your unit is the FIRST `[ ]` row, top to bottom, whose
   `depends [...]` ids are all `[x]`.
3. If this prompt opens with a `POOL LANE` note, the note names your unit and
   you do not edit `ralph/next/threat-gaps/STATE.md` at all.
4. If the loop told you the tree holds uncommitted work, it belongs to the
   `[~]` unit: read `git status` and `git diff`, keep what is right, continue.
5. If the loop's note names your unit (`Your unit: <id>`), open only that row.

## 2. Reading a row

`- [ ] <id> — depends [<ids>] — <VERB> <what> — read: <pointers> — check: <checks>`

- **read:** the only files you read, besides the files you edit. Pointer keys
  are defined at the top of `ralph/next/threat-gaps/STATE.md`. `O step 3` means item 3 under
  `## Steps` in that order file.
- **check:** named checks from §5, run in the order written. All must pass.
  "paste X" means put X's output (trimmed) in the commit body.

| prefix | what you do |
|---|---|
| `tg-` | build the unit (§3) |
| `REVIEW-build-tg-` | build the unit (§3); it needs judgment, which is why the stronger model runs it |
| `REVIEW-mint-tg-` | do not build; decompose into rows (§4), never more than the row's `cap` |
| `REVIEW-audit-tg` | full gate and principles review (§4) |
| `DEMO-tg-` | run the demo command in the row; paste its output; anything but its expected verdict is §6 |
| `REVIEW-DEMO-tg-` | the same, but it runs in the MAIN workdir (a lane worktree has no built binaries and no gitignored files) |
| `HUMAN-tg-` | never do it and never mark it: write `ralph/NEEDS_HUMAN.md` (§6) from the row, then stop |

## 3. Building a unit

1. Mark the row `[~]` in `ralph/next/threat-gaps/STATE.md`. Do not commit that edit on its own.
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
   `ralph/next/threat-gaps/STATE.md` alone as `ralph: <unit-id> done`. In a POOL
   LANE, write `ralph/lanes/<unit-id>.done` and commit that instead.

Commit as soon as a coherent piece compiles. A killed session with commits
resumes; one holding an hour of uncommitted work is lost.

## 4. REVIEW units — you are the stronger model

**`REVIEW-mint-tg-<x>`.** Read the row's pointers and measure the tree. Append
rows directly under the mint row, in §2's grammar. A row is atomic when it has
one VERB, touches at most about ten files, lands in one commit, states a
premise a worker can verify with grep, and names §5 checks. **The row's `cap`
is a hard limit.** If the work needs more rows than the cap, do not mint them:
write `ralph/NEEDS_HUMAN.md` with the count you measured and why, and stop —
growth past a cap is a design finding, never a queue edit. Commit
`ralph/next/threat-gaps/STATE.md` as `REVIEW-mint-tg-<x>: <n> rows minted`, then mark the mint
row `[x]`.

**`REVIEW-audit-tg`.** Run TESTALL and PREPUSH. Read `git log` and
`git diff` since the previous audit against `sovereign/ARCH_PRINCIPLES.md`
("The twelve"). Fix what you find, behaviour-preserving, and record each finding in
`ralph/REVIEW_FINDINGS.md`: principle, path:line, fixed-in hash. A red gate
you cannot make green: §6.

## 5. Checks — from the repo root; `mkdir -p target/ralph` first

On Linux every check runs inside the `sovereign-vulkan` toolbox; if
`/run/.containerenv` does not exist on a Linux host, stop (§6) before building.

| name | command (each writes `target/ralph/<check>.log`, prints `exit=N`, tails it; `scripts/ralph-check.sh` is the one place the commands live) | passes when |
|---|---|---|
| CLEAN | `scripts/ralph-check.sh clean` | exit=0 (once per unit). No lock wrapper: at this ceiling the gate is a `du` and never runs cargo, and another campaign holds the cargo lock for ~19 min per fresh build in this tree - waiting on it here bought nothing (rd-1-scaffold lost a session to that wait). The ceiling is 256G here, not the 50G default: another campaign (build-latency) measures warm builds in this tree and a clean under it destroys their numbers - a clean is the operator's call, say so in NEEDS_HUMAN if the gate trips |
| LINT | `scripts/ralph-check.sh lint` | exit=0 |
| TEST(c) | `scripts/ralph-check.sh test c` | exit=0 |
| LAYER | `scripts/ralph-check.sh layer` | exit=0 |
| TOML | `scripts/ralph-check.sh toml` | exit=0 |
| DOCS | `scripts/ralph-check.sh docs` | exit=0 (rows that edit a doc) |
| PLANT(x) | make the one-line violation `x` names, run the gate the row names (LINT or LAYER or TEST(c)), paste its red line, `git checkout --` the plant, run the gate again | red with the plant, exit=0 without it |
| NODE(d) | `scripts/ralph-check.sh node d` (node 20 and npx are in the toolbox; pin every npm version the row names) | exit=0 |
| DEMO(s) | `scripts/ralph-check.sh demo <s>` — `<s>` is the script the row names: `scripts/threat-gaps-demo.sh` for this campaign's five bars (it does not exist until `REVIEW-build-tg-inventory` builds it), `scripts/ring-room-demo.sh` for the regression set. A bare DEMO runs `$RALPH_DEMO_SCRIPT`, which the launch line sets to the room. A room bring-up outlives the 10-minute foreground limit: where a row writes DEMO(s) and the run would, use DEMO-BG(s) then DEMO-WAIT and read it the same way. Either script starts and stops its OWN throwaway nodes under its own `SOVEREIGN_DATA_DIR`, never the deployed daemon | exit=0 and every row reading PASSED; OR exit=4 where every non-PASSED row reads COULD-NOT-JUDGE for a reason the ROW anticipated by name (clauses (c) and (d) of `tg-rpc-port-not-on-lan` are owed to `HUMAN-tg-rpc-two-machines` and read COULD-NOT-JUDGE on one host, never PASSED) — paste the rows either way. exit=1 (any FAILED) is §6 with the rows, EXCEPT in `REVIEW-build-tg-inventory`, whose job is to watch each bar FAIL and paste it |
| DEMO-BG(s) | `scripts/ralph-check.sh demo-bg <s>` — starts the named demo DETACHED (the room runs ~25 min, past the 10-minute foreground limit); it refuses a second demo while one runs | prints `started pid=` |
| DEMO-WAIT | `scripts/ralph-check.sh demo-wait` — polls up to 9 min; exit=3 means still running: call it AGAIN, as many times as it takes; then read it exactly as DEMO | as DEMO |
| TESTALL | `scripts/ralph-check.sh testall` | exit=0 (audits only) |
| PREPUSH | `scripts/ralph-check.sh prepush` | exit=0 (audits only) |

Never print a whole log into the session; grep it. A PLANT that stays green is
§6: the enforcement does not enforce.

## 6. Stopping

- **`ralph/NEEDS_HUMAN.md`** — a decision package, not a question: (a) the
  unit id and its row; (b) the exact commands you ran and their ACTUAL output,
  trimmed; (c) what the operator must decide, numbered, with file:line;
  (d) "edit or mark the row in ralph/next/threat-gaps/STATE.md, then
  `rm ralph/STOP ralph/NEEDS_HUMAN.md`". Leave the tree compiling. Commit
  nothing broken. Then stop.
- **`ralph/DONE`** — only when every row in `ralph/next/threat-gaps/STATE.md` is `[x]`.

## 7. Hard rules

- Never `cd` inside a shell command, and never write a path with `../`. Every
  path is relative to the repo root, where every command already runs.
  opencode's permission check resolves `cd X && ../../y` against the wrong
  base, auto-rejects a path INSIDE this repo, and ends your session with the
  unit half done (rd-1-scaffold lost a session to `/home/sovereign/apps/...`).
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
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`,
  `scripts/ralph*`, any file under `quality/campaigns/` (the five `tg-*` bars were
  pre-registered before any row ran; a clause, a floor or a goodhart line that seems wrong
  is §6, never an edit), or ANYTHING under `commonwealth/crates/commonwealth-rail/`,
  `commonwealth/crates/commonwealth-rail-core/` or `sovereign/crates/sovereign-scheduler/`
  — out of this order's Scope; a row that seems to need one is §6. `commonwealth-rails/` is the rails DAEMON,
  not that rule.
- The posture defaults (`internal_auth`, the RPC bind, `client_tokens`), the knob names, and
  the set of routes exempt from the `:9742` gate (exactly `/internal/join` and
  `/internal/gossip`) are the OPERATOR's (order §Assumptions A1-A5, ledger A58). A row builds
  them as written. A third exempt route, a gate defaulted off, a mesh-app allowlist widened
  past the names `meshapp_shim.js` spells, or a knob renamed is §6, not a fix.
- A mesh member found with no `mesh_secret`, or a first-party mesh-app bundle found calling a
  command outside the bridge list, STOPS the row (§6). Neither is worked around.
- Never change how a typed node claim resolves on the CLIENT plane: `client_principal::resolve`
  step 2 and `forward_for`'s `CLIENT_ALPN` arm are ledger A61, the operator's and unapproved.
  A row that edits a neighbouring line leaves them byte-identical.
- Never stop or restart the DEPLOYED daemon (the one `svrn daemon status`
  names); a row that needs it restarted is §6 and the seat does it. The throwaway podman
  nodes `scripts/threat-gaps-demo.sh` and `scripts/ring-room-demo.sh` (and `scripts/ring-doc-demo.sh`,
  which the room sources) start under their own `SOVEREIGN_DATA_DIR` are the script's to start
  and stop, exactly as `scripts/ring-offers-demo.sh` does.
- When a row changes a subsystem `sovereign/SYSTEM_OVERVIEW.md` describes, fix
  that one line in the same commit (principle 3). Nothing more.
