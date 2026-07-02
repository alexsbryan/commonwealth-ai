# The svrnmesh rename (in progress)

> **Status: 2026-06-29, UNCOMMITTED (working tree on `main`).** The user-facing brand
> is being renamed **Sovereign → svrnmesh** and the CLI command **`sovereign` → `svrn`**
> for copyright reasons. This is a deliberately **partial, back-compat-preserving**
> migration — both old and new names are live *on purpose*. **Do not "consolidate" the
> duplication; read the rules below first.** Full detail: `~/.claude/plans/silly-leaping-ripple.md`.

## Name map — what changed vs what stayed

| Surface | State | Notes |
|---|---|---|
| Product / app display name | **svrnmesh** | Tauri `productName`/title/tray, all UI copy |
| CLI command users type | **`svrn`** | symlink → the `sovereign-cli` binary; transitional `sovereign` alias still installed (one release) |
| Rust crates / packages | **`sovereign-*` (unchanged)** | `sovereign-core`, `sovereign-cli`, … are internal — NOT renamed |
| Binaries on disk | **`sovereign-cli{,-dev,-daemon,-llm}` (unchanged)** | only the *typed command* (`svrn`) changed, via symlink |
| Home data dir | `~/.sovereign` → **`~/.svrnmesh`** | migrated at runtime; fallback + symlink during transition |
| macOS app-support dir | `…/sovereign` → **`…/svrnmesh`** | same migration path |
| Env var prefix | `SOVEREIGN_*` → **`SVRNMESH_*`** | both work (bidirectional shim); most read sites still say `SOVEREIGN_` |
| macOS / mobile bundle id | **`com.svrnmesh.desktop`** / **`ai.commonwealth.svrnmesh.mobile`** | one-way: severs auto-update from old installs |
| Deep-link scheme | **both** `svrnmesh://` and `sovereign://` registered | old invite links still open |
| launchd / systemd | **`com.svrnmesh.daemon`** / **`svrnmesh.service`** | old registration auto-removed on install |
| MCP server name | **`svrnmesh`** | update your `.mcp.json` key after the daemon restart |
| State-DB filename | `sovereign.db` (**unchanged — deferred**) | helper exists, unwired |

## Why both names coexist (the back-compat layer)

The keystone is **`sovereign-core/src/rebrand.rs`**. It makes correctness independent of how
far the rename has progressed — a site that still says `sovereign` keeps working:

- **`svrnmesh_root()` / `mesh_data_dir()`** — prefer `~/.svrnmesh`, fall back to a *populated*
  `~/.sovereign`. `sovereign-cli-shared/src/dirs.rs` duplicates this (no dep on core);
  `setup_config::default_data_dir` delegates to it.
- **`promote_legacy_env()`** — called from each binary's `main()` *before* the tokio runtime;
  mirrors `SOVEREIGN_*`↔`SVRNMESH_*` both directions when the target is unset. `svrnmesh_env(suffix)`
  is the read-side complement (checks `SVRNMESH_<suffix>` then `SOVEREIGN_<suffix>`).
- **`run_startup_migration()`** — atomic `fs::rename` of the data dirs `~/.sovereign`→`~/.svrnmesh`
  (+ a back-compat symlink), **gated on `!daemon_is_live()`** (TCP probe of the API port). The
  **daemon is the migration authority**: it runs this at startup before binding the port; CLI
  processes defer to it and ride the fallback getters until the next clean daemon start.

**Transitional, dropped in a later release:** the `sovereign` command symlink, the
`~/.sovereign → ~/.svrnmesh` symlink, the `sovereign://` deep-link scheme, and the
`.sovereignignore` read-both fallback.

## Rules when you touch this code

- **Resolve paths via the getters** (`rebrand::svrnmesh_root()`, `dirs::sovereign_root()`,
  `dirs::sovereign_indexes()`) — never hardcode `~/.svrnmesh` *or* `~/.sovereign`. A test that
  hardcoded `.sovereign` broke because the cache had already populated `.svrnmesh`.
- **New env reads** → `rebrand::svrnmesh_env("X")`. New vars should be named `SVRNMESH_*`.
- **CLI help/usage strings** → write `svrn` (the command). Keep `cargo build -p sovereign-cli`
  (package names) and `sovereign-cli*` (binary names) exactly as-is.
