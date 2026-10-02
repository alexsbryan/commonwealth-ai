<!-- ledger -->

**five-programs-58 · 2026-09-25 · fp-88 (fp-112 minted before it) · director** — this commit
- Needed: fp-88's flip, which is built and saved at ctl/fp-88-flip.patch, turns two `EmbeddedDaemon` tests red. `local_only_boot.rs:278` gets 503 where it expects 200 on `/v1/models`, and `storage_snapshot_e2e.rs:146` sees no events. Both boot with no cw-rails, and neither can be pointed at a door, because the daemon's cw-rails address is a hardcoded constant (state/node.rs:148, daemon.rs:3122) that no config key or env flag names.
- Chose: the package's option A. A new row, fp-112, declares `[daemon] rails_base` (`Option<String>`, absent means today's default) and adds one resolver that both NodeSeed and the ring-rail construction call. fp-88 depends on it. Its two tests set the key to a stand-in door and watch it with their assertions verbatim, and bootstrap's one `RailsKv` reads the same resolved value. The package's question 2 is settled by §12 decision 2: a daemon with no cw-rails answering 503 is the intended named absence, and no test asserts it away.
- Because: option B moves the snapshot test to AppState level, where it loses the `mesh_sharing` filter that lives in daemon.rs's start path, so it pins less. Option C lands red, which the standing all-green rule forbids. A config key rather than an env var because it is per daemon (so in-process tests cannot race on it) and `NodeSeed::resolved` already reads `[daemon]`. The change is additive, with no default and no route changed.

<!-- appendix -->

## five-programs-58 · 2026-09-25 — the daemon's cw-rails address becomes a declared `[daemon] rails_base` (fp-112) so fp-88's tests watch a door

<details><summary>reasoning, evidence, package</summary>

Package: ctl/NEEDS_HUMAN.resolved-fp88c-20260925.md. Reproduced at 5948a4aa0. `git apply --check` of ctl/fp-88-flip.patch succeeds. With the patch applied, in the toolbox, `sovereign-test.sh --package sovereign-daemon --filter local_only_boot` reads pass 3, fail 1: `a_local_only_daemon_spawns_no_network_service`, left 503, right 200. `--filter first_tick_emits_only_mesh_shared_corpora_to_ledger` reads fail 1, with "observed events: []". The tree was then restored clean. `ss -ltn` shows nothing on :9747.

- The address has no source. `git grep 'DEFAULT_RAILS_BASE\|rails_base'` finds the constant at rails_client.rs:38, NodeSeed's hardcode at node.rs:148, and a second direct use at daemon.rs:3122 (`RailsRingRail::new`, built before `NodeSeed::resolved` at :3248). quality/env-flags.toml names no rails variable. `DaemonSection` (setup_config.rs:1175) has no such field. The ring-sync tests already point `rails_base` at a fixture, but only at AppState level (ring_sync_by_roster.rs:152). No `EmbeddedDaemon` can.
- The 503 is the rows' stated behaviour. §12 decision 2 says "with the serving process absent the route reports absence ('a daemon alone serves no model')". fp-78's row says "a cache that has never filled reports a named absence, never an empty map". So local_only_boot's 200 must come from a door that serves the models read, not from a changed assertion.
- The charter's size rule is met. fp-112 adds one optional config field and one resolver, removes one duplicated constant use, and changes no behaviour. Nothing smaller lets the two tests watch a door.

Disclosed gap, not closed here: the other `EmbeddedDaemon` tests (about 60 construction sites under sovereign-daemon/tests/main) keep the default base. After fp-88 they dial 127.0.0.1:9747 for KV and ledger calls, and would write into a developer's live cw-rails if one listens. They pass today only because nothing does. It needs an owner and a closing condition (a hermetic test default, or each test pointing at a refused port), and that is scope the charter's size rule keeps out of fp-88. REVIEW-AFTER: the operator names the owner row or accepts the hazard.

What would falsify this: fp-112 turns out not to be behaviour-preserving (an existing config.toml fails to parse, or a default dial moves); or, with fp-112 landed, either of the two tests still cannot reach 200 / one snapshot without changing an assertion beyond the reader's handle; or the operator rules that a lone daemon should list its own resident models, which would reverse §12 decision 2's reading for `/v1/models` and mean a new row.

</details>
