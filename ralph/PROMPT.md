# ralph — the domains campaign, one unit per session

You are a worker executing ONE unit of the `domains` campaign in this
repository. A fresh session starts every iteration: this file,
`ralph/STATE.md` and the repo are your whole memory. Do exactly what your
unit's row says. Do not design anything — every design decision already lives
in the files the row points at. When a row and the tree disagree, you stop
(§6); you never improvise around it.

## 1. Pick your unit

1. Open `ralph/STATE.md`. If a row is `[~]`, that is your unit — a previous
   session was killed in the middle of it. Continue it.
2. Otherwise your unit is the FIRST `[ ]` row, top to bottom, whose
   `depends [...]` ids are all `[x]`. Rows below it are not your business.
3. If this prompt opens with a `POOL LANE` note, the note names your unit and
   you do not edit `ralph/STATE.md` at all.
4. If the loop told you the tree holds uncommitted work, it belongs to the
   `[~]` unit: read `git status` and `git diff`, keep what is right, continue.

## 2. Reading a row

`- [ ] <id> — depends [<ids>] — <VERB> <what> — read: <pointers> — check: <checks>`

- **read:** the only files you read, besides the files you edit. Pointer keys
  are defined at the top of `ralph/STATE.md` (DC, SB, DM, DT, DE, O3, O8, O9,
  O10). `§4.2` means the heading numbered 4.2. `O9 step 1` means item 1 under
  `## Steps` in that order file.
- **check:** named checks from §5, run in the order written. All must pass.
  "paste X" means put X's output (trimmed to the relevant lines) in the commit
  body.

What the id prefix tells you:

| prefix | what you do |
|---|---|
| `dm3-`, `dm-` | build the unit (§3) |
| `REVIEW-build-` | build the unit (§3); it needs judgment, which is why the stronger model runs it |
| `REVIEW-mint-` | do not build; decompose into rows (§4) |
| `REVIEW-audit-` | full gate and principles review (§4) |
| `DEMO-` | run the demo command in the row; paste its output; anything but its expected verdict is §6 |
| `HUMAN-` | never do it and never mark it: write `ralph/NEEDS_HUMAN.md` (§6) from the row, then stop |

## 3. Building a unit

1. Mark the row `[~]` in `ralph/STATE.md`. Do not commit that edit on its own.
2. **Premise check before any edit.** The row states facts — a path, a
   symbol, a count, "names no `crate::` module". Verify each with `ls`,
   `git grep` or `grep` first. If one is false, stop: §6, with what you found.
3. Do the VERB. `MOVE` uses §3a and nothing else. Change nothing the row does
   not ask for — no renames inside a move, no logic edits, no nearby cleanup
   (ARCH principle 2).
4. Run the row's checks. On a failure: read the log, fix, re-run. Two honest
   attempts at the same failure and still red: §6.
5. Commit: `git add` the paths you changed, by name — never `git add -A`, never
   `target/` or `ralph/log*`. Message `<unit-id>: <one line>`; body = the
   `exit=` lines and anything the row says to paste.
6. Mark the row `[x] <short-hash>` and commit `ralph/STATE.md` alone as
   `ralph: <unit-id> done`. In a POOL LANE, write `ralph/done/<unit-id>` and
   commit that instead.

Commit as soon as a coherent piece compiles. You can be killed at any moment;
a killed session with commits resumes, one holding an hour of uncommitted work
is lost.

### 3a. MOVE `<file>` -> `<crate>` — the only move recipe

A move keeps behaviour identical and keeps every old path compiling.

1. **Premise.** `grep -n 'crate::' <file>` (and inside its `<name>/`
   directory, if one exists). Every `crate::<m>` must be a module that already
   lives in the destination crate, or be named in the row. Otherwise §6.
2. `git mv <file> <dest>/src/<name>.rs`, and `git mv` the `<name>/` directory
   too if it exists.
3. **Destination.** Add `pub mod <name>;` to its `src/lib.rs`. For each
   external crate the moved file `use`s, copy that dependency's exact line from
   the source crate's `Cargo.toml` into the destination's.
4. **Allowed rewrites inside the moved file** — these and the ones the row
   names, nothing else: `sovereign_core::oicp::` → `oicp_types::`;
   `sovereign_core::time::` → `sovereign_time::`;
   `sovereign_core::traits::InferenceProvider` →
   `sovereign_contracts::traits::InferenceProvider`.
5. **Source.** Replace `pub mod <name>;` (or `mod <name>;`) in its `src/lib.rs`
   with `pub use <dest_crate_ident>::<name>; // shim: moved by domains <unit-id>`
   and add the destination crate to the source `Cargo.toml` if it is absent.
6. **Visibility.** If the compiler reports an item private because it now
   crosses a crate line, change that item's `pub(crate)` to `pub` and list it in
   the commit body. Any other error you cannot fix with an import path: §6 after
   two tries.

### 3b. CREATE a crate — only when a row says CREATE

Copy the `Cargo.toml` shape of the nearest sibling under
`sovereign/crates/`; add the crate to the root `Cargo.toml` `[workspace]
members`; add it to the `[[layer]]` the row names in
`quality/ARCH_LAYERS.toml`; add one line for it to `sovereign/SYSTEM_OVERVIEW.md`
§2's crate list. Its `src/lib.rs` holds only the `//!` doc the row gives.

