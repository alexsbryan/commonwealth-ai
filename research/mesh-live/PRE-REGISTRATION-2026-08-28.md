# Live verification — two machines, 2026-08-28

PRE-REGISTERED BEFORE ANY DATA. Bars are falsifiable and stated as the
OBSERVATION, not as "it works". Anything not listed here is not a result.

Hosts: MAC = Alexs-MacBook-Pro-2 (this box). FOX = RuggedFox.
Mesh: `Meshsonics`, `require_encryption = true` — so BOTH nodes bind their
client + internal APIs loopback-only and iroh is the only ingress. Any test
that "passes" over a plain LAN address on this mesh is measuring the wrong
thing.

## Phase 0 — parity. NOTHING BELOW MEANS ANYTHING UNTIL THIS PASSES.

P0.1  Both hosts at a commit that contains `ca254fe88` ("iroh access").
      `git log --oneline -1` and `git merge-base --is-ancestor ca254fe88 HEAD`
      -> exit 0 on both.
P0.2  Both daemons RESTARTED after that build. Binary mtime > commit time, and
      the running pid's exe is that binary.
      FOX builds fine without the Metal repair (GGML_METAL=OFF on Linux); MAC
      needs it (see LLAMA_CPP_COMMIT, 2026-08-28).
P0.3  `sovereign mesh status` on BOTH: the other node reads `online`.
      If FOX shows MAC offline or vice versa, gossip is not flowing and every
      bar below is unreadable. STOP and fix reachability first.

## Phase 1 — the acceptor fix: a dial string is not a credential

The finding: an iroh endpoint accepts anyone, the dial string is public (it is
in every invite's `dial=` and gossiped as `node_pubkey`), and the acceptor
forwards to a loopback listener that admits loopback before reading a bearer.

1.1  MAC dials FOX's `CLIENT_ALPN` from a FRESH NON-MEMBER key, no bearer,
     GET /v1/models.
     BAR: **401**. (Pre-fix this is 200 with the full client API.)
1.2  Same dial, same key, presenting FOX's daemon client token.
     BAR: **200** — a stranger is downgraded to the bearer gate, not walled off.
1.3  Same dial, same key, no bearer, GET /status.
     BAR: **200** — AUTH_EXEMPT_PATHS stay readable; a node must be able to
     read the federation handshake before it could hold anything.
1.4  Same as 1.1 but dialing `RPC_ALPN`.
     BAR: **connection closed / request errors**. No downgrade exists: the
     ggml rpc-server authenticates nothing.
1.5  Reverse direction: FOX runs 1.1 against MAC. BAR: same 401.

## Phase 2 — REGRESSION BAR. This is what Phase 1 could break.

Peer federated inference carries NO Authorization header; its credential is
membership-by-key. If the member check is wrong, peers 401 each other and the
mesh stops serving. This bar is the reason Phase 1 is not free.

2.1  MAC: `svrn chat ask` routed to a model FOX serves (or any cross-node
     inference), while both are online.
     BAR: **served, no 401**, and the answer comes from FOX.
2.2  MAC daemon log during 2.1.
     BAR: **NO** `CLIENT_ALPN dial from a non-member` line naming FOX's key.
     That line firing for a real member means the member check is reading the
     wrong thing, even if the request somehow succeeded.

## Phase 3 — guest link over iroh (the feature)

FOX is the LENDER (it has the models). MAC is the guest.
NOTE: MAC is a MEMBER of Meshsonics. That does not invalidate the test — the
guest path rides `GUEST_ALPN`, whose listener admits any dialer and reads the
bearer — but it does mean 3.4 is the assertion that the SCOPE bound, not the
transport.

3.1  FOX: `svrn mesh grant --model <id> --ttl 30m --label live`.
     BAR: prints a link whose query carries **`dial=`**, and the output says
     "over the mesh tunnel". A link carrying only `url=` on this mesh is the
     old inert-link failure (note 3ec305f3).
3.2  MAC: `svrn mesh use '<link>'`.
     BAR: verifies and stores; prints the granted model ids read back FROM FOX
     through the tunnel.
3.3  MAC: `svrn chat ask "..."` .
     BAR: answered, stderr names FOX and "over the mesh tunnel", and the
     response `model` is the granted id.
3.4  MAC, through the tunnel, POST /v1/chat/completions naming a model the
     grant does NOT list.
     BAR: **refused**, error code `model_not_granted`. Never served, never
     silently swapped for the default (§18.3).
3.5  MAC, through the tunnel, GET /v1/knowledge/search.
     BAR: **403**, error code `out_of_scope`.
3.6  FOX: `svrn mesh grant --revoke <token>`. Then MAC repeats 3.3.
     BAR: **401 on the very next request** — no restart, no wait.
3.7  MAC: `svrn mesh use --forget`. BAR: chat returns to MAC's own daemon.

## Phase 4 — multi-membership: park, not leave

From the earlier pre-registration (`VERIFY_LIVE.md`, steps 1-4), unchanged.

4.1  MAC: `svrn mesh list` -> Meshsonics, active.
4.2  MAC: join/create a SECOND mesh.
     BAR: both listed, second active, Meshsonics **parked**; and
     `<root>/meshes/<meshsonics-id>/mesh.json` + `join_key.secret` unchanged
     byte-for-byte (checksum before and after).
4.3  FOX: `svrn mesh status`.
     BAR: MAC reads **Offline**, still in the roster, **`removed_at` absent**.
     A tombstone here is the auto-leave regression.
4.4  MAC: `svrn mesh switch Meshsonics` — **with no invite redeemed**.
     BAR: resumes; mesh id is UNCHANGED (id continuity is the discriminating
     observable — auto-leave rolls into a fresh solo with a NEW id); FOX sees
     MAC Online within one gossip round.

## Phase 5 — founder rotation propagates

5.1  Wait three gossip rounds (~30s) with both online. MAC: `svrn mesh rotate`.
     BAR: **succeeds with no `--force`**, because every online peer is
     confirmed post-split.
5.2  Wait 30s. `svrn mesh status` on BOTH.
     BAR: every peer still **Online**, no `removed_at` anywhere. This is the
     step that fails on pre-#51 code.
5.3  Attempt a join using the PRE-rotation join key, from a third node or a
     container, with **no daemon restart in between**.
     BAR: **refused**.
5.4  FOX: after 5.1, its `invite_key_hash` / `invite_version` match MAC's.
     BAR: FOX admits on the NEW key. Rotation that is node-local is the P2 bug.

## Stop conditions

- Phase 0 fails -> stop, nothing else is readable.
- Phase 2 fails -> STOP AND REVERT the acceptor change. A mesh that cannot
  serve its own peers is worse than the hole it closed.
- Phase 5.2 shows a partition -> `svrn mesh rotate --force` was NOT consented
  to; rejoin FOX before continuing.
