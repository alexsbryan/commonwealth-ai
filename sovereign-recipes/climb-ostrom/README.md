# climb-ostrom — the prompt-climb v0 fixture (tight-loop mechanism proof)

Pre-registration: `research/prompt-climb/PRE-REG-obsidian-2026-09-09.md`
in the monorepo. Read it first — bars, arms, and the wall ledger live
there. This directory is everything a second machine needs.

Contents:

- `recipe.toml` — scratch corpus `climb-ostrom` (one essay, tiered→
  philosophy domain; the enrichment config pins `philosophy_atlas`).
- `Ostrom Summary.md` — the essay (copied from the author's vault;
  license private, mesh_sharing false — do not republish).
- `climb-ostrom-golden.toml` — the obsidian golden's Ostrom-essay
  entries, copied verbatim per the pre-reg.
- `prompt-workdir/` — the solve WORKDIR TEMPLATE: the philosophy_atlas
  prompt overlay + `check.sh` (the `counts:` checker). Baseline with
  pristine prompts: 3p/1f — `FAIL person_atoms.Garrett Hardin`.

## Pickup on a fresh machine

1. Build the daemon with the current engine (the counts: contract,
   instrument profile, and artifact default-target fixes must all be
   in): `cargo build --bins --features sovereign-cli/dev-tools`, then
   `sovereign daemon restart`.
2. Materialize the workdir OUTSIDE the monorepo (solve wants its own
   git root): `cp -r sovereign-recipes/climb-ostrom/prompt-workdir
   ~/climb-workdir && cd ~/climb-workdir && git init && git add -A &&
   git commit -m init`.
3. Install the scratch corpus: `sovereign corpus install
   sovereign-recipes/climb-ostrom/recipe.toml`.
4. Watch the gate fail: `sh ~/climb-workdir/check.sh` → expect 3p/1f.
5. Verify the instrument BOTH ways before trusting any climb data
   (garbage a phase1* prompt → the run must change; restore).
6. Solve with `test_command: counts: ./check.sh` — but read the
   pre-reg's WALL 7 order first: the flip is blocked on that engine
   knot, not on the fixture.

## Known-good cadence

Reset→re-init→build→eval ≈ 1.5–2 min per candidate; K=3 serial ≈ 5–6
min per round. If a round exceeds ~10 min, something is caching or the
daemon is contended — check before interpreting ties.
