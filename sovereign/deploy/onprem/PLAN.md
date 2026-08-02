# On-prem legal pilot — deployment plan

**Status:** planning · workstream 1 landed · 2026-08-01
**Audience:** us, then a law firm's IT team (the `README.md` this plan produces is the IT-facing artifact)
**Why this exists:** business stakeholders say yes and IT says "we don't have time." Every decision below is
optimised for *IT minutes spent*, not for feature surface.

---

## Bottom line

Four workstreams, ~3 days, producing one `tar.zst` that an IT team installs in under an hour reading a single
page. The pilot's differentiator is not "local RAG" — it is that the box demonstrably **refuses to answer what
it cannot source**, and the installer proves that on their hardware before a single lawyer logs in.

Deliberate v1 cuts, stated up front so the pilot is not mistaken for the product: **no SSO** (static bearer
tokens), **no ACLs** (one shared tenant — everyone sees every document *and* every conversation), **no mesh**,
**no desktop app**.

---

## Architecture

```
lawyers ──TLS──▶ nginx :443            route allowlist, bearer auth, access log
                    │ loopback
                    ▼
              sovereign-server :8080     grounded RAG: retrieval → grounding gate
                    │                    → citations. Built --no-default-features.
                    │ loopback           One tenant, N static keys.
                    ▼
              sovereign-cli-daemon :9741 owns the GGUFs. Weights paid ONCE.
                    :9742 → 127.0.0.1    (internal port: no auth, ever)
```

Three non-obvious choices, each load-bearing:

1. **`sovereign-server` is the front door, not the daemon.** The daemon's `/v1/chat/completions` is a raw
   OpenAI passthrough onto the llama.cpp slot — no retrieval, no grounding gate, no citations
   (`commonwealth-api/src/routes_inference.rs:26`). The grounded runtime (`sovereign_core::runtime::Runtime`)
   is constructed in exactly three places: the desktop app, `svrn chat`, and `sovereign-server/src/main.rs:248`.
   Only the last is headless and multi-user. An acceptance test that POSTs the daemon and expects abstention
   *cannot pass*.

2. **`sovereign-server` delegates inference to the daemon.** Both binaries load GGUFs in-process by default, so
   running both naively doubles VRAM. A single `[[inference.backends]] type = "remote"` pointed at
   `127.0.0.1:9741` pays for the weights once. `sovereign-core/src/mobile_host.rs:301-322` already generates
   this exact config shape — reuse, not invention.

3. **The reverse proxy must carry the authentication.** Both servers decide trust by *peer address*: the
   daemon admits every loopback caller with no token (`commonwealth-api/src/client_auth.rs:16-23`), and
   `sovereign-server`'s MCP routes gate on `ip.is_loopback()` (`routes_mcp.rs:127`). A same-host reverse proxy
   satisfies both on behalf of *every* remote caller. This is why dangerous routes are compiled out rather than
   merely firewalled.

---

## Workstream 1 — harden `sovereign-server` ✅ LANDED

`dev-routes` cargo feature, **default ON** so every existing build (desktop, mobile host, dev workstation) is
unchanged. `--no-default-features` removes:

| Removed | Why it cannot ship to a firm |
|---|---|
| `POST /v1/solve`, `POST /v1/cycle/bdd` | Client-supplied `test_command` reaches `sh -c` (`commonwealth-tdd/src/shared/test_runner.rs:44-52`), inside the *authenticated* router (`main.rs:768`). Any tenant key is a shell. Runs unconditionally as the baseline, before any model call. |
| `POST /v1/documents/upload`, `POST /v1/corpora/upload` | Ingest an **absolute server-side path** (`routes_documents.rs:130`, `corpus_upload.rs:48`). Any tenant can ingest any file the process can read — including the config holding every other tenant's API key — into their own queryable corpus. |
| `/mcp`, `/mcp/message`, `/mcp/stats` | Outside the auth layer entirely (`main.rs:807`), gated only by a loopback peer check that a reverse proxy defeats. Also carries `TddState`, so it is a second path to the same shell. |
| `ShellTool` | Registered on the agent runtime unconditionally (`main.rs:381`). Its approval grant is cached store-wide, and a pending approval blocks a scheduler permit with no timeout. |

