# ralph — the five-programs queue, one unit per session

You are a worker executing ONE unit of the `five-programs` campaign
(docs/FIVE_PROGRAMS.md; the row catalog is
docs/FIVE_PROGRAMS_DECISIONS.tsv — find your row by its source→target pair).
Every design decision already lives in FIVE_PROGRAMS §12 (six decisions, all
taken — cite the decision number in your commit body) and §11 (the seam and
refusal history: a probe marked REFUSED there is dead — do not re-probe it).
A fresh session starts every iteration: this file, `ralph/next/five-programs/STATE.md` and the repo
are your whole memory. Any other STATE.md under `ralph/` is ANOTHER queue —
never open it, never mark it. When a row and the tree disagree, you stop (§6);
you never improvise around it.

## 0. Standing facts (do not re-derive)

- Branch `cut`, tag `pre-cut` = 6bda3417a. NOTHING IS EVER PUSHED — push is
  the operator's call.
- `cargo xtask boundary-gate` (from `corpus-engine/`) is the only scoreboard:
  EXIT=1 with `N violation(s)`. The raw count goes in EVERY commit body.
  `layer-gate` stays ✓.
- The atlas carve (Phase A) has LANDED: the `corpus-engine-atlas-reader` leaf
  holds the whole resolved-atlas READ surface; corpus-engine keeps writers,
  backfill, freshness deciders, the class-composite opener, the chapters
  manifest and `read_section_rows`. Consumers' paths are NOT yet repointed —
  deliberately: path repoints alone close ZERO gate edges (the 8 consumer
  crates keep non-atlas residue), so each crate's repoint is folded into the
  row that closes ITS last corpus-engine use (fp-14). Do not repoint early.
- ~331 old consumer enrichment refs already resolve into
  `understanding_vocab` (atoms/edges/stable_key/read fns/ATLAS_DIRNAME/
  skeleton) — repointing those is a path rewrite, part of fp-14's folds.
- Builds on this host MUST go through the toolbox:
  `toolbox run -c sovereign-vulkan bash -lc '...'` (native host builds die on
  llama-cpp-sys-4). Compile is the gate: scoped
  `./scripts/sovereign-lint.sh --human` per step, `--full` when a unit
  touches a shared crate. The test/lint cadence is the LOOP's, not per-step —
  see `The loop` above: every 5th net-decreasing closure runs
  `sovereign-test.sh` + `sovereign-lint.sh --full`, both green.
- CLEAN trips are REPORT-AFTER: the gate's du and its `cargo clean` are one
  invocation (`dev-build.sh --clean --gate-only`), so a run over the 256G
  ceiling cleans (~300G) and rebuilds (~5 min) — it cannot report first.
  Run it, then write the trip report (sizes, files removed, rebuild verdict)
  to `ralph/next/five-programs/ctl/NEEDS_HUMAN.md`; under the ceiling it is a bare du,
  exit=0. fp-0 precedent 2026-09-22: 288G → 307.3GiB removed, rebuild green
  4m45s.
- A `[[package_leaf]]` must NOT also be a package member — check which list a
  crate line lives in (layer vs package) in `quality/ARCH_LAYERS.toml`.
  `sovereign-contracts` (layer 0) may not name understanding-vocab or
  corpus-index (layer 1): layer-gate blocks it.
- After a crate fork expect ~2 lint rounds (`sovereign_daemon::` → `crate::`;
  sibling paths → `super::`). A moved module keeps every pub item reachable
  at its historical path (a re-export, never a twin — ARCH §10.6). Child
  `mod` decls under a `#[path = "..."]`-loaded file need explicit `#[path]`
  themselves (the tests/main/gossip_integration precedent).
- Every file the arch-gate flags is SPLIT, never re-pinned; never
  `--update-baseline` on a dirty tree.

## The loop — the operator's cadence, verbatim (2026-09-23)

```
until boundary-gate exits 0 {
    run the gate;                    # toolbox + cargo lock; group BY TARGET
    identify gaps;                   # first dep-ready row; scout before cutting
    close gaps;                      # cutters never cargo/git; you hold the compile
    if (closures_since_last_full_check % 5 == 0) {
        run sovereign-test.sh AND sovereign-lint.sh --full;   # both green
    } else {
        all-targets build only;      # full resolved feature set
    }
}
```

