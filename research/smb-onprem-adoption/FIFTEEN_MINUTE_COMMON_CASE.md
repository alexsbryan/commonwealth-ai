# The 15-minute common case

_2026-08-13 · RuggedFox · research status: assessment from code read, no runs_

The operator's bar: a person in a firm sees the pitch ("this sounds
cool"), and within 15 minutes has the batteries-included common case
running — ask a question over documents, in a browser, with citations,
on their own machine. Companion to `MULTI_TENANT_SMB_ADOPTION.md` (the
multi-tenant deployment assessment); this doc is the adoption-funnel
question: what stands between the current platform and that first
15 minutes.

## 1. What the 15 minutes must contain

| Window | Step | State today |
|---|---|---|
| t=0 | one command the person already trusts | EXISTS — `curl -fsSL https://svrnme.sh/install.sh \| sh` installs the three CLI binaries (`landing/install.sh`, platform-detect, checksummed) |
| t≈2m | binaries in place | EXISTS |
| t≈2-10m | model + embed + starter corpus download | EXISTS AS PARTS — `svrn setup` fetches GGUFs; the docs' CPU-only floor for a real primary is **~2.5 GB** (`TWO_NODE_QUICKSTART.md`), embed ~0.6 GB, a starter corpus ~0.5 GB (`svrn corpus install sep`). Total ≈ 3.5 GB ≈ 5 min on 100 Mb/s, longer on bad Wi-Fi — a bracket to measure, not a number to claim |
| t≈10-12m | point at their documents | EXISTS — `svrn corpus watch <dir>` is one command; sweep picks up additions |
| t≈12-15m | ask a question **in a browser**, get citations | **MISSING — no web UI exists anywhere in the platform** |

Everything before the last row exists. The last row is the whole gap in
the wow moment, and everything else is ceremony a single verb could
collapse.

## 2. What exists already (the inventory — most of this is assembled, not built)

- **The one-command install.** `landing/install.sh` does curl|sh with
  platform detection and checksums. Targets: Linux x86_64, macOS
  arm64/x86_64. No Windows.
- **Model acquisition.** `svrn setup` fetches GGUFs from HuggingFace —
  exactly the path the hardened kit deliberately avoids (`EGRESS.md` §4)
  and exactly the right path for a self-serve demo on the person's own
  machine.
- **A real primary at a laptop-sized floor.** The two-node quickstart
  states the CPU-only floor for a real primary at ~2.5 GB and runs its
  demo that way. The repo's dev models (11-35 GB) and the kit's quoted
  profile (35B Q4, 20.5 GB) are the wrong default here; the small
  profile is the battery.
- **Corpus from a folder in one command.** `svrn corpus watch <dir>`
  — watched-folder registration, five-minute sweep, failures reported
  not silent (`watched/worker.rs` → `scanned_no_text`).
- **A starter corpus in one command.** `svrn corpus install <recipe>`
  against the catalog (~0.5 GB for `sep`), or snapshot restore for a
  bundled one — the kit's `us-code` restore proves the mechanism.
- **The grounded runtime, headless.** `sovereign-server` already owns the
  runtime (`main.rs:248`), conversations, tenant-scoped storage, and a
  WebSocket stream that emits `QueuePosition`. It serves JSON over HTTP
  with bearer auth. What it lacks is only a surface a person would call
  an interface.
- **`svrn chat`** — terminal wow. Real, but a lawyer does not fall in
  love with a TUI.

## 3. The gaps, ranked

**Gap 1 — a browser chat surface. Nothing exists; everything it needs
exists.** `sovereign-server` already speaks conversations/messages/
corpora/search/WS-with-queue-signal behind bearer auth. A minimal static
page (one HTML file: ask box, answer with citations, conversation list,
"upload a folder is `svrn corpus watch`" note) served by the server and
driving the existing routes is days of work, not weeks — and it is the
difference between "ooo" and "up and running." An OpenAI-compatible
wrapper is the wrong first choice for this audience: it still assumes
they know what a client is. Browser or nothing.

