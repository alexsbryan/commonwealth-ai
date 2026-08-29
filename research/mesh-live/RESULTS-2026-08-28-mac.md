# Live results — 2026-08-28, MAC side

Bars are in `VERIFY_LIVE_2026-08-28.md`, written before any of this ran and
NOT edited since. Where an observation disagrees with the bar as written, the
disagreement is recorded, not the bar rewritten.

Host: MAC = Alexs-MacBook-Pro-2, mesh `Meshsonics` (`require_encryption=true`).
Daemon: `target/debug/sovereign-cli-daemon`, built 08-28 00:05, restarted and
confirmed via `lsof` on pid 68034. Commit: `ba87c1eca` (contains `ca254fe88`).

## Phase 0 — parity

- P0.1 MAC: PASS. HEAD `ba87c1eca`, `ca254fe88` is an ancestor.
- P0.2 MAC: PASS. Binary 08-28 00:05 > commit; running pid's exe is that file.
- P0.3 MAC side: PASS. `mesh status` -> RuggedFox **online**, 2/6.
- P0.1-0.3 FOX side: **NOT RUN** — no shell on that host.

## Phase 1 — a dial string is not a credential  (MAC as the dialled node)

Dialled MAC's own published dial string from a FRESH RANDOM key over iroh.
This is a real test of the acceptor even on one host: the QUIC identity is what
`forward_for` reads, and a fresh key is a genuine stranger. It is NOT the
loopback-HTTP trap of note `1a0b8482` — no request here originates on loopback;
the acceptor's forward hop is what makes it look that way, which is the whole
point.

| probe | observed | bar |
|---|---|---|
| stranger, `CLIENT_ALPN`, `/v1/models`, no bearer | **403** `remote access not configured` | PASS as "refused" |
| stranger, `CLIENT_ALPN`, `/status`, no bearer | **200** | PASS — exempt path stays open |
| stranger, `GUEST_ALPN`, `/v1/models` | **403** | PASS — reached the bearer listener |
| stranger, `ALPN` (internal), `/v1/models` | **404** | PASS — reached the INTERNAL router (route absent there), i.e. a different listener; open by design for joiners |
| stranger, `RPC_ALPN` | **no response** | PASS — refused outright, no downgrade |