None of this is sloppiness: the crate is documented as *the phone's host for one operator*
(`ARCHITECTURE_TOUR.md:54`, `SYSTEM_OVERVIEW.md:1717`), where the operator and the developer are the same
person. Every gap follows from that assumption. It simply is not the assumption a law firm runs under.

**Verified:** both feature configurations compile clean at the same 2 pre-existing warnings
(`handle_tools_list`, `TenantRuntime::handle_message` — neither introduced here). Under
`--no-default-features` the dead-code count drops 47 → 2, confirming the modules are genuinely excluded from
the binary rather than merely unreachable.

**Remaining:** `DEFAULTS_LEDGER.md` row + commit.

---

## Workstream 2 — headless OCR (~half day)

**Status: not started.** For a litigation practice, scanned PDFs are not an edge case — they are the corpus.
Without OCR the pilot silently ingests their discovery as empty documents, which is worse than refusing,
because nothing tells the user their scans produced nothing.

The blocker was never architectural — it was one function nobody calls. `OcrCtx` is plain data (`PathBuf`s, a
`String`, a `u32`, an enum) living in `sovereign-tools` (`local_corpus/ocr/mod.rs:59-100`); `set_ocr_ctx` is a
per-instance method (`manager.rs:373`); and the daemon **already builds that manager**
(`daemon_cmd/bootstrap.rs:1769`). The code anticipates a non-desktop caller: `watched/worker.rs:439-443` notes
the context is re-read per sweep "so a runtime install via `set_ocr_ctx` takes effect on the very next sweep
without a daemon restart."

| Step | Where | Size |
|---|---|---|
| Feature `ocr = ["sovereign-tools/paddle-ocr"]`, **off by default** | `sovereign-cli-daemon/Cargo.toml` | 3 lines |
| `daemon_cmd/ocr_install.rs` — env-first asset resolution, replacing the desktop's three `AppHandle` bundle probes | new file | ~70 lines |
| One call in the `Ok(manager)` arm | `bootstrap.rs:1779` | 1 line |

The two non-obvious `OcrCtx` fields — `daemon_base_url` and `cleanup_model` — are already computed in that
scope (`bootstrap.rs:1794-1797`).

**Engine: PaddleOCR, not tesseract.** Tesseract looks cheaper (unconditionally compiled, zero Cargo changes)
but its install story is `apt install tesseract-ocr`, which is precisely what an air-gapped box cannot do.
Paddle's dependencies are *files* that go in the tarball: `det.onnx` + `rec.onnx` + `dict.txt` at 12.6 MB, and
`libpdfium.so` at 7.6 MB, both with working `x86_64-unknown-linux-gnu` fetch paths in
`scripts/fetch-desktop-binaries.sh:58,140`. Paddle is also ~3× more accurate on the in-repo bake-off (CER
0.0212 vs 0.0652), and on documents where a misread digit is a wrong damages figure that difference is the
argument.

Already proven headless in-tree: `sovereign-tools/examples/ocr_images.rs`.

**Two traps to design around, not discover:**
- `paddle::models_root()` defaults to a hardcoded `~/.sovereign/models/paddle-ocr` and is **not**
  rebrand-aware, so on a `~/.svrnmesh` install the default path misses. Set
  `SOVEREIGN_PADDLE_OCR_MODEL_DIR` explicitly.
- `cleanup_model` must be a GGUF **file stem**, never a slot alias like `"fast"` (`ocr/mod.rs:76-82`). A wrong
  value degrades to raw un-polished OCR text with a `<!-- raw OCR (cleanup unavailable) -->` marker rather than
  failing — the kind of silent quality loss nobody reports.

Off-by-default because it pulls `ndarray`/`imageproc`/`i_overlay` into every dev build. `package.sh` turns it
on. **This makes it a `DEFAULTS_LEDGER.md` row.**

---

## Workstream 3 — the kit (~1 day)

Lands in this directory.

