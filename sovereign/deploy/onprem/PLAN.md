# On-prem legal pilot — deployment plan

**Status:** workstreams 1-4 landed · 2026-08-03 · **not yet rehearsed on a clean VM**
**Audience:** us, then a law firm's IT team (the `README.md` this plan produces is the IT-facing artifact)
**Why this exists:** business stakeholders say yes and IT says "we don't have time." Every decision below is
optimised for *IT minutes spent*, not for feature surface.

> **2026-08-03 — the plan's central egress claim was wrong, and is now fixed in code.**
> The appendix asserted that three config keys give a zero-egress box and that the claim "is defensible
> line-by-line". A line-by-line audit refuted it. Three agent tools — the `search` tool's web fallback
> (DuckDuckGo → Google → DuckDuckGo Lite), `web_fetch` (**any** URL the model emits, scheme-only
> validation), and `wikipedia_fetch` — were registered **unconditionally** on the agent runtime, fired on
> **ordinary chat turns**, were governed by no config key or env var, and were **not** removed by
> `--no-default-features`. They sit three lines below `ShellTool`, which *is* gated, which is why this was
> easy to miss. `Permission::Network` is not a control: it is consulted at exactly one call site, in the
> plan executor, and the chat path calls `tool.execute()` directly.
>
> Fixed by a `net-tools` cargo feature (default ON, so no other build changes) — see workstream 1 below.
> Under `--no-default-features`, `web_fetch` and `wikipedia_fetch` are not registered at all and `search`
> is constructed local-only. `acceptance.sh` check 0c asserts this against the running box, and both
> systemd units carry `IPAddressDeny=any` so the kernel is the backstop rather than this analysis.
>
> Three other appendix claims were also checked and are corrected in place below and in `EGRESS.md`:
> `[iroh] enabled = false` does **not** close the mesh-join relay path (only `discovery = "none"` does);
> the Wikimedia SSE stream is dead code with no caller; and `[daemon] internal_bind`'s `0.0.0.0` default
> is not *unconditional* (an encrypted mesh forces loopback).

---

## Bottom line

Four workstreams producing one `tar.zst` that an IT team installs in under an hour reading a single page.
The pilot's differentiator is not "local RAG" — it is that the box demonstrably **refuses to answer what it
cannot source**, and the installer proves that on their hardware before a single lawyer logs in.

**Status 2026-08-03:** all four landed and gated. One thing has never been executed: `package.sh` and
`install.sh` end-to-end on a clean VM, which is also the only way to test OCR against a real scan. Until
that rehearsal runs, "under an hour" is an estimate, not a measurement, and `README.md` should not go to a
firm.

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

**Two** cargo features, both **default ON** so every existing build (desktop, mobile host, dev workstation)
is unchanged. `--no-default-features` drops both.

They are deliberately separate flags. `dev-routes` gates developer surfaces whose risk is *privilege* (a
shell, an arbitrary file read). `net-tools` gates ordinary product features whose risk is *egress*. One flag
for two unrelated decisions would mean neither name is true.

### `net-tools` — added 2026-08-03, the audit finding

| Removed | Why it cannot ship to a firm |
|---|---|
| the `search` tool's **web fallback** | `POST html.duckduckgo.com`, then `www.google.com` when that bot-blocks, then `lite.duckduckgo.com`. Fires on any chat turn where the top **local** retrieval score is below `SCORE_SUFFICIENT` — i.e. precisely when the corpus is thin, which on a fresh pilot is often. The tool itself survives, built with `SearchTool::new` instead of `with_web`: corpus search **is** the product; reaching the open web was a separate capability sharing a tool id. |
| `web_fetch` | Retrieves **any** URL the model emits. Scheme-only validation, no host allowlist, follows 5 redirects. |
| `wikipedia_fetch` | `en.wikipedia.org`. Still registered on the *daemon's* MCP registry (no equivalent feature there); loopback-only reachable, and `IPAddressDeny` is its control. |

Verified two ways, plus a kernel backstop: `acceptance.sh` check 0c enumerates `GET /v1/tools` on the running
box and fails if either tool id is present *or* if `search` went missing with them; both systemd units set
`IPAddressDeny=any` with a loopback-only allow.

