# Live results — 2026-08-28, FOX side

Bars from `PRE-REGISTRATION-2026-08-28.md` (the brief calls it
`VERIFY_LIVE_2026-08-28.md`; that file does not exist under that name —
the pre-registration is the one that was written before any data).
Where an observation disagrees with the bar as written, the disagreement
is recorded, not the bar rewritten.

Host: FOX = RuggedFox (`node-44ae76142b0c3c72`), mesh `Meshsonics`
(`require_encryption=true`). Fedora HOST shell; all builds and daemon
control via `toolbox run -c sovereign-vulkan`.

## F0 — parity  **PASS**

- P0.1 **PASS**. HEAD `838c1e14d` ("Feat/edit slot dual lane (#48)");
  `git merge-base --is-ancestor ca254fe88 HEAD` exit 0. FOX is 5 commits
  ahead of the `ba87c1eca` MAC ran.
- P0.2 **PASS**. `cargo build --workspace --features
  corpus-engine/treesitter,sovereign-cli/dev-tools` clean in 19.6s.
  Daemon restarted; pid 2836091, `/proc/<pid>/exe` =
  `target/debug/sovereign-cli-daemon` mtime 08:18:06 > commit 08:12:27.
  (`dev-tools` added to the brief's command deliberately: a bare
  `--workspace` build silently downgrades `sovereign-cli`.)
- P0.3 **PASS**. `mesh status` -> 3/6 online; `Alexs-MacBook-Pro-2`
  reads **online**. Gossip healthy throughout: `reach ok
  peer=node-37f17554b6c4ff29` every ~10s, 23-36ms.

NOTE: the brief warned Linux might fail on `ibv_*`. It did not — no
ibverbs on this host, and `GGML_RPC_RDMA=OFF` is now pinned regardless.

## F1 — FOX's dial string

    46d0c1fb4e0c15d90ee54b372146e2c7ab3f185b3d71bbd5c271d98d56882aab@https://usw1-1.relay.n0.iroh.link./,69.181.167.209:42030,100.115.12.21:42030,192.168.1.13:42030

Relay-only robust form:

    46d0c1fb4e0c15d90ee54b372146e2c7ab3f185b3d71bbd5c271d98d56882aab@https://usw1-1.relay.n0.iroh.link./

## F2 — a dial string is not a credential (MAC as the dialled node)  **PASS**

FOX dialled MAC over iroh with a FRESH RANDOM key each time (a genuine
stranger; `dial_probe` defaults to a new identity). This is the reverse
direction of MAC's Phase 1 — pre-registration step 1.5.

| probe | observed | bar | verdict |
|---|---|---|---|
| stranger, `CLIENT_ALPN`, `/v1/models`, no bearer | **403** `{"error":"remote access not configured"}` | refused, not 200 | PASS |
| stranger, `CLIENT_ALPN`, `/status`, no bearer | **200** | 200 | PASS |
| stranger, `RPC_ALPN`, `/v1/models` | **NO RESPONSE** (connection error) | no response | PASS |

The `/status` body returned `node_id: node-37f17554b6c4ff29` — which is
`Alexs-MacBook-Pro-2` in FOX's roster. That is the discriminating
evidence the dial landed on MAC and not on a local listener.

BAR MISMATCH, recorded: pre-registration 1.1/1.5 said **401**; observed
**403**, from a second host, against the same node MAC probed. MAC
recorded the same mismatch and attributed it to `client_bind` being
forced loopback on an encrypted mesh, so `install_client_token(None)`
takes the fail-closed arm rather than the bearer-mismatch arm. Two
independent hosts observing 403 makes that systematic, not a one-host
artifact. The assertion the bar exists to test — refused, NOT 200 —
holds. The bar should have read "401 or 403".

## Phase 2 — REGRESSION BAR, reverse direction  **PASS** (new)

MAC recorded a SCOPE LIMIT on their 2.1: it exercised FOX's acceptor
because MAC dialled out, and said nothing about MAC's member check
admitting FOX. FOX closes that half.

FOX requested `Qwopus3.5-4B-v3-MTP-Q8_0` — resident on MAC, NOT held by
FOX (FOX's residents are `Qwen3.5-4B-UD-MTP-Q6_K_XL`,
`Qwen3.8-27B-UD-Q6_K_XL`, `Qwen3-Embedding-0.6B-Q8_0`):

    model : Qwopus3.5-4B-v3-MTP-Q8_0 @ peer Alexs-MacBook-Pro-2
    wall  : 1.58s, no 401

Peer federated inference carries no `Authorization` header, and
`require_encryption` puts inference on iroh in REQUIRE mode with no
plaintext fallback. So this went over `CLIENT_ALPN` and MAC's member arm
admitted it. **Both directions of the regression bar are now
established.** The acceptor change does not break peer serving.

FOX-side 2.2: **no** `CLIENT_ALPN dial from a non-member` line in FOX's
journal since restart, through continuous MAC gossip.
LIMIT, stated: since the restart FOX has only been observed dialling
OUT to MAC; inbound member traffic to FOX is not separately proven in
this window. MAC's F3 guest traffic will supply inbound evidence.

## F3 — guest link over iroh, FOX as LENDER  **grant PASS; guest half pending MAC**

`sovereign mesh grant --model Qwen3.5-4B-UD-MTP-Q6_K_XL --ttl 30m --label live`

- BAR: link query carries `dial=` -> **PASS**. Carries
  `&dial=46d0c1fb…%40https%3A%2F%2Fusw1-1.relay.n0.iroh.link.%2F%2C…`.