BAR MISMATCH, recorded: 1.1 was pre-registered as **401** and observed **403**.
The assertion is "refused, not 200", which holds. 403 rather than 401 is
correct here and I named the wrong code: on an encrypted mesh `client_bind` is
forced to loopback, so `install_client_token(None)` runs and `client_auth_layer`
takes its fail-closed arm ("bound somewhere a remote could reach us, but no
token was ever configured") instead of the bearer-mismatch arm. A node with a
token installed would answer 401. The bar should have said "401 or 403".

Five distinct outcomes across five ALPNs is itself the result: one listener
answering everything cannot produce 403 / 200 / 403 / 404 / closed.

GLASSBOX (§9.1) — both new branches are visible at INFO/WARN in `daemon.err`:

    INFO  iroh(mesh): CLIENT_ALPN dial from a non-member — routing to the
          bearer-checking listener (closing if it did not bind) dialer=403ba7ed03c…
    WARN  iroh(mesh): REFUSED an RPC_ALPN dial from a non-member — the
          rpc-server authenticates nothing, so there is no safe downgrade dialer=6aeb…
    INFO  iroh(mesh): accepting GUEST_ALPN → bearer-only client listener
          guest_forward=127.0.0.1:53099

NOT ESTABLISHED: the member arm on a live daemon. The only member key on this
host is MAC's own, and dialling a node with its own key is a self-dial that
never connects. Needs FOX.

## Phase 2 — REGRESSION BAR (the one that could force a revert)

2.1 **PASS.** `POST /v1/chat/completions` for `Qwen3.5-4B-UD-MTP-Q6_K_XL`, a
model only RuggedFox advertises and holds resident:

    model  : Qwen3.5-4B-UD-MTP-Q6_K_XL @ peer RuggedFox
    answer : mesh ok            (0.78s wall)

Peer federated inference carries no `Authorization` header, and on this mesh
`require_encryption` puts every class on iroh in REQUIRE mode with no plaintext
fallback — so this went over `CLIENT_ALPN` and was admitted. Cross-node serving
is not broken by the acceptor change.

2.2 **PASS.** No `CLIENT_ALPN dial from a non-member` line accompanies the
inference. The three that exist all match my own probe dials.

SCOPE LIMIT, stated: 2.1 exercises **FOX's** acceptor, because MAC dialled out.
It says nothing yet about MAC's member check admitting FOX. That needs traffic
in the other direction.

## Phase 3 — guest link over iroh. 3.2 FAILED. 3.3-3.7 COULD NOT JUDGE.

FOX minted the link (its 3.1) and sent it at 08:32 local. MAC is the guest.

**3.2 FAILED.** `svrn mesh use '<link>'`:

    The issuing node did not accept this link: remote access not configured

Not a transport failure, and not FOX's configuration. Three observations on
the SAME tunnel, same ALPN (`cwth/guest/0`), same listener, minutes apart:

| probe | credential | result |
|---|---|---|
| FOX plain `http://100.115.12.21:9741/status` | — | `000` (unreachable — correct, encrypted mesh binds loopback) |
| `dial_probe --alpn guest --path /status` | none, fresh random stranger key | **HTTP 200**, body names `node-44ae76142b0c3c72`, mesh Meshsonics, `Qwen3.5-4B-UD-MTP-Q6_K_XL` resident |
| `dial_probe --alpn guest --path /v1/models --bearer <grant>` | the valid grant | **HTTP 403** `{"error":"remote access not configured"}` |

The tunnel, relay, guest ALPN and guest listener are all live — the exempt
path answers 200 to a caller holding nothing. The credentialed path refuses
the grant. That pair isolates the failure to the auth layer alone.

**Cause, cited.** `commonwealth-api/src/client_auth.rs::client_auth_layer`
read `let Some(expected) = state.client_token() else { 403 "remote access not
configured" }` BEFORE the guest-grant arm, which sat inside the `match` that
followed. `"remote access not configured"` has exactly one production
emitter, so FOX has no daemon client token — and a live grant was therefore
unreachable on the very daemon that minted it. `svrn mesh grant` minted a
link its own node refuses. Same class as note `68bfc154`.

**3.3 — same branch, second path.** `svrn chat ask` through the stored link
(`--no-verify`): `POST /v1/conversations: 403 Forbidden {"error":"remote
access not configured"}`. The CLI correctly reported routing "over the mesh
tunnel" to FOX first, so link storage and routing are not implicated.

**3.4, 3.5, 3.6, 3.7 — COULD NOT JUDGE, not failed.** Every one of them is
downstream of the arm that refuses: the scope check (`permits_path`), the
`model_not_granted` refusal and the revoke-takes-effect bar can only be read
once a grant is admitted at all. Nothing is claimed for them.

Fixed on MAC in the same session (`client_auth.rs`): the daemon token and the
grant are independent credentials, daemon token still checked first so a
guest token cannot widen into full access by matching an earlier arm, and the
no-credential-at-all case still 403s `remote access not configured`. Watched
failing: `a_guest_grant_is_honoured_on_a_daemon_with_no_client_token` and
`no_token_and_no_grant_still_refuses_a_credential_less_remote_caller`.

Phase 3 must be RE-RUN against a build carrying that fix. This record is the
first observation; the re-run is a second one, not a replacement.

## Phase 3 — SECOND OBSERVATION, on FOX grant `370eafa6`. 3.2/3.4/3.5 PASS.

The first round's 401 was CORRECT BEHAVIOUR ON A DEAD TOKEN, not an auth bug.
FOX's `/internal/guest/grant/list` returned `200 []` — empty, with `svrn mesh
grant --list` agreeing, which rules out the two-process pubkey collision of
note `88e3353e`: one store, and it was empty. FOX's daemon had been through
three pids in twenty minutes and the grant was minted into the middle one.
Grants are `Mutex<HashMap>` in RAM by design. So the fix was not implicated;
it was reporting a token that no longer existed.

Re-run on a grant verified present in FOX's store, MAC on a freshly built
`sovereign-cli-llm` (a stale sibling invalidates any claim about mesh CLI
behaviour — the dispatcher warns about exactly this):

| bar | pre-registered | observed | verdict |
|---|---|---|---|
| 3.2 | verifies, stores, prints granted ids read back FROM FOX | `Verified against http://100.115.12.21:9741 (over the mesh tunnel) — models in scope: Qwen3.5-4B-UD-MTP-Q6_K_XL` | **PASS** |
| 3.4 | refused, code `model_not_granted` | `403` · `{"code":"model_not_granted","message":"this guest link does not cover model 'llama-3.3-70b' — it grants: Qwen3.5-4B-UD-MTP-Q6_K_XL"}` | **PASS** |
| 3.5 | `403`, code `out_of_scope` | `403` · `{"code":"out_of_scope","message":"this guest link does not cover /v1/knowledge/search"}` | **PASS** |

POSITIVE CONTROL, not in the pre-registration and added because 3.4 and 3.5
are both REFUSALS — a listener that refused everything would pass both and be
useless (§18.1). POST `/v1/chat/completions` naming the GRANTED model, same
tunnel, same bearer:

    HTTP 200 · {"model":"Qwen3.5-4B-UD-MTP-Q6_K_XL",
                "choices":[{"message":{"content":"mesh ok"}}],
                "usage":{"total_tokens":26}}

A non-member, holding nothing but a scoped bearer, ran inference on FOX's
model over the mesh tunnel. That is the feature.

3.3 REMAINS FAILED and is not FOX's. `svrn chat ask` is a pure surface — the
turn runs on the daemon — so pointing the CLI at a lender sends the whole
conversation there, and a grant scopes only `/v1/models` +
`/v1/chat/completions`. Fix is daemon-side guest routing on the GUEST's node.

3.6 (revoke takes effect on the next request) and 3.7 (`--forget` returns
chat to the local daemon) not yet run.

TOOLING CHANGED MID-RUN, disclosed because it is part of the instrument:
`dial_probe` gained `--body` (POST). The interesting scope bars are POST
routes, and a GET against one is refused by the auth layer BEFORE routing, so
it cannot tell an out-of-scope refusal from a method mismatch — which is the
distinction 3.4 is about.

## Phase 3, THIRD observation — on `129460da4` + two live-only fixes. 3.3 STILL NOT DEMONSTRATED.

3.2 PASS again (verified through the tunnel, ids read back from FOX).

**TWO BUGS THE UNIT TESTS WERE STRUCTURALLY BLIND TO, both found only here.**

1. `StoredGuestLink` was constructed with the daemon's `cfg.data.dir`, which on
   this machine is `/Users/alexsbryan/.sovereign`, while `svrn mesh use` writes
   `/Users/alexsbryan/.svrnmesh/guest.json`. Different directories, so every
   lookup found nothing — silently, with no error on any surface. The unit
   tests hand both halves the same tempdir, so the roots were equal by
   construction and none of them could fail. Fixed structurally:
   `StoredGuestLink::new()` now takes NO path and resolves
   `svrnmesh_root()`, the SSOT both halves already share; `new_in` is
   test-only. Regression test `the_default_root_is_the_one_the_cli_writes_to`.

2. Only the hot-RELOAD factory (`daemon_cmd/provider.rs`) called
   `set_guest_source`. The COLD-START assembly point
   (`bootstrap::build_mesh_provider`) did not, so a freshly started daemon kept
   `NoGuestLenders` and the route was dead until something happened to trigger
   a provider reload. Wired at the assembly point, which exists precisely so
   there is "no slot this bootstrap can forget".

**AFTER BOTH FIXES, the listing half is PROVEN LIVE:**

    GET /v1/models ->  Qwen3.5-4B-UD-MTP-Q6_K_XL | advertised_by: ["http://100.115.12.21:9741"]
    daemon.err    ->  guest-lender: opened the mesh tunnel to a lending node
                      lender=http://100.115.12.21:9741 bridge=http://127.0.0.1:53093

The lender is a holder under its OWN name, not `local` and not a peer name.

**3.3 IS NOT DEMONSTRATED, and the reason is a seam not previously named.**
Two runs, both served from MAC's own slot:

    mesh-inference: scoring local ... local_pick="Qwen3.8-27B-UD-Q6_K_XL"

`scoring local` is the RANKED path — the request carried no explicit model id
at all. The conversation DID stay local (correct, and the 3.3 fix works), but
so did the completion. Cause: `sovereign-turn-client` has no model parameter
anywhere (`grep model` in `src/lib.rs`: one doc comment, no field), so a
daemon-run turn picks its own model and the CLI's resolved `chat_model` never
reaches it. Making bootstrap prefer a lender-advertised id — landed, and
correct on its own terms — cannot fix this, because the id is never put on the
wire.

WHAT IS OWED for 3.3: the DAEMON's turn pipeline must prefer a granted model
when a live link exists. That is the same thesis as the rest of this change
(the link is a daemon-level fact), applied to model SELECTION rather than to
model ROUTING. Not attempted here; naming it rather than guessing at it.

NOT ROUNDED UP: two runs answered a question and looked fine on stdout. Both
were MAC talking to itself. The pre-registered discriminator is what caught it.

## Phases 4, 5 — NOT RUN

Park/switch and rotation need the peer to run commands or to have its status
read. Nothing is claimed for them.

## Build repair (prerequisite, not a mesh result)

`main` was unbuildable on macOS from `5dde3e6e6` (#52). Two independent causes,
both from the vendored llama.cpp fast-forward `1464c62d88 -> 035e22731a`:

1. 20 `ggml/src/ggml-metal/kernels/*.metal` sources were never vendored (the
   fast-forward refreshes files that already exist), and the old single-file
   `ggml-metal.metal` was deleted as "inert". cmake could not configure:
   `file STRINGS file .../kernels/fa.metal cannot be read`.
2. ggml-rpc gained an RDMA transport that AUTO-ENABLES when `find_library`
   locates `rdma`/`ibverbs`, but `build.rs` emits the Rust link line and never
   learned about it: `ld64.lld: error: undefined symbol: ibv_reg_mr` +7.

Fixed: kernels vendored (tree 1784 files, byte-identical to upstream);
`GGML_RPC_RDMA=OFF` pinned explicitly on every platform so the build does not
depend on host archaeology; `verify-vendored-llama-cpp.sh` gained a REVERSE
pass so an OMISSION fails the way an edit already did, with the deliberate
exclusions declared in `vendor/llama-cpp-sys-4/VENDOR_EXCLUDE`.

The reverse pass was watched failing: remove one kernel, it reports
`1 missing locally` and exits 1. Clean tree exits 0 at 1784/1784.

Linux is unaffected by both (GGML_METAL=OFF, and no ibverbs found), which is
why #52 landed green from the Fedora host.

---

# Round 2 — afternoon, after the daemon-side model-selection fix

Daemon pid 47769, binary mtime 15:18:40, restarted 15:18 (P0.2 satisfied:
binary predates the process, and the running exe is that binary).
Grant `5efa5c56`, minted by FOX 22:10:53 UTC, TTL 60m.

## 3.2 PASS (re-run, new grant)

    Verified against http://100.115.12.21:9741 (over the mesh tunnel) — models in scope:
      Qwen3.5-4B-UD-MTP-Q6_K_XL

## 3.3 PASS — first time it is actually demonstrated

    stdout banner:  … · 135.0s · LOOKUP · Qwen3.5-4B-UD-MTP-Q6_K_XL
    stderr:         Guest link: routing to http://100.115.12.21:9741 over the mesh tunnel

    22:22:31  mesh-inference: a live guest link supplies the primary model for a
              turn that named none  lender=http://100.115.12.21:9741
              model=Qwen3.5-4B-UD-MTP-Q6_K_XL
    22:22:31  mesh-inference: serving a named model from a GUEST LINK
    22:22:31  mesh-inference: routing to a GUEST LENDER by model name  soft=true

The pre-registered discriminator (`scoring local` / `local_pick=`) does NOT
appear for the answering call. Three earlier calls in the same turn fell
through with `reason=PrivacyLocalOnly` — internal stages carrying a
`local_only` envelope, which is correct and is pinned by
`a_local_only_envelope_keeps_a_bare_turn_home_despite_a_live_grant`.

## THREE ROOT CAUSES, all found only by running it on two machines

**R1 — the turn pipeline never named the granted model.** `serve_turn` builds
its `CompletionRequest` with `model_id: None`, so `select_route` fell to
ranked scoring and answered locally. Fixed: a live grant supplies the SOFT
named target, guest before shared. No privacy gate at the naming site — the
enforcement already lives one call down in `resolve_named_dispatch`, and a
second reading of "may this leave" is the §10.6 duplicated decider. The
conservative reading would have made the fix inert, since the turn path
carries no OICP envelope at all.

**R2 — a bridge died silently and the cache kept selling its address.**
`HttpBridge`'s accept loop was `let Ok(..) = accept().await else { break }`
with NO tracing on the branch, so one transient error killed the port
permanently and invisibly. Observed: tunnel opened 21:11:10 on port 61564,
served a `/v1/models` listing, and by 21:12:39 refused connections with
nothing in the log. `StoredGuestLink` cached that base URL and kept handing it
out, so the failure presented as "the lender revoked your grant". Fixed both
ends: the loop logs every accept failure, survives transient ones, exits only
after 32 consecutive with a loud ERROR naming the dead port; `Drop` traces;
and `route_for` TCP-probes the cached bridge before use and reopens if dead.

**R3 — a refused grant was indistinguishable from no grant.** FOX's service
manager restarted it at 14:19:38 (grants are RAM-only), MAC's next four
requests got `403`, and `granted_models()` returned `None` — the same value it
returns for a node that never borrowed anything. Every one of those turns was
answered by MAC's own 27B, silently. That is the SAME defect this exercise was
convened to catch, reached by a different route. Fixed with a three-state
`GrantPosture { NoLink, Unusable{lender,why}, Granted{lender,ids} }`; an
`Unusable` posture refuses the turn naming the lender, the reason and
`--forget`, bounded by the link's own TTL.

Tests: 9 in `guest_lender_routing`, 6 in the `guest_lender` unit module, all
green. Four are new and discriminating — a bare turn reaching the lender, a
`local_only` envelope staying home, a refused grant refusing the turn, and a
liveness probe that must tell a bound listener from a dropped one.

## Not yet judged

3.6 (revoke-in-place) — sampler armed, awaiting FOX's revoke.
3.7 (`--forget`) — follows 3.6.
