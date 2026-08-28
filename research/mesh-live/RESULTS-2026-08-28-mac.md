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

## Phases 3, 4, 5 — NOT RUN

Guest link over iroh, park/switch, and rotation all need the peer to run
commands or to have its status read. Nothing is claimed for them.

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
