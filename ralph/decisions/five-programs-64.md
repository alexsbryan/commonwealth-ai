<!-- ledger -->

**five-programs-64 · 2026-09-25 · REVIEW-mint-fp-rails-solo · director** — this commit
- Needed: the mint row said cw-rails' `ensure` starts it "under sovereign-contracts' run_lock", but `quality/ARCH_LAYERS.toml:676-679` forbids commonwealth-rails → sovereign-* (the lift boundary), so cw-rails cannot link `RunLock`. The row also named "the existing sibling resolution", which is six private `locate()` copies in binary crates that neither sovereign-daemon nor cli-llm can call.
- Chose: option (A). cw-rails holds its own lock on its own data root for the lifetime of `run`, with std `File::try_lock` and no new dependency, mirrored and documented on both sides like `DEFAULT_RAILS_BASE`. The six locators collapse into one `locate_sibling(bin, env_var)` in sovereign-contracts. Five rows minted, fp-solo-a..e, which is the cap. Solo idle-exit is not minted.
- Because: principle 12. Each program owns the singleton of its own data root; the daemon's run_lock and cw-rails' lock guard different roots, so no one thing has two deciders. (B) widens the lift closure, which is the operator's call. (C) leaves two cw-rails on one root unguarded. Lifting the locator is the reuse the row asked for under principles 8 and 11, and it deletes five copies (five-programs-54).

<!-- appendix -->

## five-programs-64 · 2026-09-25 — cw-rails guards its own root; one sibling locator; fp-solo-a..e minted

<details><summary>reasoning, evidence, package</summary>

Package: `ctl/NEEDS_HUMAN.resolved-fprailssolo-20260925.md`. Reproduced at dc654cd31:

- `quality/ARCH_LAYERS.toml:676-679`: `[[forbid]] from = "commonwealth-rails" to = "sovereign-*"`, reason being the lift by `scripts/cw-rails-lift.sh`.
- `commonwealth-rails/Cargo.toml` carries no `libc`. The toolchain is 1.95.0 (`rust-toolchain.toml`), so std `File::try_lock` (stable since 1.89) serves, and sovereign-core/src/deep_research/state.rs:226 already uses it. Option (A) therefore adds zero crates to the lift closure.
- `Refusal::NoMesh` has one producer, lib.rs:299, and one reader, cli.rs:194.
- `fn locate() -> Option<PathBuf>` appears in sovereign-cli/src/{daemon,mesh,llm,agent_bench,dev}_bin.rs and sovereign-cli-daemon/src/daemon_bin.rs:17. sovereign-daemon does not depend on sovereign-cli-shared, the only other crate carrying `which`, and sovereign-contracts is the crate both sovereign-daemon and cli-llm already name.
- cli-llm's portfolio and newsworthy dial through `sovereign_daemon::rails_client::resolve_rails_base` (legacy_store.rs:28), so one `ensure_rails` in rails_client serves both programs' clients.

Idle-exit is left out because the source row says solo mode "may" idle-exit, and the charter's size rule is strictly necessary. A detached cw-rails that stays up is what the meshed mode already does.

Falsified if cw-rails' lock and the daemon's run_lock turn out to need to agree on one path (for example, if both are ever pointed at one data root), because that would make them one decider in two copies. Also falsified if `locate_sibling` cannot preserve any of the six call sites' current behaviour without a per-site branch.

</details>
