# CLI Documentation-Conformance Audit — 2026-06-21

One-time reconciliation of every CLI command/flag **claimed** in documentation against the **actual** dispatch surface (the hand-written `match` ladders in the four binaries). This pass is now superseded by the automated harness (`docs/cli-contract.toml` + the `cli_contract_*` conformance tests) — this file records the state at the time the harness was introduced and the disposition of each finding.

## Method

- **Docs scanned (claims):** `CLI_REFERENCE.md` (canonical — 24 `### sovereign <verb>` headers), `README.md`, `RUNBOOK.md`, `ATOS.md`, `CODE_INTELLIGENCE.md`, `KNOWLEDGE_BASES.md`, `GETTING_STARTED.md`, `TROUBLESHOOTING.md`, `DEVELOPMENT.md`, `FEATURES.md`, `.claude/CLAUDE.md`, `sovereign-recipes/GETTING_STARTED.md`.
- **Code scanned (reality):** dispatcher `sovereign-cli/src/main.rs` + siblings `sovereign-cli-dev`/`-llm`/`-daemon` `main.rs` + every `*_cmd` module's dispatch arms. Roughly **50 top-level verbs / 200+ subcommands**; CLI_REFERENCE documents **24 verbs / ~90 commands**.
- No daemon required (static read of `match` arms + `--help` semantics).

## Headline

The docs do **not substantially lie** — every documented command and flag spot-checked against the high-traffic verbs (`atlas`, `eval`, `recipe`, `mcp`, `chat`, `enrich`, `atos`, `daemon`, `setup`, `doctor`, `corpus`) exists. The drift is overwhelmingly **omission** (real surface ≫ documented surface, ~55% of commands undocumented) plus **one reachability gap**. That shape is exactly what the harness is built to hold the line on going forward.

## Visibility model (maintainer steer, 2026-06-21)

Not everything in `sovereign --help` is public. Per maintainer direction, the **public** surface is what the top-level READMEs promise — **local inference + mesh + knowledge bases**: `setup`, `chat`, `mesh`, `corpus`, `doctor`, `install-service`, `daemon`. Everything else (code intelligence, ATOS, enrichment, benches, gyms, governance, atlas, drift, archaeology, voice, …) is **internal / developer tooling**. The manifest encodes this as a `visibility = public | internal` axis, orthogonal to the `feature` build-gate; the harness holds the public surface to the strictest bar (must work in the shipped build + be named in the README).

### D3 — code intelligence promoted as public but gated out of the shipped binary (RESOLVED in docs)

The shipped CLI is built without `--features dev-tools` (`.github/workflows/cli-release.yml:4-6,138`), so `project` / `code` / `tools` — all in `DEV_VERBS` — hit the intercept and **exit 2** for public users. Yet the README ("For your code → `sovereign project init`") and `CODE_INTELLIGENCE.md` promoted them as public. Resolution (maintainer decision — *code intel is internal/advanced, fix the docs*): the README's code-intel section is reframed as a developer build, `CODE_INTELLIGENCE.md` carries a "developer build required" note, and `project`/`code`/`tools` are tagged `visibility = internal`. The public code-intelligence path is the MCP server on `:9741`, which the shipped daemon serves. The manifest invariant `visibility=public && feature=dev-tools → error` guards this from regressing.

### Internal tooling in the public `--help` (recommendation, not yet acted on)

The default-build `HELP` (`sovereign-cli/src/main.rs:178`) advertises internal tooling — `govern`, `search-gym`, `knowledge-gym`, `mobile`, etc. — to public users. Consider gating these out of the public `--help` (or grouping them under a "developer" heading) so the advertised surface stays inference + mesh + knowledge bases. Not changed in this pass (a `HELP`/`DEV_VERBS` code edit); flagged for decision.

## Bucket 1 — Dangerous (fix / escalate)