**`package.sh` deliberately does NOT grep the binary for the tool ids**, and the reason is worth recording
because the first version of that gate was wrong twice and both failures were silent. Measured on a real
`--no-default-features` build:

| Literal | substring matches | whole-line matches |
|---|---|---|
| `/v1/solve` | **0** — genuinely gone | 0 |
| `/mcp/stats` | **0** — genuinely gone | 0 |
| `/v1/conversations` (registered, control) | 1 | **0** |
| `web_fetch` | **7 — still present** | 0 |
| `wikipedia_fetch` | **2 — still present** | 0 |

Two lessons. First, `grep -qx` **never matches**: Rust packs string literals into one blob, so `strings`
emits them glued to neighbours and a whole-line match returns 0 whether the code is compiled in or not — the
original gate always passed and proved nothing. Second, the tool-id strings **survive** a correct hardened
build: they come from `sovereign-tools`, which stays linked, and `net-tools` gates the *registration*, not
the type. Grepping for them would have refused to package a correct kit.

What `package.sh` now does: substring-matches the **route** literals (sound — they live in this crate behind
`#[cfg]`), plus a **positive control** on `/v1/conversations`. If the control is also absent, `strings` read
nothing and the script says so rather than passing. A gate that cannot fail is not a gate.

### `dev-routes` — landed 2026-08-02

`--no-default-features` removes:

| Removed | Why it cannot ship to a firm |
|---|---|
| `POST /v1/solve`, `POST /v1/cycle/bdd` | Client-supplied `test_command` reaches `sh -c` (`commonwealth-tdd/src/shared/test_runner.rs:44-52`), inside the *authenticated* router (`main.rs:768`). Any tenant key is a shell. Runs unconditionally as the baseline, before any model call. |
| `POST /v1/documents/upload`, `POST /v1/corpora/upload` | Ingest an **absolute server-side path** (`routes_documents.rs:130`, `corpus_upload.rs:48`). Any tenant can ingest any file the process can read — including the config holding every other tenant's API key — into their own queryable corpus. |
| `/mcp`, `/mcp/message`, `/mcp/stats` | Outside the auth layer entirely (`main.rs:807`), gated only by a loopback peer check that a reverse proxy defeats. Also carries `TddState`, so it is a second path to the same shell. |
| `ShellTool` | Registered on the agent runtime unconditionally (`main.rs:381`). Its approval grant is cached store-wide, and a pending approval blocks a scheduler permit with no timeout. |

None of this is sloppiness: the crate is documented as *the phone's host for one operator*
(`ARCHITECTURE_TOUR.md:54`, `SYSTEM_OVERVIEW.md:1717`), where the operator and the developer are the same
person. Every gap follows from that assumption. It simply is not the assumption a law firm runs under.

**Verified:** both feature configurations compile clean. Under `--no-default-features` the dead-code count
drops 47 → 2, confirming the modules are genuinely excluded from the binary rather than merely unreachable.
(One follow-on: gating the upload route left `IngestProgress` imported but unused in the hardened build —
the import now follows the route, so a hardened build carries no warning that reads like rot.)

**Done:** `DEFAULTS_LEDGER.md` rows for both `dev-routes` and the daemon's `ocr` feature, with falsifiable
flip conditions and a 2026-09-15 review-by.

---

## Workstream 2 — headless OCR ✅ LANDED

**Correction to this section's premise.** It said OCR-less ingest "silently ingests their discovery as empty
documents, which is worse than refusing, because nothing tells the user their scans produced nothing." That
is not what the code does. `watched/worker.rs` puts every scanned PDF into
`WatchedFolderState.failed_files` with reason `scanned_no_text`, and it distinguishes the two cases in the
message: "turn on OCR to read it" vs. "OCR is enabled but the daemon's OcrCtx isn't installed". It is
reported, not silent — but only in `svrn corpus watch-status <id> --failures`, which nobody runs unprompted.
So the real gap is discoverability, not silence, and the case for OCR is unchanged: for a litigation
practice, scans **are** the corpus, and a reported failure still means the discovery never got indexed.
`README.md` names that command in the day-to-day section for exactly this reason.

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
- `paddle::models_root()` defaults to a hardcoded `~/.svrnmesh/models/paddle-ocr` and is **not**
  rebrand-aware, so on a `~/.svrnmesh` install the default path misses. Set
  `SOVEREIGN_PADDLE_OCR_MODEL_DIR` explicitly.