**Gap 2 — the orchestration verb.** After `curl | sh`, the person faces
`svrn setup` (wizard), daemon config (no `--port` flag, config-by-file),
`svrn daemon run`, then `sovereign-server --config <file>` — where
`--config` is REQUIRED with no default, and the empty-`[auth.keys]` trap
silently serves every route unauthenticated as tenant `"default"`
(`server-config.toml:68-73`). None of that is one command, and two of
those traps are silent. The missing verb is roughly
`svrn firm-demo --docs <folder>`: pick the small profile, fetch
models+embed, write both configs with a generated key (the marker logic
already exists in the kit's `install.sh:191-199`), pin
`internal_bind = "127.0.0.1"` (see the hazard below), start daemon +
server, print `open http://localhost:PORT` + the key. Every primitive it
needs exists; the orchestrator does not.

**Gap 3 — the demo path must carry two safety defaults the general build
does not, because the person's laptop sits on the office network.**

- `[daemon] internal_bind` defaults to **0.0.0.0 with NO authentication**
  (`PLAN.md` appendix ports table; `daemon-config.toml` calls the loopback
  pin "the single most load-bearing line in this file"). A demo verb that
  forgets it puts an unauthenticated corpus-write and raw-model endpoint
  on the office LAN. This is not a hardening nicety; it is the difference
  between a demo and an incident.
- The auth trap above: the verb must generate a key and verify it landed
  inside `[auth.keys]` (install.sh's awk guard is the precedent).
- The web fallback/`web_fetch`/`wikipedia_fetch` egress tools are ON by
  default in a normal build (`net-tools` default on). For a self-serve
  demo that is arguably fine (it is the person's own machine, and the
  search-tool web fallback fires precisely when the corpus is thin — the
  demo case), but it must be a *stated* posture, not a silent one: the
  demo is not the air-gapped kit and should say so in one line.

**Gap 4 — demo probes, not customer-authored ones.** The kit's
`acceptance.sh` exits 2 (could-not-judge) until the firm authors golden/
abstention/OCR probes. A 15-minute flow cannot ask for authored content.
The demo needs bundled defaults: a starter corpus + a certified-absent
abstention probe against it — the repo's chaos-monkey question banks are
the precedent for certifiable probes (`sovereign-eval/src/chaos_monkey/question.rs`).
The wow moment *is* the abstention moment: ask something the folder
can't answer and watch it refuse rather than guess. That demonstration
is the product, and the demo should stage it rather than leave it to
chance.

## 4. The honest trade-off the demo makes

A ~2.5 GB primary on a laptop CPU is not the 35B profile the kit quotes.
Answers will be worse; citations and refusal will still work, and for
the 15-minute case that is the correct allocation — the demo proves the
*behaviour*, the quoted box proves the *quality*. The funnel is:
browser demo on their machine → the hardened kit on a GPU box (the
`MULTI_TENANT_SMB_ADOPTION.md` gaps) → multi-tenant rollout. Sell the
demo as the demo, and name the model-size trade-off in the pitch rather
than letting the customer discover it as "this thing is dumb."

Also stated, not silent: the demo path fetches from HuggingFace and runs
the un-hardened build on a personal machine — it is deliberately NOT the
zero-egress posture the kit sells, and the two artifacts must not be
confused in either direction (the demo verb must not be run on the firm's
server; the kit must not be the first thing a curious person touches).

## 5. Open questions

- **What does `svrn serve` actually serve?** Answered 2026-08-13: it is
  the code-intelligence MCP server (`sovereign-cli/src/main.rs:487`),
  not a web surface. Gap 1 does not shrink; a browser chat surface must
  still be built. (The web-experience concepts that build it:
  `WEB_EXPERIENCE_CONCEPTS.md`.)
- **The small-profile default is unmeasured.** "~2.5 GB at the CPU-only
  floor" is a doc claim, not a benchmarked profile for this use case.
  The demo verb needs one pinned (model file, quant, ctx size) with a
  measured first-answer latency on a laptop-class CPU — the person's
  first question must answer in seconds, not minutes, or the wow dies
  regardless of everything else.
- **Windows.** The installer has no Windows target. A firm is the most
  Windows-heavy customer imaginable. The 15-minute case on "the laptop I
  have" is Mac or Linux only today; Windows support is a funnel
  decision, not an implementation detail.
- **Where the demo's starter corpus comes from.** `sep` is philosophy —
  wrong genre for a firm. A small public-domain business/general corpus,
  or "no starter, just `--docs <their-folder>`," are both open. The
  abstention probe only needs *a* certifiable corpus, which `--docs` does
  not provide on day one (their folder's absence-set is uncertifiable).