| File | What it is |
|---|---|
| `README.md` | The IT brief. Security posture in one page, install runbook, acceptance, backup/restore, honest limits, v2 roadmap. |
| `EGRESS.md` | Every outbound call in the tree with its kill switch. The artifact that wins the security review. |
| `daemon-config.toml` | `~/.svrnmesh/config.toml` |
| `server-config.toml` | `sovereign-server`'s `--config` (a *different* schema — see appendix) |
| `systemd/*.service` | Two **system** units with `User=`, `NoNewPrivileges`, `ProtectSystem=strict`. The repo only ships a `--user` unit (`sovereign/contrib/systemd/svrnmesh.service`). |
| `nginx/firm-rag.conf` | TLS + allowlist of exactly the client routes; everything else 404s. |
| `install.sh` | Untar, stage models + OCR assets, restore the `us-code` snapshot, write both configs, enable units. |
| `acceptance.sh` | The checks below, non-zero exit on any. |
| `package.sh` | Runs on **our** side — see below. |

`package.sh` is where the air-gap is actually solved: build four binaries (incl. `sovereign-server
--no-default-features` and the daemon `--features ocr`), fetch GGUFs + OCR assets, build `us-code` and
`svrn corpus snapshot publish` it, emit one archive + sha256. The firm's box never contacts HuggingFace, so
`svrn setup` is never invoked and `install.sh` writes both configs by hand.

**Corpora:** `us-code` prebuilt (no credentials, ~0.5 GB indexed); firm files post-install via
`svrn corpus watch` on a mounted share. Not shipping `scotus-opinions` / `olc-opinions` (CourtListener API
token on a paid tier) or `crs_reports` (5 GB) in v1.

---

## Workstream 4 — acceptance (~half day)

Six scripted checks, non-zero exit on any:

| # | Check | Assertion |
|---|---|---|
| 0 | **Security** | `POST /v1/solve` → 404, `/mcp` → 404. Proves the *hardened* binary is the one installed. |
| 1 | **Slots** | `/status` on :9741 — `inference.resident[]` has `primary`/`fast`/`embed` all `resident:true, transitioning:false`. |
| 2 | **Corpus** | `GET /v1/corpora` on :8080 lists `us-code`. |
| 3 | **Grounded answer** | Golden question → `citations[]` non-empty with real `corpus_id` + `chunk_id`. |
| 4 | **Abstention** | `epistemic_state.verdict == "cannot_know_from_here"`. |
| 5 | **OCR** | Ingest a scanned PDF → non-empty extracted text. |

**Check 1:** assert on `resident`, not `loaded_models`. The latter is plan-derived and joins on the registered
*model name*, not the slot role (`routes_status.rs:70-83`).

**Check 4 is the one worth the whole exercise, and it has two traps.** The raw `grounding_gate.action` is *not*
projected onto the wire (`projection.rs:112-135`), so `epistemic_state` is the only structured handle. And the
probe question must be **in-domain but absent** — an out-of-domain question triggers `gk_rescue`
(`knowledge_query.rs:1659-1683`), which replaces the abstention with a caveated parametric answer and rewrites
the action to `gk_rescue_released`, making the check flap.

---

## Risk register — what IT will find