- `cleanup_model` must be a GGUF **file stem**, never a slot alias like `"fast"` (`ocr/mod.rs:76-82`). A wrong
  value degrades to raw un-polished OCR text with a `<!-- raw OCR (cleanup unavailable) -->` marker rather than
  failing — the kind of silent quality loss nobody reports.

Off-by-default because it pulls `ort`/`ndarray`/`imageproc`/`i_overlay` into every dev build. `package.sh`
turns it on. **`DEFAULTS_LEDGER.md` row landed**, with the flip condition "clean-build cost under 60 s **and**
the OCR assets ship in the standard release artifact" — a build with the code and no models fails at ingest,
which is worse than not having it.

**What actually landed**, against the sizing above:

| Step | Where | Actual |
|---|---|---|
| `ocr = ["sovereign-tools/paddle-ocr"]`, off by default | `sovereign-cli-daemon/Cargo.toml` | as planned |
| `daemon_cmd/ocr_install.rs` | new file | ~200 lines incl. 4 unit tests |
| One call in the `Ok(manager)` arm | `bootstrap.rs` | as planned |

Three departures from the sketch, each earning its keep:

- **The module compiles unconditionally, carrying both `cfg` arms.** The single call site in `bootstrap.rs`
  never grows a `#[cfg]`, and a build *without* `--features ocr` logs `ocr:unavailable
  reason=feature_not_compiled` at boot rather than doing nothing. An operator who turned OCR on in
  `corpus watch` and sees nothing happen must be able to find out why from the daemon log alone.
- **Probe order is `env → <data_dir> → ~/.svrnmesh`, and the env value is a parameter, not a read.**
  `data_dir` before the hardcoded home path is the fix for the rebrand trap named above; passing the env
  override in makes precedence a pure function of its inputs, testable without mutating process-global
  state — which under a parallel test runner is a race, not a test. Both orderings are pinned by tests.
- **`libpdfium` gets the same treatment and a `warn!` naming every path probed.** Missing models and missing
  pdfium fail differently: no models → the context is not installed and scans are reported
  `scanned_no_text`; no pdfium → the context installs, then no PDF can be rasterized and OCR yields nothing.
  The second is the quieter failure, so it gets the louder log. `install.sh` hard-fails on either.

The `cleanup_model` trap above is handled by passing the same chat-slot **file stem** the enrichment defaults
already use, and warning explicitly when it arrives empty.

---

## Workstream 3 — the kit ✅ LANDED

All in this directory. `acceptance-probes.env.template` is an addition to the table: the golden question,
the abstention probe and the OCR fixture are the plan's own open questions, and they must come from the
firm. Rather than inventing defaults, the kit ships the template and `acceptance.sh` reports those checks as
**could-not-judge** (exit 2) until it is filled in.

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

## Workstream 4 — acceptance ✅ LANDED

Twelve checks in eight groups (the six planned plus two the audit added), **four verdicts not two**:
`pass` / `FAIL` (exit 1) / `UNSURE` (exit 2) / never-ran. An install where two checks could not be *judged*
is an install with two unknowns; reporting that as green is the failure the script exists to prevent.

| # | Check | Assertion |
|---|---|---|
| 0 | **Security** | `POST /v1/solve` → 404, `/mcp` → 404. Proves the *hardened* binary is the one installed. |
| 0b | **Auth is on** | An unauthenticated `GET /v1/corpora` → 401. **Added:** `sovereign-server` enables auth only when `[auth] mode == "api_key"` **and** `keys` is non-empty; `mode = "api_key"` with an empty map does not fail, does not warn, and serves every route as tenant `"default"`. A loopback `bind` means the startup exposure guard does not fire either. Nothing else catches this. |
| 0c | **No egress tools** | `GET /v1/tools` contains neither `web_fetch` nor `wikipedia_fetch`, **and still contains `search`**. **Added:** the audit finding above. Asserting only the absence would pass a build where retrieval had been crippled too. |
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
the action to `gk_rescue_released`, making the check flap. Both `citations` and `epistemic_state` carry
`skip_serializing_if`, so an ungrounded answer **omits the key** rather than sending an empty value —
absence and emptiness are the same failure and the script catches both.

