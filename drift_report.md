# Drift Report — 9 actionable · 0 confirmed · 0 queued

**Code**: `drift-target-self-atlas`  ·  **Narrative**: `drift-target-system-overview`, `drift-target-arch-principles`

## Act on

**1. normative claim _(anchor `../docs/THREAT_MODEL.md` not in atlas)_ — ...the sole residual plaintext on an encrypted mesh.** _(drift-target-system-overview sec_00023)_  
> ...the sole residual plaintext on an encrypted mesh.

_Next step:_ Search the codebase for `../docs/THREAT_MODEL.md`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**2. normative claim _(anchor `TrustStore` not in atlas)_ — The historical per-session-cert/TrustStore mTLS scaffolding was removed on 2026-06-15 and :9742 should never be describe…** _(drift-target-system-overview sec_00027)_  
> No per-request auth: ... The historical per-session-cert/`TrustStore` mTLS scaffolding...

_Next step:_ Search the codebase for `TrustStore`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**3. normative claim _(anchor `admin_http::tests::loopback_guard_works_under_production_listener_shape` not in atlas)_ — Production listener shape tests verify the loopback guard works correctly in admin_http::tests::loopback_guard_works_und…** _(drift-target-system-overview sec_00027)_  
> ...pinned listener-shape test (`admin_http::tests::loopback_guard_works_under_prodution_listener_shape`). The listener must use `.into_make_service_with_connect_info::<SocketAddr>()`

_Next step:_ Search the codebase for `admin_http::tests::loopback_guard_works_under_production_listener_shape`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**4. normative claim _(anchor `qwen3-embedding-0.6b` not in atlas)_ — The default embedding model is `qwen3-embedding-0.6b` (1024 dims, the canonical `corpus_engine::DEFAULT_EMBED_DIM`).** _(drift-target-system-overview sec_00029)_  
> Default embedding model: `qwen3-embedding-EmbedDim`. The Embed slot is a cross-peer interoperability contract — nodes sharing a corpus must produce bit-compatible vectors (`EmbedModelInfo` must match).

_Next step:_ Search the codebase for `qwen3-embedding-0.6b`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**5. normative claim _(anchor `worker_eligibility::EligibilityConfig::anchor` not in atlas)_ — Anchors get the stricter worker_eligibility::EligibilityConfig::anchor profile (settle 300s, quarantine on first flap).** _(drift-target-system-overview sec_00040)_  
> `discover_rpc_workers` filters candidates to `can_anchor` so a casual peer never joins the spit, and anchors get the stricter `worker_eligibility::EligibilityConfig::anchor` profile (settle 30s, quarantine on first flip).

_Next step:_ Search the codebase for `worker_eligibility::EligibilityConfig::anchor`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**6. normative claim _(anchor `proxy_from_env` not in atlas)_ — proxy_from_env` is always on.** _(drift-target-system-overview sec_00047)_  
> `proxy_from_env` is always on.

_Next step:_ Search the codebase for `proxy_from_env`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**7. normative claim _(anchor `File paths` not in atlas)_ — File paths, tool counts, enum variants, CLI subcommands, HTTP routes — all are assertions, all must verify.** _(drift-target-arch-principles sec_00003)_  
> File paths, tool counts, enum variations, CLI subcommands, and HTTP routes are all assertions that must be verified.

_Next step:_ Search the codebase for `File paths`; if present, the structural atlas is missing this symbol (separate fix). If absent, the principle is stale — revise or implement.

**8. normative claim _(no anchor)_ — Files over 1200 lines must be split.** _(drift-target-arch-principles sec_00031)_  
> > 1200 Split. No exceptions that aren't already documented in §12 of SYSTEM_OVERVIEW.md.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

**9. normative claim _(no anchor)_ — The KnowledgeView feature promises that the three-map corpora never leave the user's machine.** _(drift-target-arch-principles sec_00046)_  
> The KnowledgeView feature promises that thethree-map corporanever leave theuser'smachine.

_Next step:_ Locate the implementation of this rule; either anchor it via rustdoc on the relevant code or revise the principle if implementation has shifted.

## Provenance & Evolution (12199 of 12324 atoms enriched)

_Repo `/home/alexbryan/dev/commonwealth-ai` · 1050 co-evolution pairs · 12199 fresh / 0 moved · renames not followed in v1._

**Stability highlights** _(load-bearing — held longest unchanged)_

- `sovereign/crates/sovereign-cli/src/main.rs` · 92 days · 83 commits · alexsbryan@gmail.com, alexbryan01@gmail.com, 6320088+alexsbryan@users.noreply.github.com
- `sovereign/crates/sovereign-core/src/context.rs` · 92 days · 29 commits · alexsbryan@gmail.com, alexbryan01@gmail.com, 6320088+alexsbryan@users.noreply.github.com
- `sovereign/crates/sovereign-core/src/router.rs` · 92 days · 69 commits · alexsbryan@gmail.com, alexbryan01@gmail.com, 6320088+alexsbryan@users.noreply.github.com
- `sovereign/crates/sovereign-core/src/runtime.rs` · 92 days · 158 commits · alexsbryan@gmail.com, alexbryan01@gmail.com, 6320088+alexsbryan@users.noreply.github.com
- `sovereign/crates/sovereign-core/tests/core_tests.rs` · 92 days · 43 commits · alexsbryan@gmail.com, alexbryan01@gmail.com, 6320088+alexsbryan@users.noreply.github.com

**Recent volatility** _(currently active surfaces)_

- `commonwealth/crates/commonwealth-daemon/src/main.rs` · last touched 2026-07-02 by alexsbryan@gmail.com — "desktop qa iterations + SYSTEM_OVERVIEW updates"
- `corpus-engine/xtask/src/main.rs` · last touched 2026-07-02 by alexsbryan@gmail.com — "open source readiness"
- `commonwealth/crates/commonwealth-api/src/routes_knowledge.rs` · last touched 2026-07-02 by 6320088+alexsbryan@users.noreply.github.com — "Saas (#13)"
- `commonwealth/crates/commonwealth-api/src/routes_oicp.rs` · last touched 2026-07-02 by 6320088+alexsbryan@users.noreply.github.com — "Saas (#13)"
- `commonwealth/crates/commonwealth-api/src/routes_ollama.rs` · last touched 2026-07-02 by alexsbryan@gmail.com — "open source readiness"

**Co-evolution clusters** _(implicit coupling)_

- `commonwealth/crates/commonwealth-core/src/capabilities.rs` ↔ `commonwealth/crates/commonwealth-discovery/src/gossip_service.rs` · 100% (9 of 9)
- `sovereign/crates/sovereign-tools/src/atlas_view/atom_detail.rs` ↔ `sovereign/crates/sovereign-tools/src/atlas_view/stable_key.rs` · 100% (8 of 8)
- `corpus-engine/src/enrichment/atlas/atoms_delta.rs` ↔ `corpus-engine/src/enrichment/atlas/doc_to_atoms.rs` · 100% (6 of 6)
- `corpus-engine/src/extractors/csv.rs` ↔ `corpus-engine/src/extractors/plaintext.rs` · 100% (6 of 6)
- `sovereign/.sovereign/notes.db-shm` ↔ `sovereign/.sovereign/notes.db-wal` · 100% (6 of 6)

---
_Per-finding detail in the JSON sidecar._