## 4. REVIEW units — you are the stronger model

**`REVIEW-mint-<x>`.** Read the row's pointers and measure the tree (`git
grep`, `wc -l`, `sovereign tools call callers --symbol=<S>`). Append rows
directly under the mint row, in §2's grammar. A row is atomic when it has one
VERB, touches at most about ten files, lands in one commit, states a premise a
worker can verify with grep, and names §5 checks. Order rows so every row's
dependencies sit above it. Anything that needs judgment — a cycle, a
back-edge, a port, a type name — becomes its own `REVIEW-build-` row; only
mechanical work becomes a `dm-` row. Put a `REVIEW-audit-` row after every five
or so build rows, and a `HUMAN-` row before anything that adds an
`[[exception]]`, widens an `except`, or changes behaviour. Commit
`ralph/STATE.md` as `REVIEW-mint-<x>: <n> rows minted`, then mark the mint row
`[x]`. If the pointed design is contradicted by the tree, §6.

**`REVIEW-audit-<n>`.** Run TESTALL and PREPUSH. Read `git log` and `git
diff` since the previous audit's hash against `sovereign/ARCH_PRINCIPLES.md`
("The twelve", and the section of any principle you cite). Fix what you find,
behaviour-preserving, and record each finding in `ralph/REVIEW_FINDINGS.md`:
principle, path:line, fixed-in hash. Delete shims whose importers are all
repointed. A red gate you cannot make green: §6.

## 5. Checks — from the repo root; `mkdir -p target/ralph` first

| name | command | passes when |
|---|---|---|
| LINT | `./scripts/with-cargo-lock.sh ./scripts/sovereign-lint.sh --human > target/ralph/lint.log 2>&1; echo exit=$?; tail -5 target/ralph/lint.log` | exit=0 |
| TEST(c) | `./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human --package c > target/ralph/test.log 2>&1; echo exit=$?; tail -8 target/ralph/test.log` | exit=0; exit=4 (zero tests) only for a crate created in this unit, said in the commit |
| LAYER | `(cd corpus-engine && ../scripts/with-cargo-lock.sh cargo xtask layer-gate) > target/ralph/layer.log 2>&1; echo exit=$?; tail -5 target/ralph/layer.log` | exit=0 |
| BOUNDARY | same, `boundary-gate`, log `target/ralph/boundary.log` | exit=0, unless the row says red is expected |
| DOCS | same, `docs-gate`, log `target/ralph/docs.log` | exit=0 |
| INSTR | same, `instrument-gate`, log `target/ralph/instr.log` | exit=0 |
| TOML | `python3 -c "import tomllib; [tomllib.load(open(p,'rb')) for p in ('quality/DOMAINS.toml','quality/campaigns/domains.toml','quality/ARCH_LAYERS.toml')]" && echo exit=0` | exit=0 |
| CENSUS | `python3 scripts/domains-census.py --self-test > target/ralph/census.log 2>&1; echo exit=$?; tail -20 target/ralph/census.log` | exit=0 |
| CALLERS(S) | `sovereign tools call callers --symbol=S` | paste the output |
| TESTALL | `./scripts/with-cargo-lock.sh ./scripts/sovereign-test.sh --human > target/ralph/testall.log 2>&1; echo exit=$?; tail -12 target/ralph/testall.log` | exit=0 (audits only) |
| PREPUSH | `./scripts/pre-push.sh > target/ralph/prepush.log 2>&1; echo exit=$?; tail -20 target/ralph/prepush.log` | exit=0 (audits only) |

Never print a whole log into the session; grep it.

## 6. Stopping

- **`ralph/NEEDS_HUMAN.md`** — a decision package, not a question:
  (a) the unit id and its row; (b) the exact commands you ran and their ACTUAL
  output, trimmed to what matters; (c) what the operator must decide or check,
  numbered, with file:line; (d) how to acknowledge — "edit or mark the row in
  ralph/STATE.md, then `rm ralph/STOP ralph/NEEDS_HUMAN.md`". Leave the tree
  compiling: `git checkout -- <paths>` undoes a partial edit. Commit nothing
  broken. Then stop.
- **`ralph/DONE`** — only when every row in `ralph/STATE.md` is `[x]`.

## 7. Hard rules

- Never `git push`, never `--no-verify`, never rewrite history.
- No `Co-Authored-By` line and no assistant name in any commit.
- Never run `--update-baseline` on a ratchet. Never add an `[[exception]]` row
  or widen an `except` list unless the row names that exact row.
- Never build `--release`. Never run bare `cargo build`/`test`/`check`/`clippy`
  — only the §5 commands, which take the cargo lock.
- Never edit `sovereign/ARCH_PRINCIPLES.md`, `AGENTS.md`, `.claude/`,
  `scripts/ralph-*.sh` or `quality/baselines/`. Never stop or restart the
  daemon.
- When a move changes a path that `quality/DAEMON_CORE.md`,
  `sovereign/SERVING_BOUNDARY.md` or `corpus-engine/DECOMPOSITION.md` names,
  fix that line in the same commit.