**Check 5 is two assertions, not one.** Negative: the fixture is not in the sweep's `failed_files` under
`scanned_no_text`. Positive: a known phrase comes back from search. The negative alone only proves nothing
complained. It searches the daemon-side corpus with `svrn corpus search`, **not** `POST /v1/search` — the
server's `[retrieval] corpora` allow-list does not contain a temporary acceptance corpus, so the server route
would return nothing and the check would fail for the wrong reason.

**Every check has been watched pass AND fail on a named input**, per §18.1 / §18.4. Six stub runs:

| Stub | Result |
|---|---|
| **dead** box (nothing listening) | all UNSURE, exit 2 — no check claims a verdict |
| **hardened** (dangerous routes 404, no-token 401) | 0 / 0b / 2 pass |
| **leaky** (dangerous routes answer 200) | check 0 FAILs, exit 1 |
| **correct turns** (grounded answer w/ citations; abstention w/ `cannot_know_from_here`) | 3 and 4 pass |
| **hallucinating** (verdict `general_knowledge`, `citations` key omitted) | check 4 FAILs, exit 1 |
| **leaked tools** (`/v1/tools` lists `web_fetch` + `wikipedia_fetch`) | check 0c FAILs, exit 1 |

The dead-box run is what found the bug this validation exists to catch: **`curl` writes the literal `000`,
not an empty string, when it never got an HTTP response.** Before the fix, a box where *nothing was running*
reported `POST /v1/solve is REACHABLE` — the most alarming possible false positive from a security check,
and precisely the "check with no failing input you can name" smell.

The request/response field names are verified against the source, not against the stub: `SendMessageRequest
{ content }` and `CreateConversationResponse { id }` (`routes.rs:21,36`).

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
| Zero tests on `sovereign-server`'s `routes.rs` / `ws.rs` / `auth.rs` / `tenant.rs`; no release job; no contract journey | `acceptance.sh` is the compensating control, run on *their* box at install time. Disclosed in `README.md` "Honest limits" rather than left for them to find. |
| **`[auth] mode = "api_key"` with an EMPTY `keys` map silently disables auth entirely** — every `/v1/*` route served as tenant `"default"`, no error, no warning. A loopback `bind` means the startup exposure guard does not fire either. | `install.sh` generates a key and writes it into `[auth.keys]` **by marker substitution, not append** (appending would land it under `[iroh]`, be dropped as an unknown key, and leave auth off), then re-reads the file to confirm it landed in the right table. `acceptance.sh` check 0b sends a no-token request and requires 401. Highest-severity item in this table. |
| The three internet-reaching agent tools (`search` web fallback, `web_fetch`, `wikipedia_fetch`) had no runtime switch and were not removed by `--no-default-features` | Closed by the `net-tools` feature. Verified by acceptance check 0c and `IPAddressDeny=any` in both units. |
| The daemon's MCP registry still carries `wikipedia_fetch`, with no cargo feature to remove it | Loopback-only reachable, and `sovereign-server` does not drive the daemon's MCP surface. `IPAddressDeny` is the control. Named explicitly in `EGRESS.md` rather than omitted. |
| Ethical walls | One shared tenant is incompatible with a conflicts screen. Stated plainly as a pilot constraint: a matter under a screen must not be ingested. |
| First query after snapshot restore can block on a synchronous atlas parse (~38 s measured on a 1.6 M-atom atlas, `docs/specs/ATLAS_STORAGE.md:25-33`); a pre-warm was built, measured as a regression, and reverted | `us-code` ships enrichment-disabled, so not hit in v1. Watch if enrichment is ever enabled. |

---

## Sequencing

1. ~~Commit workstream 1 + `DEFAULTS_LEDGER.md` row.~~ ✅ Ledger rows landed for `dev-routes` and `ocr`.
2. ~~OCR wiring~~ ✅ landed and unit-tested. **Not yet tested against a real scanned PDF** — that needs
   staged Paddle models and a Linux box, so it is folded into step 4.
