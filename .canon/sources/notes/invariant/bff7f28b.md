# SOVEREIGN-SERVER IS THE PHONE'S HOST, NOT A SHARED-DEPLOYMENT FRONT DOOR — and until 2026-08-01 its authenticated router contained a shell…

SOVEREIGN-SERVER IS THE PHONE'S HOST, NOT A SHARED-DEPLOYMENT FRONT DOOR — and until 2026-08-01 its authenticated router contained a shell (adversarial review for a law-firm on-prem pilot).

WHY IT MATTERS: sovereign-server is the ONLY headless surface that builds the grounded runtime. sovereign_core::runtime::Runtime is constructed in exactly three places — the desktop app, svrn chat (chat_cmd/bootstrap.rs:306), and sovereign-server/src/main.rs:248. The daemon's /v1/chat/completions is a raw OpenAI passthrough onto the llama.cpp slot (commonwealth-api/src/routes_inference.rs:26): no retrieval, no grounding gate, no citations. So any headless multi-user RAG product has to route through sovereign-server, which means this note applies to every such deployment.

WHAT WAS FOUND (all pre-existing, all a consequence of the one-operator assumption the crate was written under — ARCHITECTURE_TOUR.md:54, SYSTEM_OVERVIEW.md:1717):
1. POST /v1/solve + /v1/cycle/bdd sit INSIDE the authed router (main.rs:768 merges tdd_router before the auth layer at :769). Body carries client-supplied workdir + test_command; only guard is "not a system path, is a git repo" (commonwealth-tdd/src/workdir.rs:64-86); the string reaches `sh -c` (shared/test_runner.rs:44-52) unconditionally as the baseline, BEFORE any model call. Any tenant key = shell.
2. POST /v1/documents/upload (routes_documents.rs:130) and POST /v1/corpora/upload (corpus_upload.rs:48) take an ABSOLUTE SERVER-SIDE PATH. Any tenant can ingest the config file holding every other tenant's API key into their own queryable corpus.
3. /mcp is outside the auth layer (main.rs:807) and gated only by ip.is_loopback() (routes_mcp.rs:127) — WHICH A SAME-HOST REVERSE PROXY SATISFIES FOR EVERY REMOTE CALLER. It also carries TddState (routes_mcp.rs:140), so it is a second path to (1). This is the generalisable trap: both servers decide trust by PEER ADDRESS, and the daemon does the same (commonwealth-api/src/client_auth.rs:16-23 admits every loopback caller with no token). Putting nginx in front does not add auth — it LAUNDERS the absence of it.
4. GET /v1/conversations (routes.rs:343) and POST /v1/search (routes.rs:416) filter by tenant AFTER the SQL LIMIT (sqlite/conversation.rs:128-136, :185-190). Multi-tenant + load = your own conversations render empty. Not a leak; looks like data loss.
5. POST /v1/tasks/{id}/approve (routes.rs:375) takes no TenantId at all. Approval channel is a single global RwLock<String> (approval.rs:139) broadcast to every socket (:193).
6. No TLS in the crate at all. Zero tests on routes.rs / ws.rs / auth.rs / tenant.rs / approval.rs. No release job, no contract journey, no container build.
7. [knowledge_view] enabled defaults TRUE — background-ingests every conversation into corpora. [iroh] is defaulted ON by mobile_host (mobile_host.rs:83), tunnelling the local HTTP port to the public internet via third-party relays.

THE FIX SHIPPED: `dev-routes` cargo feature on sovereign-server, DEFAULT ON so every existing build is unchanged. --no-default-features compiles out (1), (2), (3) and ShellTool. Verified: dead-code count 47 -> 2 under the flag, confirming the modules are excluded from the binary rather than merely unreachable. Items (4) and (5) were NOT fixed — they are unreachable under the one-shared-tenant pilot posture and are recorded as hard blockers for a second tenant.

ALSO LOAD-BEARING FOR ANY TWO-PROCESS BOX: sovereign-server loads GGUFs in-process by default (main.rs:121-136), so running it alongside the daemon DOUBLES VRAM. The supported arrangement is a single [[inference.backends]] type="remote" pointed at 127.0.0.1:9741 — mobile_host.rs:301-322 already generates exactly this.

Plan + full ground-truth appendix: sovereign/deploy/onprem/PLAN.md