| # | Finding | Evidence | Disposition |
|---|---|---|---|
| D1 | **RESOLVED 2026-07-28 — both are reachable now.** `proxy` and `portfolio` were added to the LLM cluster arm (`sovereign-cli/src/main.rs:934-936`); verified live (`sovereign portfolio --help` exits 0). The finding below is kept for provenance. A *separate* rough edge remains and is now ledgered in `cli-contract.toml`'s `[[stranded]]` block: `proxy --help` exits 2, because `proxy_cmd/mod.rs:29` matches `--help` as a subcommand name. Original finding: **`proxy` and `portfolio` are unreachable via `sovereign`.** Both are implemented and dispatched by the LLM sibling (`sovereign-cli-llm/src/main.rs:92-93` → `proxy_cmd::run_proxy`, `portfolio_cmd::run_portfolio`) but the top-level dispatcher (`sovereign-cli/src/main.rs`) has **no match arm** for them, so `sovereign proxy ask …` / `sovereign portfolio …` fall through to `print_usage()` and exit 1. | dispatcher match arms 569-744 contain no `proxy`/`portfolio`; sibling dispatches them; no doc references either verb. | **REPORT + propose fix.** Shipped features (session notes: `proxy ask` AC-4, `portfolio` CLI) are dead from the `sovereign` entrypoint. The fix is ~2 lines — add `proxy`/`portfolio` to the llm-cluster arm. Not auto-applied: dispatcher is a hot file, daemon is mid-test, and intent should be confirmed. |
| D2 | **No documented command found to be non-existent, and no documented flag found missing**, across spot-checks of the most-used documented verbs. | `atlas wikipedia/budget/status`, `eval run`, `recipe test/validate/list/publish`, `mcp list/test/tools`, `chat --show-reasoning`, `reflect --retire`, `corpus reconstruct-manifest --source-dir` all present. | The **code-conformance gate (harness Phase 2)** proves the *entire* documented set exhaustively and continuously — this audit only spot-checked. |

## Bucket 2 — Coverage gaps

### 2a. Public verbs in `sovereign --help` but absent from CLI_REFERENCE.md — **FIX (add entries)**

`HELP` (`sovereign-cli/src/main.rs:178`) advertises these to every user, yet CLI_REFERENCE has no section:

- `govern` (`seed` / `tensions` / `resolve` / `accept` / `ask`)
- `search-gym`
- `knowledge-gym`
- `mobile` (`serve` / `status` / `pair`)

### 2b. Secondary/internal verbs that dispatch but are undocumented — **REPORT**

`voice`, `newsworthy`, `reading-diag`, `meta-atlas`, `meshapp`, `router-cache`, `recipe-agent`, `maintainer`, `memory` (`list`/`expand`), `stop`, `claim`, `nudge` (`dismiss`), `awareness` (feature-gated).

### 2c. Undocumented subcommands under documented verbs — **REPORT**

| Verb | Documented | Undocumented (live) |
|---|---|---|
| `corpus` | list, install, remove, status, reconstruct-manifest (5) | diag, dedupe, repair, merge-partitions, pull, migrate-to-partition, catalog, extract-entities, scrub, snapshot, watch, watch-list, watch-status, watch-pause, watch-resume, watch-confirm-deletion, watch-sync-now, watch-add-root, watch-remove-root, watch-remove, stream-axes, export-parcels (22) |
| `atlas` | wikipedia, budget, status | list-corpora, list-atoms, show-atom, migrate-ids, build-doc-index, enable-incremental, typed-extension, stats |
| `code` | index, watch, mcp-status, search | finalize, brief, reflect |
| `enrich` | init, build, query, report, review, bridge, seed, extract, cluster, name, resolve, tensions, gaps, configure, status, show, exemplars, reset | delta, delta-manifest, ingest, raptor, raptor-index, triage, atlas-eval, eval, eval-median, atlas-drift-report, sep-ingest, classify, extract-typed, sheets-ingest, reconcile, tensions-classify, investigation, diagnose, errors |
| `mesh` | create, join, rotate, status, balance, leave, logs, fetch-model, warm-cache | check-invariants, soak-gate |
| `pipeline` | run, status, list, pod up, pod list, pod down | pause, pod pool |
| `atos` | provision, next, start-milestone, end-milestone, archive, status, promote, diff, run-ab, probe-driver, report, teardown, feature, spec, doctor, install-plugin | run, replay |
| `notes` | (flags only) | add, promote, migrate-from |
| `plan` | (flags only) | validate |