3. ~~Kit files.~~ ✅ all eight, plus `acceptance-probes.env.template` and `nginx/firm-rag-proxy.conf`.
4. **REMAINING — `package.sh` dry run, then a real install rehearsal into a clean VM.** The brief cannot
   claim "under an hour" on an untested path. Five things have *never been executed*, all for the same
   reason (this work was done on macOS; the target is Linux):

   - `package.sh` end-to-end. Most fragile part: the snapshot-archive lookup globs two snapshot roots for a
     file newer than 10 minutes.
   - `install.sh` end-to-end.
   - acceptance check 5 against a real scan — needs staged Paddle models + `libpdfium`, which only
     `package.sh` produces.
   - **`systemd-analyze verify` on both units** — unavailable on macOS, so the unit files are unparsed. In
     particular `IPAddressDeny=any` / `IPAddressAllow=localhost` has never been exercised; confirm the
     daemon still reaches its own loopback ports under it.
   - **`nginx -t` on the route allowlist** — likewise unparsed. The regex `location` ordering (first match
     in file order, with `=` and `^~` beating regex) is the part most likely to be subtly wrong.

   Everything else in this plan is verified by a run. This step is the honest gap, and it is what stands
   between "written" and "works".
5. ~~Full gates.~~ ✅ `sovereign-lint.sh --human --full` **exit 0**, 0 errors, workspace scope. Plus the
   four builds the workspace run does **not** cover, because neither `--no-default-features` nor `ocr` is a
   default: `cargo check -p sovereign-server` with and without default features, and
   `cargo check -p sovereign-cli-daemon` with and without `--features ocr`. All four clean; zero warnings
   from the new/changed files in any of them.

   `sovereign-test.sh --human` reports **3 failures out of 9,023**, all in
   `sovereign-compute supervisor::tests` (`a_proven_healthy_generation_resets_the_breaker`,
   `brief_healthy_stretches_do_not_reset_the_breaker`,
   `backoff_is_floored_by_the_last_generation_s_load_cost`). **Pre-existing, not caused by this work** —
   established by stashing every change and re-running on a clean tree, which still fails 1 per run. They
   spawn real child processes and assert on wall-clock restart counts inside a 1200 ms drain window
   (`supervisor.rs:1246`), so they lose the race on a loaded machine; which of the three fails varies run to
   run. Not touched here: this work changed `sovereign-server`, `sovereign-cli-daemon`, docs and configs,
   none of which `sovereign-compute` depends on.

## Open questions

All three are still open. Two of them are now *encoded* rather than merely noted, so they cannot be
forgotten: they live in `acceptance-probes.env.template`, and `acceptance.sh` reports the affected checks as
could-not-judge (exit 2) until answered.

- **Target box specs.** Concurrency sizing and model profile are both downstream of VRAM. Absent an answer,
  the configs assume 24 GB and the `very_high` profile (Qwen3.5-35B-A3B-Q4_K_M, 20.5 GB), with
  `max_concurrent_turns = 4` and the GGUF filenames written in as placeholders. Both config files say so at
  the point of the assumption.
- **The golden question and the abstention probe**, from their actual practice area. Check 4 needs an
  in-domain-but-absent fact that can be *certified* absent, in the style of the chaos-monkey banks
  (`sovereign-eval/src/chaos_monkey/question.rs:179-236`). The template carries the certification recipe:
  `svrn corpus search us-code "<phrase>"` must return nothing relevant.
- **A scanned PDF from their own production** for check 5 — added to the same template. Ours says nothing
  about their scanner, their DPI, or their paper, and a born-digital PDF would pass the check without OCR
  ever running.
- **Is a clean VM available for step 4?** Still the sharpest one. `package.sh` and `install.sh` are written
  but have never been executed end-to-end. Until they have, "under an hour" is an estimate, and
  `README.md` should not be sent to a firm claiming otherwise.

---

## Appendix — verified ground truth

Expensive to re-derive; all confirmed against the tree on 2026-08-01.

### Binaries
The release set is three: `sovereign-cli`, `sovereign-cli-daemon`, `sovereign-cli-llm`
(`scripts/release-cli-local.sh:50`). **`sovereign-server` is not in it** — `package.sh` must build it.
`docs/INTEGRATION_SURFACES.md:87` says so explicitly.