| Risk | Disposition |
|---|---|
| `GET /v1/conversations` and `POST /v1/search` filter by tenant *after* the SQL `LIMIT` (`routes.rs:343` → `sqlite/conversation.rs:128-136`) | Unreachable with one shared tenant. **Hard blocker for practice group #2** — a busy colleague's afternoon would make your own conversation list render empty. |
| `POST /v1/tasks/{id}/approve` takes no tenant (`routes.rs:375`) | Same — moot at one tenant, blocker beyond. |
| ~10 concurrent users serialize on one model slot (`model_slot.rs:1315`) and one SQLite connection (`sqlite.rs:42`). REST callers get **no** queue signal; only WebSocket emits `QueuePosition`. | Size `max_concurrent_turns` to the box; document the ceiling. Real, disclosed. |
| A pending approval blocks a scheduler permit with **no timeout**, and a REST caller can never answer it (`approval.rs:207`) | `ShellTool` removal takes out the main trigger. Residual risk disclosed. |
| No `.doc` / `.msg` / `.pst` / `.xlsx`. Supported: pdf, txt, md, html, mhtml, epub, docx (`extract_stage.rs:133-151`) | Disclosed. `.msg`/`.pst` is the likely top v2 ask for litigation. |
| `svrn corpus ingest` is **not recursive** (`sovereign-workflow/src/model.rs:329`) | Use `svrn corpus watch` (walkdir-based) for the mounted share. Baked into `install.sh`. |
| `SetupConfig` has no `deny_unknown_fields` — a typo'd key is silently ignored and dropped on next save | `install.sh` writes configs; acceptance re-reads and asserts the values that matter. |
| Zero tests on `sovereign-server`'s `routes.rs` / `ws.rs` / `auth.rs` / `tenant.rs`; no release job; no contract journey | `acceptance.sh` is the compensating control, run on *their* box at install time. |
| Ethical walls | One shared tenant is incompatible with a conflicts screen. Stated plainly as a pilot constraint: a matter under a screen must not be ingested. |
| First query after snapshot restore can block on a synchronous atlas parse (~38 s measured on a 1.6 M-atom atlas, `docs/specs/ATLAS_STORAGE.md:25-33`); a pre-warm was built, measured as a regression, and reverted | `us-code` ships enrichment-disabled, so not hit in v1. Watch if enrichment is ever enabled. |

---

## Sequencing

1. Commit workstream 1 + `DEFAULTS_LEDGER.md` row.
2. OCR wiring, tested against a real scanned PDF.
3. Kit files.
4. `package.sh` dry run, then a **real install rehearsal into a clean VM**. The brief cannot claim "under an
   hour" on an untested path.
5. Full gates: `sovereign-lint.sh --human --full`, `sovereign-test.sh --human`, plus
   `cargo check -p sovereign-server --no-default-features` — the workspace run only exercises default features.

## Open questions

- **Target box specs.** Concurrency sizing and model profile are both downstream of VRAM. Absent an answer,
  assume 24 GB and the `very_high` profile (Qwen3.5-35B-A3B-Q4_K_M, 20.5 GB).
- **The golden question and the abstention probe**, from their actual practice area. Check 4 needs an
  in-domain-but-absent fact that can be *certified* absent, in the style of the chaos-monkey banks
  (`sovereign-eval/src/chaos_monkey/question.rs:179-236`).
- **Is a clean VM available for step 4?** If not, "under an hour" ships as an estimate with the assumption
  stated, which is materially weaker.

---

## Appendix — verified ground truth

Expensive to re-derive; all confirmed against the tree on 2026-08-01.

### Binaries
The release set is three: `sovereign-cli`, `sovereign-cli-daemon`, `sovereign-cli-llm`
(`scripts/release-cli-local.sh:50`). **`sovereign-server` is not in it** — `package.sh` must build it.
`docs/INTEGRATION_SURFACES.md:87` says so explicitly.

### Two config files, different schemas — do not mix keys
- Daemon: `~/.svrnmesh/config.toml`, struct `SetupConfig` (`sovereign-contracts/src/setup_config.rs:32`).
  Falls back to a *populated* `~/.sovereign/config.toml`.
- Server: arbitrary path via `--config` (**required, no default**), struct `ServerConfig`
  (`sovereign-server/src/config.rs:9`).
- The project file `.sovereign/sovereign.toml` is watcher config and has nothing to do with deployment.

Key names that are commonly guessed wrong: it is `context_size`, **not** `n_ctx`. There is **no** `host` key
(`client_bind` + `client_port`). There is **no** `[logging]` section. There is **no** `n_gpu_layers` under
`[models]` — GPU offload for the embedded engine is `SOVEREIGN_GPU_LAYERS` only.

### Ports
| Port | Process | Default bind | Auth |
|---|---|---|---|
| 9741 | daemon, client | `127.0.0.1` (`setup_config.rs:971`) | bearer, but **loopback callers always admitted with no token** |
| 9742 | daemon, internal | **`0.0.0.0` unconditionally** on a plaintext mesh (`sovereign-mesh/src/daemon.rs:37-56`) | **none at all** (`commonwealth-api/src/server.rs:456`) |
| 8080 | sovereign-server | `127.0.0.1` (`config.rs:232`) | `[auth] mode`, default `"none"` |