- BAR: output says "over the mesh tunnel" -> **PASS**. Verbatim:
  "Reach at: over the mesh tunnel (this mesh encrypts, so its plaintext
  API is closed to the network)".

Observed and worth naming: the link carries `url=http://100.115.12.21:9741`
**as well as** `dial=`. The bar forbids a link carrying ONLY `url=` (the
inert-link failure, note `3ec305f3`); both present is a pass, and on this
mesh that plaintext endpoint is closed, so `dial=` is the live path.

Token `e14d51ca…`, expires unix 1787932626. `grant --list` -> `live`.
Glassbox: `guest_grant: issued an ephemeral guest grant
expires_at_ms=1787932626301 grants=Qwen3.5-4B-UD-MTP-Q6_K_XL label="live"`,
and at boot `iroh(mesh): accepting GUEST_ALPN → bearer-only client
listener guest_forward=127.0.0.1:38985`.

Scope codes MAC will check are implemented and unit-tested, not
aspirational:
- 3.4 `model_not_granted` — `commonwealth-api/src/routes_inference.rs:74`,
  asserted at :2126 and :2141. Its comment names the §18.3 hazard it
  exists to prevent: without it a guest naming an ungranted model falls
  through to `default_model_id()` and is silently served something else.
- 3.5 `out_of_scope` (type `guest_scope`) —
  `commonwealth-api/src/client_auth.rs:289`.

PENDING MAC: 3.2 `mesh use`, 3.3 `chat ask`, 3.4/3.5 out-of-scope
probes, then FOX revokes for 3.6.

## F4, F5 — NOT RUN

Both are MAC-driven with FOX observing. Nothing claimed.

## Incidental observations (not bars)

1. **`mesh grant` has a cold-start race.** The first `mesh grant`
   seconds after `daemon start` failed with "No daemon detected on
   :9741 — minting a grant needs one", while the daemon was up and
   `curl /status` returned 200. Cause: `daemon_listening_on`
   (`sovereign-cli-llm/src/mesh_cmd.rs:2820`) probes `/v1/models` with a
   **500ms** timeout and treats any transport error as "no daemon".
   Measured cold `/v1/models` = 229ms, warm = 1-2ms; under index-open
   load it exceeds 500ms. Retrying warm succeeded. The failure text
   tells the operator to start a daemon that is already running.
2. **Two roster entries share one iroh endpoint.** `BeefyMac`
   (`b88252e4325bc377`) and `Alexs-MacBook-Pro-2` (`37f17554b6c4ff29`)
   both gossip via `iroh:127.0.0.1:45169→86627fd5` and both advertise
   `100.104.36.28` + the same IPv6. `86627fd5` is MAC's endpoint. Looks
   like a stale duplicate identity for the same physical Mac.
3. **Bridge retarget churn.** `iroh bridge: peer dial info changed —
   retargeted in place (port held) peer=86627fd5 bridge=127.0.0.1:45169`
   fires ~6x per 10s gossip round for the same peer with an UNCHANGED
   bridge address. Change-detection appears to be firing on equal input.

---

# RE-RUN — 2026-08-28 12:5x, fresh daemons both sides

The first run's Phase 3 was invalidated twice: once by the FOX-side auth
bug MAC found (grant unusable on a token-less daemon), once by FOX's
daemon restarting between mint and use and silently voiding the grant.
This is a THIRD observation, not a replacement for either.

State: FOX HEAD `4b8e966ae` == origin/main, `ca254fe88` ancestor.
Daemon pid **3873176**, started 12:55:13, single lifetime for everything
below (pid asserted at both ends of each sequence).

PARITY, stated precisely rather than as a blanket claim: the binary
(12:54:35) predates HEAD (12:57:30), but every commit in that window is
a rebase of next-edit work plus `.opencode/opencode.json`. Seven `.rs`
files are newer than the binary — all next-edit / fim / doctor. NONE in
the mesh, guest or auth path: `client_auth.rs`, `guest_grant.rs`,
`routes_inference.rs` and all of `sovereign-mesh` predate the build. The
binary carries the code under test. It is stale only for next-edit,
which is out of scope here.

## F0 / F1  **PASS**

MAC (`Alexs-MacBoo`) **online**, 2/6. BeefyMac reads **offline** despite
being booted on latest origin/main — not gossiped in at observation time.
FOX dial ports moved to 33276 (they move every restart).

## F2 — stranger dials, reproduced on fresh daemons  **PASS**

| probe | observed | bar |
|---|---|---|
| stranger `CLIENT_ALPN` `/v1/models` | **403** `remote access not configured` | refused, not 200 |
| stranger `CLIENT_ALPN` `/status` | **200**, `node_id: node-37f17554b6c4ff29` | 200 |
| stranger `RPC_ALPN` | **NO RESPONSE** | no response |

Third independent reproduction of the 403-not-401 mismatch. The bar text
should read "401 or 403".

## Phase 2 reverse — MAC's member arm admits FOX  **PASS**

    model : Qwopus3.5-4B-v3-MTP-Q8_0 @ peer Alexs-MacBook-Pro-2

Reproduced on fresh daemons. Both directions of the regression bar hold.

## F3 — mint  **PASS**, guest half pending MAC

Token `7af90e87`, 90m, verified PRESENT in the store immediately after
mint (`live:true, revoked:false`), pid unchanged 3873176.

**The volatility disclosure shipped and is observed.** The CLI now
prints:

    Expires:  in 1h30m (unix 1787952858), or when this daemon restarts

That closes the gap this run's second invalidation exposed: previously
the link promised an hour and could die in seconds with nothing said.
`dial_probe` also now carries `--body`, so the POST-shaped scope bar
(3.4) is drivable.