### Two config files, different schemas — do not mix keys
- Daemon: `~/.svrnmesh/config.toml`, struct `SetupConfig` (`sovereign-contracts/src/setup_config.rs:32`).
  Falls back to a *populated* `~/.svrnmesh/config.toml`.
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
| 9742 | daemon, internal | `0.0.0.0` by default (`setup_config.rs:1043-1047`) — **not unconditionally**, see below | **none at all** (`commonwealth-api/src/server.rs:456`) |
| 8080 | sovereign-server | `127.0.0.1` (`config.rs:232`) | `[auth] mode` + **non-empty `keys`**, default off |

**Correction (2026-08-03):** the `0.0.0.0` on :9742 is the *config default*, not an unconditional bind.
`sovereign-mesh/src/daemon.rs:38-58` forces `127.0.0.1` whenever the mesh requires encryption, and an
unparseable `internal_bind` warns and falls back to `0.0.0.0`. This box runs no mesh, so the override never
fires and the config line is the only thing holding it — which is why `daemon-config.toml` calls it the most
load-bearing line in the file.

**Correction (2026-08-03):** `[auth] mode` on :8080 is necessary but not sufficient. Auth engages only when
`mode == "api_key"` **and** `keys` is non-empty; `mode = "api_key"` with an empty map serves every route
unauthenticated as tenant `"default"`, with no error and no warning. With a loopback `bind` the startup
exposure guard does not fire either. `acceptance.sh` check 0b is the only thing that catches it.

`[daemon] internal_bind = "127.0.0.1"` is the single most load-bearing line in the whole config. :9742 serves
`/internal/v1/models/file/{name}` (raw GGUF bytes), corpus mutation, and model load/unload.

### Zero-egress posture — **REWRITTEN 2026-08-03; three keys was wrong**

Config is necessary and was never sufficient. See the banner at the top of this file and `EGRESS.md` for the
full audit. Corrected summary:

```toml
[daemon] freshness_watchers_enabled = false   # else a Wikipedia MediaWiki poller spawns AT
                                              # STARTUP (daemon.rs:2917-2977)
[iroh]   enabled = false                      # necessary, NOT sufficient — see below
[iroh]   discovery = "none"                   # the key that actually closes the n0 path
[discovery] mdns = false                      # multicast bind is otherwise FATAL at boot
```

Plus, on the server: `[knowledge_view] enabled = false` (defaults **true**, including when the section is
omitted — it has a hand-written `Default` returning true), and `[iroh] enabled = false` / `discovery = "none"`.

Four corrections to what this section used to say:

- **`[iroh] enabled = false` does not remove all n0 traffic.** The mesh-join path (`join.rs:336-356`, called
  from `daemon.rs:1240-1279`) builds a relayed endpoint from `relay_urls` + `discovery` and **never consults
  `enabled`**. Only `discovery = "none"` closes it. That key is load-bearing independently. A second
  override: a mesh whose policy requires encryption turns iroh on despite an explicit `false`.
- **The Wikimedia SSE stream does not exist as a live path.** `newsworthy_event_stream::spawn()` has no
  caller in any binary — compiled-in dead code. The MediaWiki poller is real; the SSE half of the claim was
  not. (The poller is additionally double-gated: it needs a corpus-engine handle, and every tick returns
  early unless the `wikipedia-newsworthy` corpus is installed, which here it is not.)
- **`mobile_host` defaults iroh on — in the *generator*, not in this deployment.** `ServerConfig`'s own
  `[iroh] enabled` defaults to **false** (`config.rs:37-40`, a derived `Default`). The true operational rule
  is "never run `svrn mobile serve`, never use a mobile-host-generated server config", not "a live risk".
- **`[knowledge_view]` makes no network call.** Every file under the knowledge-view tree was searched for
  HTTP clients and URL literals; none exist. It is still switched off, for confidentiality on a
  single-tenant box, not for egress.

There is **no telemetry anywhere in the tree**, no update check reachable from either process, and no
HuggingFace reachability probe in the daemon boot path. Those three survived the audit intact. They are the
easy part of `EGRESS.md`; the three agent tools are the hard part, and honesty about having got that wrong
is what the document is worth.

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
