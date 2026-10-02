<!-- ledger -->

**phase-b-53 · 2026-09-29 · pb-ingest-dial-daemon · director** — this commit
- Needed: pb-ingest-dial-daemon stopped at its census with no code written. Two files that name the engine sat outside the row, and one of them made a dependency cycle with pb-ingest-rehome. The package also put two design forks: what a standalone svrn holds where boot takes an ingest port today, and where the engine's startup chores run.
- Chose:
  - routes_edit_predictions.rs:49: add pb-meshapp-rest to this row's `depends`. That row is ready (its three depends are `[x]`), and it moves the whole file out of the daemon.
  - The recipe_project_http.rs cycle: package option 2(a). This row takes the recipe parse and offline validation as ONE `IngestPort` method, `validate_recipe_toml`, beside `recipe_corpus_id`. pb-ingest-rehome drops `corpus_engine::Recipe` from its pre-flight and keeps the store and harness half.
  - Standalone svrn: package option 3(a). The boot consumers take `Option`, and `None` withholds the subsystem by name. No `IngestAbsent` implementor.
  - Startup chores. Fingerprint stamping runs where the stock binary composes the engine. The package's fork 4 is corrected for the geometry gate: that gate lives on the leaf reader (`FsIndexSource`), so the daemon arms the reader it holds from its embed probe, and no path may serve retrieval with the gate unarmed.
- Because:
  - Extend, never re-own (§2c). Supplying the grammar lookup from the distribution would add a second supplier of something pb-meshapp-rest deletes. The validation method extends the port that already parses recipe TOML (`recipe_corpus_id`, corpus-index ingest_port/daemon.rs:363). No existing port method validates TOML text: `dry_run_recipe` and `test_recipe_report` take a path and run the harness.
  - Principle 6. An absent-ingest implementor would have to answer the port's non-`Result` methods (for example `corpus_is_installed -> bool`) with success-shaped defaults. With `Option`, absence stays visible.
  - Principle 10 on the geometry gate. The package would have moved `set_expected_embedding_dimensions` to the compose closure, which runs before the probe measures the width (boot.rs:536-548), so the gate would never have been armed.
  - Boundary gate: 20 violations, EXIT=1 at fca107922 (`cargo xtask boundary-gate`, corpus-engine/). This commit changes no Rust.

<!-- appendix -->

## phase-b-53 · 2026-09-29 — pb-ingest-dial-daemon waits on pb-meshapp-rest, takes recipe validation onto IngestPort, threads Option for a standalone svrn

<details><summary>reasoning, evidence, package</summary>

Reproduced at fca107922:
- `routes_edit_predictions.rs:49` `corpus_engine::extractors::code::language_for_extension`. pb-meshapp-rest (STATE.md:1023) moves routes_edit_predictions.rs whole, and its depends pb-code-server, pb-code-daemon-exit and pb-stock-binary are `[x]`.
- `recipe_project_http.rs:36` (`use corpus_engine::Recipe`) and `:617` (`validate_recipe_offline`), and `Recipe::from_toml` at :615 and :647. Outside corpus-engine's own tests, `validate_recipe_offline` has this one caller.
- tests/main/engine_census.rs:31-40 `OWNED_ELSEWHERE` exempts both files. pb-ingest-rehome (STATE.md:821) `depends [pb-ingest-dial-daemon, ...]`, so the cycle is real.
- `sovereign-daemon/src/bin/sovereign-daemon.rs:32` runs `process::run(&raw_args, None, None, None)`. hosted_ingest.rs:7 already says that svrn alone "reports ingest absent by name".
- `FsIndexSource::set_expected_embedding_dimensions` is at corpus-index fs_source.rs:131, and `CorpusEngine::set_expected_embedding_dimensions` forwards to `self.source` (engine/mod.rs:694-695). boot.rs:547 arms it from `advertise_embed.info()` after the probe.

Not checked: the package's list of boot consumers, one by one. A consumer this row finds that already tolerates absence needs no `Option`. A consumer the list missed gets the same treatment.

No FIVE_PROGRAMS edit. The rulings apply §2c and the existing "svrn alone reports ingest absent" contract, and they move no boundary.

What would falsify this:
- pb-meshapp-rest turns out not to be landable ahead of this row, for example because its PROOF needs something this row produces. Then the grammar lookup needs another home.
- A recipe-project route needs more of `Recipe` than the parse and the verdict. Then 2(b), reordering the rows, is back on the table.
- The stock binary cannot hand the probe's width to the composed engine's reads without a new port method. Then that method is what fork 4 becomes.

</details>