- The counter lives at the top of `ralph/next/five-programs/STATE.md` ("closures since last full
  check"). After YOUR row closes net-decreasing: increment it; at 5 run the
  full test + lint pair, repair anything red IN THE SAME STEP, reset to 0.
  A rolled-back or delta-0 row does NOT count (fp-29's refusal is the
  precedent — net +1 as spec'd means refused, not counted).
- A red test is repaired, never queued: classify drift-vs-regression FIRST
  (read what the census pins, find the commit that moved the code), then fix
  in the correct direction. A census's failure message is an argument, not
  an order — fp-53 refused chunk_provenance's own "delete from
  MANUFACTURED" advice because the producer was alive and the scan was
  broken. Faking a test, weakening a census, or fixing the test instead of
  the tree is a fake zero and halts for the operator.
- Build-only between checks still means the FULL all-targets build with the
  repo's resolved feature contract — through the scripts, never bare cargo.
- Standing rules the whole loop obeys: net-decreasing only (a move that adds
  a red edge elsewhere is rolled back and recorded); no fake zeros (never
  promote a program-owned store to a leaf; leaf admissions are OPERATOR
  decisions per the §12 decision 3a ladder — named refusal of each existing
  home, the two programs sharing the vocabulary, the leaf count in the
  burn-down); ALL TESTS GREEN IS STANDING (operator, 2026-09-23: "doesn't
  matter who caused them"); commit as you go; NOTHING IS EVER PUSHED; DONE
  only when the gate exits 0 AND §11's three finish conditions hold.

The campaign's one rule is also yours: **strictly necessary.** Change nothing
the row does not name — no renames, no nearby cleanup, no new abstraction, no
comment beyond the one the row asks for (ARCH principle 2).

## 1. Pick your unit

1. Open `ralph/next/five-programs/STATE.md`. If a row is `[~]`, that is your unit — a previous
   session was killed in the middle of it. Continue it.
2. Otherwise your unit is the FIRST `[ ]` row, top to bottom, whose
   `depends [...]` ids are all `[x]`.
3. If this prompt opens with a `POOL LANE` note, the note names your unit and
   you do not edit `ralph/next/five-programs/STATE.md` at all.
4. If the loop told you the tree holds uncommitted work, it belongs to the
   `[~]` unit: read `git status` and `git diff`, keep what is right, continue.
5. If the loop's note names your unit (`Your unit: <id>`), open only that row.

## 2. Reading a row

`- [ ] <id> — depends [<ids>] — <VERB> <what> — read: <pointers> — check: <checks>`

- **read:** the only files you read, besides the files you edit. Pointer keys
  are defined at the top of `ralph/next/five-programs/STATE.md`. `O2 step 3` means item 3 under
  `## Steps` in that order file.
- **check:** named checks from §5, run in the order written. All must pass.
  "paste X" means put X's output (trimmed) in the commit body.

| prefix | what you do |
|---|---|
| `fp-` | build the unit (§3) |
| `HUMAN-fp-` | never do it and never mark it: write `ralph/next/five-programs/ctl/NEEDS_HUMAN.md` (§6) from the row, then stop |

## 3. Building a unit

1. Mark the row `[~]` in `ralph/next/five-programs/STATE.md`. Do not commit that edit on its own.
2. **Premise check before any edit.** The row states facts — a path, a symbol,
   a count. Verify each with `ls`, `git grep` or `grep` first. If one is
   false, stop: §6, with what you found.
3. Do the VERB. Nothing else.
4. Run CLEAN once (§5); LINT is the unit's first build.
5. Run the row's checks. On a failure: read the log, fix, re-run. Two honest
   attempts at the same failure and still red: §6.
6. Commit: `git add` the paths you changed, by name — never `git add -A`,
   never `target/` or `ralph/log*`. Write the message to
   `target/ralph/five-programs/commit-msg.txt` with the Write tool, then
   `git commit -F target/ralph/five-programs/commit-msg.txt` — not a heredoc, not `git -c`
   (neither matches the allowlist; both ask the operator). Message
   `<unit-id>: <one line>`; body = the `exit=` lines, every PLANT's red line,
   and anything the row says to paste.
7. Mark the row and commit the queue with ONE command:
   `scripts/ralph-mark.sh <unit-id> <short-hash>` — it rewrites the row to
   `- [x] <unit-id> <short-hash> — depends [...] — ...` and commits
   `ralph/next/five-programs/STATE.md` alone as `ralph: <unit-id> done`. In a POOL
   LANE, write `ralph/lanes/<unit-id>.done` and commit that instead.

Commit as soon as a coherent piece compiles. A killed session with commits
resumes; one holding an hour of uncommitted work is lost.

## 4. REVIEW units — you are the stronger model

**`REVIEW-mint-fp-<x>`.** Read the row's pointers and measure the tree. Append
rows directly under the mint row, in §2's grammar. A row is atomic when it has
one VERB, touches at most about ten files, lands in one commit, states a
premise a worker can verify with grep, and names §5 checks. **The row's `cap`
is a hard limit.** If the work needs more rows than the cap, do not mint them:
write `ralph/next/five-programs/ctl/NEEDS_HUMAN.md` with the count you measured and why, and stop —
growth past a cap is a design finding, never a queue edit. Commit
`ralph/next/five-programs/STATE.md` as `REVIEW-mint-fp-<x>: <n> rows minted`, then mark the mint
row `[x]`.

**`REVIEW-audit-fp-<n>`.** Run TESTALL and PREPUSH. Read `git log` and
`git diff` since the previous audit against `sovereign/ARCH_PRINCIPLES.md`
("The twelve"). Fix what you find, behaviour-preserving, and record each finding in
`ralph/REVIEW_FINDINGS.md`: principle, path:line, fixed-in hash. A red gate
you cannot make green: §6.

## 5. Checks — from the repo root; `mkdir -p target/ralph/five-programs` first

On Linux every check runs inside the `sovereign-vulkan` toolbox; if
`/run/.containerenv` does not exist on a Linux host, stop (§6) before building.

| name | command (each writes `target/ralph/five-programs/<check>.log`, prints `exit=N`, tails it; `scripts/ralph-check.sh` is the one place the commands live) | passes when |
|---|---|---|
| CLEAN | `scripts/ralph-check.sh clean` | exit=0 (once per unit). No lock wrapper: at this ceiling the gate is a `du` and never runs cargo, and another campaign holds the cargo lock for ~19 min per fresh build in this tree - waiting on it here bought nothing (rd-1-scaffold lost a session to that wait). The ceiling is 256G here, not the 50G default: another campaign (build-latency) measures warm builds in this tree and a clean under it destroys their numbers - a clean is the operator's call, say so in NEEDS_HUMAN if the gate trips |
| LINT | `scripts/ralph-check.sh lint` | exit=0 |
| TEST(c) | `scripts/ralph-check.sh test c` | exit=0 |
| LAYER | `scripts/ralph-check.sh layer` | exit=0 |
| BOUNDARY | `scripts/ralph-check.sh boundary` — the burn-down count (EXIT=1 is its honest state; the COUNT is what your commit body quotes) | prints `N violation(s)` |
| LAYER | `scripts/ralph-check.sh layer` (builtin) | exit=0 |
| COMPILE | `scripts/ralph-check.sh compile` — the toolbox-wrapped scoped compile; on this host it is the only honest LINT for anything that reaches llama-cpp-sys-4 | exit=0 |
| ARCH | `scripts/ralph-check.sh arch` (builtin; rides on LINT in the base) | exit=0 |
| DOCS | `scripts/ralph-check.sh docs` | exit=0 (rows that edit a doc) |
| PLANT(x) | make the one-line violation `x` names, run the gate the row names (LINT or LAYER or TEST(c)), paste its red line, `git checkout --` the plant, run the gate again | red with the plant, exit=0 without it |
| TESTALL | `scripts/ralph-check.sh testall` | exit=0 (audits only) |
| PREPUSH | `scripts/ralph-check.sh prepush` | exit=0 (audits only) |

Never print a whole log into the session; grep it. A PLANT that stays green is
§6: the enforcement does not enforce.

## 6. Stopping

- **`ralph/next/five-programs/ctl/NEEDS_HUMAN.md`** — a decision package, not a question: (a) the
  unit id and its row; (b) the exact commands you ran and their ACTUAL output,
  trimmed; (c) what the operator must decide, numbered, with file:line;
  (d) "edit or mark the row in ralph/next/five-programs/STATE.md, then
  `rm ralph/next/five-programs/ctl/STOP ralph/next/five-programs/ctl/NEEDS_HUMAN.md`". Leave the tree compiling. Commit
  nothing broken. Then stop.
- **`ralph/next/five-programs/ctl/DONE`** — only when every row in `ralph/next/five-programs/STATE.md` is `[x]`.

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
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`, or
  `scripts/ralph*`. Never stop or restart the DEPLOYED daemon (the one
  `svrn daemon status` names). Never touch `ralph/STOP`,
  `ralph/NEEDS_HUMAN.md`, or another queue's directory under `ralph/next/`.
- **Behaviour-preserving or reported.** This campaign re-cuts the repo; a
  route, tool or read that worked must still work. When a row dials a
  capability away from the daemon, the stub REPORTS ABSENCE (§12 decision 2,
  principle 6) — never a silent fallback, never a 404 where a named absence
  belongs. A row that would change the TEXT of an answer or a default
  without its decision saying so is §6.
- A `[[package_leaf]]` admission must be one the queue row names exactly; the
  leaf test is: no fs, no store, workspace deps ⊆ the leaf's allow-list.
- When a row changes a subsystem `sovereign/SYSTEM_OVERVIEW.md` describes, fix
  that one line in the same commit (principle 3). Nothing more.