- **Do NOT blanket-`sed` `sovereign`→`svrn`.** The token means different things:
  command = `svrn`; product/UI (capital `Sovereign`) = `svrnmesh`; crate/binary = `sovereign-*` (keep);
  `~/.sovereign` = a path that *migrates* (don't text-replace it); `sovereign://` / `sovereign-server`
  = functional identifiers handled case-by-case.
- **`CLI_REFERENCE.md`** command headers use `` ### `svrn <verb>` `` — the `cli_contract_docs`
  test enforces binary-manifest ↔ doc alignment (its parse MARK is `` `svrn ``).
- **Desktop copy lives in THREE places** — sweep all when rebranding UI text, not just the
  frontend: (1) the Svelte frontend `sovereign-desktop/src/`; (2) the **Tauri Rust side**
  `src-tauri/src/` (tray menu + tooltip, conversation export md/PDF, import error messages,
  the "svrnmesh Models" label, crash/setup-report headers); (3) `index.html` — the window
  `<title>`. A frontend-only sweep leaves the title bar, tray, and exports stale. Note a
  built/running app shows NONE of these until it's rebuilt (`npm run build && cargo tauri build`,
  or restart `cargo tauri dev`).
- **Grep case-INSENSITIVELY for UI copy.** The brand renders ALL-CAPS `SOVEREIGN` in several
  spots (`App.svelte` loading `<h1>`, empty-chat `<h2>`, `AssistantMessage`'s `◈` speaker mark,
  `ConversationList`'s `.brand-name`) → now `SVRNMESH` — a mixed-case `s/Sovereign/.../` sed
  silently skips them. Also sweep lowercase displayed copy (`<code>sovereign daemon …</code>` →
  `svrn …`, the `sovereign-answer.md` export filename, `~/.sovereign` paths shown in Settings)
  while PRESERVING the functional forms (`sovereign://`, `@sovereign/chat-ui`, `sovereign-local`,
  `sovereign:inner_work:`, and the `sovereign/*` Obsidian tag namespace — that last is a data
  format, deferred like the per-project dir).

## Gotchas (these cost real time)

- **Stale siblings.** `cargo test` / `sovereign-test.sh` rebuilds the `sovereign-cli` *dispatcher*
  but NOT the sibling bins it `exec`s (`sovereign-cli-dev`, etc.). The `aliases`/`phase6` tests run
  the real siblings and compare against STALE `sovereign`-era banners. **Run `cargo build --bins`
  first** (the "rebuild the sibling" rule, biting via the test harness).
- **MCP key.** After the daemon is rebuilt + restarted, the MCP server reports `svrnmesh`. Update
  `.mcp.json` key `sovereign`→`svrnmesh`; tools become `mcp__svrnmesh__*` / `svrn tools call …`.
- **Migration timing.** The `~/.sovereign`→`~/.svrnmesh` move only happens on a *clean* daemon start
  (nothing on the API port). While a daemon is live, CLIs defer and the fallback getters keep
  everything working. (Verified safe: on a machine with a live daemon, migration correctly defers
  and `~/.sovereign` is untouched.)

## Deferred (NOT done — follow-ups; all back-compat-covered)

- **Per-project `.sovereign/` dir** — the ATOS/spec/drift on-disk contract (~50+ refs across
  `sovereign-cli-dev`, `sovereign-tools`, `sovereign-mesh` + the FS-watcher's default ignore
  component). (`.sovereignignore`→`.svrnmeshignore` *is* done, read-both.)
- **Bulk env read-site rename** `SOVEREIGN_*`→`SVRNMESH_*` (~190 sites; the shim already bridges both).
- **State-DB filename** `sovereign.db`→`svrnmesh.db` (`rebrand::state_db_path()` exists, unwired;
  the file lives inside the already-renamed dir).
- **Doc tail** — design docs, specs, and `.claude/CLAUDE.md` command/MCP references (~400 cosmetic
  refs; commands work via the `sovereign` alias meanwhile).

## Verification (2026-06-29)

Workspace lint 17/17 (0 errors) · full `cargo test` 7164/0 · desktop svelte-check + vitest 204/204.
