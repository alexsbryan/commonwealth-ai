<!-- ledger -->

**phase-c-11 · 2026-10-02 · pc-cli-config-load-silent-default · worker** — this commit
- Needed: the row's census found the incident's config fails `SetupConfig::load` by validation (`validate_class`: neither `[models]` nor a `[node]` entry), not by parse, and that the row's fix as written touches 11 crates against a three-crate lift. A first session stopped at census with both forks (validation vs parse-only refusal; split or authorize); the package never reached the director.
- Chose: refuse on ANY load error, validation included, which is the row's letter ("exists but does not load refuses"). Built the half inside the lift: `SetupConfig::load_present` in contracts and every cli-mesh dial (the incident's verbs) on it, 16efaf015 and 0f61bc951. Left the `client_daemon_base`/`internal_daemon_base` half (35 callers, 9 crates) to the director as a split, and did not write the lane's `.done`.
- Because: refusing is the conservative reading. A config that fails validation also fails the daemon's own load, so no daemon can be running on the port it names, and dialling it would reach nothing; refusing names the file instead. Parse-only refusal would mean a second loader that skips validation, a second answer to "does this config load" (principle 8). The scope guard stops at more than twice the lift, so the 9-crate half is the director's to split or authorize. REVIEW-AFTER: a sandbox that wants mesh verbs now has to write a `[models]` or `[node]` stanza.

<!-- appendix -->

## phase-c-11 · 2026-10-02 — mesh verbs refuse a config that does not load, validation included; the client_daemon_base half waits for a split

<details><summary>reasoning, evidence, package</summary>

Census at 5a9f821df: `[daemon] client_port = 19751` parses (every section is serde-default) and `validate_class` (setup_config.rs:1853) refuses it. `load_from` runs that check on every load. Before the fix, the sandbox's `mesh status` printed the operator's live Meshsonics roster and exited 0, because it dialled cw-rails' default :9747 and not 9741 as the row says.

The proof is tests/config_load_refuses.rs. With the old fallback planted back into `dial_config` (`.ok().flatten()`), TEST(sovereign-cli-mesh) went red: 193 pass, 2 fail, `["mesh", "status"] exited 0`. The planted `mesh status` read the live mesh again. Reverted, it is green at 195/0.

Remaining for the director: `client_daemon_base()` (setup_config.rs:1549) and `internal_daemon_base()` (:1491) still turn a load error into the default port. Their callers are `git grep -n "client_daemon_base()"` minus comments: corpus-index 2, cli-base 4, cli-bench 9, cli-dev 2, cli-llm 5, core 1, enrichment-catalog 1, eval 1, pipeline 2, plus internal_daemon_base's 4. Several of them are String-returning clap default fns, which have to become run-time resolution.

What would falsify this: the operator ruling that a sandbox config with only a `[daemon]` section is meant to dial what it names. That would need a dial-only loader, and principle 8 makes it an operator call.

</details>