(`project register/unregister/list/watch` are documented — CLI_REFERENCE.md:48-51 — and correctly excluded above.)

## Bucket 3 — Intentional surface (record as `hidden`/`alias_of` in the manifest)

- **`enrich` legacy v1** (`cluster-questions`, `name-concerns`, `cluster-chunks`, `extract-positions`, `detect-tensions`, `detect-gaps`, `cascade`, `legacy-query`, `validate`, `promote`, `diff`) — already documented *as legacy* (CLI_REFERENCE.md:356-358), hidden from `--help`. → `hidden = true`.
- **`enrich` alias spellings** (`triage`|`triage-candidates`, `query`|`atlas-query`, `report`|`schema-report`, `review`|`schema-review`, `cluster`|`cluster-atlas`, `name`|`name-atlas-clusters`, `resolve`|`atlas-resolve`, `reconcile`|`atlas-reconcile`, `tensions`|`atlas-tensions`, `gaps`|`atlas-gaps`, `configure`|`atlas-configuration`, `bridge`|`atlas-cross-corpus`, `tensions-classify`|`atlas-tensions-classify`) → `alias_of` canonical.
- **`atos`/`project` hidden delegator arms** in `sovereign-cli-dev` (`atos-status-promote`, `atos-status-report`, `atos-teardown`, `atos-spec-accept`, `atos-spec-diff`, `atos-milestone-end`, `project-status`/`-charter`/`-amend`/`-design`/`-plan`/`-init`/`-refresh`/`-phase-pass`/`-serve`/`-audit`/`-daemon-is-running`, `drift-detect`, `audit-recover`) — internal exec targets the flat verbs delegate into; not user-facing. → `hidden = true`.
- **`__dump-commands` / `__contract-smoke`** (added by this work) — hidden introspection arms.

## Probe-strategy notes (for the manifest `probe` field)

- Most verbs honor `--help` (call `help::wants_help` at the top → exit 0), including daemon-backed ones (`chat ask --help` exits 0 with no daemon). → `probe = "help"`.
- The ATOS leaf delegators (`atos spec diff`, `atos end-milestone`) treat `--help` as a positional and exit 2. → `probe = "no-args"` (assert dispatch reached the handler, ignore exit code), matching the existing tactic in `sovereign-cli/tests/aliases.rs:242`.

## Disposition summary

- **Fixed in this change (docs):** reframed the README code-intelligence section + `CODE_INTELLIGENCE.md` as a developer build (D3); added CLI_REFERENCE.md sections for `govern`, `search-gym`, `knowledge-gym`, `mobile` (documented in the full reference, tagged `visibility=internal`); added a dedicated `install-service` section.
- **Encoded in the manifest:** `docs/cli-contract.toml` tags the public surface (`visibility=public`: setup, chat, mesh, corpus, doctor, install-service, daemon) vs internal tooling, with the `public ⇒ not dev-tools` invariant.
- **Reported for triage (not changed):** the `proxy`/`portfolio` routing gap (D1, with proposed 2-line fix), internal tooling in the public `--help`, and the internal undocumented surface (2b, 2c).
- **Locked in going forward:** the `cli_contract_docs` test (manifest ↔ CLI_REFERENCE/README, written) and the `cli_contract_code` test (manifest ↔ binary, Phase 2) turn any future divergence into a CI failure, so this audit never has to be done by hand again.
