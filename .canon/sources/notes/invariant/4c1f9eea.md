# Encrypted-mesh join failing with \"no known protocol\"/ALPN reject despite the founder's acceptor being registered can be a STALE iroh…

Encrypted-mesh join failing with \"no known protocol\"/ALPN reject despite the founder's acceptor being registered can be a STALE iroh relay/pkarr registration on a long-lived founder daemon — restart the founder to refresh it.

On 2026-07-18, RuggedFox could not join the encrypted mesh "Meshsonics"
(founder = BeefyMac, this Mac). RuggedFox's iroh debug showed it connected to
the relay, discovered the Mac via pkarr, reached the endpoint, and got
error 120: "peer doesn't support any known protocol" (ALPN reject). The
peer agent concluded the Mac's dial-by-key acceptor never registered
`cwth/http/0`, citing `grep -c "dial-by-key access enabled" ~/…/daemon.err = 0`.

That grep was a log-rotation FALSE NEGATIVE. The acceptor line is emitted at
daemon startup; the founder had been up ~1.5 days, and `daemon.err` rotates at
10 MiB (copy-truncate, keep 5 baks — see `log_rotation.rs`). The startup line
had rotated into a `.bak`. Grepping `daemon.err*` (incl. baks) showed the
acceptor DID register `cwth/http/0` on endpoint `86627fd5…` repeatedly, with
zero `endpoint bind failed`. ALPN strings matched on both sides
(`cwth/http/0`, iroh 1.0.2). So it was never an acceptor/ALPN/build problem.

**Actual cause (confirmed by the fix): the long-lived founder's iroh ingress
address had gone stale.** Last acceptor (re)registration was ~31h before the
join attempt; peers dialing the pubkey reached a dead address. **Restarting the
founder fixed it immediately** — same node key (invite pubkey `86627fd5…`
unchanged, so the invite stayed valid), but a fresh endpoint (new QUIC port +
fresh relay/pkarr publish). RuggedFox's next pkarr lookup hit the live acceptor,
`cwth/http/0` negotiated, `/internal/join` ran, `handshake_accepted`.

How to apply: When an encrypted-mesh join rejects at ALPN ("no known
protocol") but the founder's acceptor is demonstrably up (grep ALL `daemon.err*`
incl. baks for `dial-by-key access enabled`; check for `endpoint bind failed`),
suspect stale iroh ingress on a long-uptime founder before chasing build/ALPN
skew. Restart the founder and have the joiner re-fire with a FRESH lookup.
Diagnostic grep targets are INFO on allowlisted targets and need no special env:
`dial-by-key access enabled` (target `sovereign_mesh`), `handshake_accepted` /
`handshake_rejected` on `/internal/join` (target `commonwealth_api`). The iroh
internals (relay-connect / pkarr / error 120) are dark unless
`RUST_LOG=iroh=debug,commonwealth_transport=debug,…`. Encrypted mesh is
fail-closed iroh-only — the `relay=<lan-ip>` invite hint is inert (see
[[project_encrypted_mesh_impl_2026_06_22]]). Open follow-up: is there an endpoint
keepalive / periodic re-publish that should prevent this staleness automatically?

---