`[daemon] internal_bind = "127.0.0.1"` is the single most load-bearing line in the whole config. :9742 serves
`/internal/v1/models/file/{name}` (raw GGUF bytes), corpus mutation, and model load/unload.

### Zero-egress posture
Three keys, then the box makes no outbound connections:
```toml
[daemon] freshness_watchers_enabled = false   # else a Wikipedia MediaWiki poller +
                                              # Wikimedia SSE spawn AT STARTUP (daemon.rs:2928)
[iroh]   enabled = false                      # no relay, no n0 DNS/pkarr
[discovery] mdns = false                      # multicast bind is otherwise FATAL at boot
```
Also set `[knowledge_view] enabled = false` on the server — it defaults **true** and background-ingests every
conversation into corpora. And `[iroh] enabled = false` on the server too; `mobile_host` defaults it *on*,
which tunnels the local HTTP port to the public internet via third-party relays.

There is **no telemetry anywhere in the tree**, no update check, and no HuggingFace reachability probe in the
daemon boot path. That claim is defensible line-by-line and is the centrepiece of `EGRESS.md`.

### Cold boot without the setup wizard
Config load failure, GGUF load failure, and data-dir creation failure are **fatal**. A missing `[models]` path
is caught by the VRAM preflight, which labels the slot `UNREADABLE` and refuses with a repair hint
(`daemon_cmd/build/preflight.rs:128-136`). A *corrupt but present* GGUF is not caught there — it dies at slot
load with `error: failed to load models` (`build/inference.rs:99-144`). The daemon refuses to start; it never
starts degraded. `validate_gguf` is **not** in the boot path — only in download/setup flows.

`sovereign.db` need not pre-exist: migrations are unversioned idempotent DDL replayed on every open
(`sovereign-store/src/sqlite.rs:73-101`).

VRAM gate is **advisory** by default — overcommit warns and starts. It refuses only under
`SOVEREIGN_STRICT_VRAM_CHECK=1`. `SOVEREIGN_SKIP_VRAM_CHECK=1` bypasses everything including the
unreadable-file refusal, so do **not** set it in an air-gapped install.

### Corpus snapshots
`svrn corpus snapshot publish <id>` / `restore --archive <path> --as <id> --into <dir>` is the supported
relocation path (`corpus-engine/src/snapshot.rs`). Restore **hard-errors** on an embedding-dimension mismatch,
so the box must run the same embed model that built the snapshot (Qwen3-Embedding-0.6B, 1024 dims, on every
profile).

Two things the archive does **not** carry: `<data_dir>/local-corpora/<id>.json` (watched-folder registration —
a restored watched corpus is unregistered until re-registered) and the RAPTOR tree rows, which live in
`sovereign.db`'s `conv_raptor_nodes`, not in the index directory. Neither affects `us-code`, which ships
enrichment-disabled.

### Grounding gate
On by default and **env-only** — there is no TOML knob (`runtime/grounding/config.rs:44-48`, default τ = 0.9).
The gate **fails open** when the judge is unavailable (`action = judge_failed_open`), by design: "the gate is a
quality lever, not an availability risk." It runs only when retrieval returned something — zero-chunk turns
have no gate metadata at all and instead take a separate retrieval-miss path
(`handlers/knowledge_query.rs:165-225`, `metadata.retrieval_missed = true`).

### Useful CLI facts
- Foreground daemon for `Type=simple` is `svrn daemon run` — `start` detaches and is wrong for systemd.
- No `--port` flag; port comes only from config. `--config` is space-separated (`--config=x` is not parsed).
- Readiness probe everywhere is `GET /v1/models` returning 2xx. There is no dedicated ready command.
- `svrn corpus list` prints the **static built-in catalog**; `svrn corpus status` prints what is **installed**.
  Neither has a `--format json`.
- `svrn doctor --json` (not `--format json`); exit 1 on any Failed check.
- Exit 102 = RSS hard-limit self-SIGTERM; exit 104 = client listener lost.
