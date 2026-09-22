<!-- ledger -->

**browser-dial-2 · 2026-09-22 · bd-1-browser-dials-relay · director** — this commit
- Needed: the-link's tl-3 clause (c) — a plain browser dialing a node over iroh's relay — was deferred when clause (b) was recorded as a build failure. Bar `bd-browser-dials-relay` clause (b) requires one grant-scoped `GET /v1/rail/log` to return through a live browser dial, clause (c) requires every layer named with its verbatim outcome, clause (d) requires a zero product diff.
- Chose: built the page under `target/` (Rust → wasm32-unknown-unknown; `iroh =1.0.2`, `wasm-bindgen =0.2.128`, a real browser endpoint) and opened it in Google Chrome for Testing 147.0.7727.15 (headless) driven by the repo's vendored `playwright-core` 1.59.1. It dialed a live node's `GUEST_ALPN` (`cwth/guest/0`) through the n0 relay and returned **`HTTP/1.1 200 OK`** from `GET /v1/rail/log` with the guest bearer, carrying the named rail row **`ring-658e43cce7830b48`**. WORKED.
- Because: each layer answered in turn — the wasm endpoint bound, the relay handshake completed, the QUIC connection negotiated `cwth/guest/0` and established, the request rode one bi-stream, and the guest-channel admission read the grant and served the rail. The one non-obvious path fact (a wasm client must hold the bi-stream's send half open until the response, as `HttpBridge::pump` does) is recorded below with both verbatim runs. No product code moved — this row's own `git diff` over sovereign/crates and commonwealth/crates is empty.

<!-- appendix -->

## browser-dial-2 · 2026-09-22 — the browser dial is a measured fact: every layer answered

<details><summary>reasoning, evidence, package</summary>

**The claim.** A plain browser, on a network the daemon has never seen, opens the
guest link and dials the node itself over iroh's relay, then makes one
grant-scoped rail read. The dial is possible because:
- the daemon routes `GUEST_ALPN = b"cwth/guest/0"` (`commonwealth-transport/src/iroh.rs:101`)
  to a loopback Guest listener whose auth layer reads a bearer and admits only a
  live guest grant whose `permits_path` covers the request (`client_auth.rs:265-311`);
- `Scope::Rails` grants `permits_path` for `/v1/rail/log`
  (`sovereign-grants/src/guest_grant.rs:132-135`);
- the browser half speaks HTTP/1.1 on one QUIC bi-stream, which is exactly what
  `IrohAcceptor`'s `pump` (`commonwealth-transport/src/iroh.rs:875`) expects —
  a byte splice to a local hyper listener.

**The page (never committed; all under `target/ralph/browser-dial/`).**
- `page-build/` — Rust cdylib, `iroh = "=1.0.2"`, `wasm-bindgen = "=0.2.128"`,
  `wasm-bindgen-futures = "=0.4.78"`, `js-sys = "=0.3.105"`; own `[workspace]`,
  manifest KEPT so the build is re-derivable.
- Build: `cargo build --manifest-path …/page-build/Cargo.toml --target
  wasm32-unknown-unknown` → `Finished \`dev\` profile … in 19.03s` (exit 0).
  `wasm-bindgen --target web` (CLI 0.2.128) wrote the JS glue. iroh's wasm tree
  pulls `getrandom 0.4` with `wasm_js` (target-activated), so unlike the
  getrandom-0.3 note in `docs/RING_APP_LIBRARY.md:360-380` no `--cfg
  getrandom_backend` was needed.
- `site/` — the page; `run-page.mjs` — a node server that serves it and a
  `playwright-core` launcher; `run-live.log` — the run below.

**The live node dialed (a throwaway current-binary node, not the deployed daemon).**
- `target/debug/sovereign-cli-daemon daemon run`, `SOVEREIGN_DATA_DIR=target/ralph/browser-dial/live`,
  `client_port=19841`, `[iroh] enabled = true`, `[discovery] mdns = false` — the
  node shape `scripts/ring-doc-demo.sh`'s local backend uses (`mkcfg`/`start_daemon`),
  which `scripts/ring-room-demo.sh` sources. Its own mesh, its own key, the same
  n0 relay the deployed daemon uses.
- One rail act seeded (`svrn ring introduce alex --key <node> --ring browser-dial-ns`
  → `ring-658e43cce7830b48`); one grant minted via the internal route
  (`POST /internal/guest/grant`, `scopes = {rail:"browser-dial-ns"}`, ttl 1200s).

**The layers, verbatim (from `run-live.log`, engine = Google Chrome for Testing 147.0.7727.15):**

```
PAGE: engine=Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/147.0.0.0 Safari/537.36
LAYER wasm-bindgen/iroh-endpoint: ok — local endpoint ac87abb295b2711ec3444f63c7b79e132fd4c76cf8d66a0cac20ac7015dd11c3 bound, relay mode default
LAYER relay-connect: ok — endpoint reports online (relay handshake done)
LAYER tls/alpn+relay-hop: ok — QUIC connection established to 22a72bb42fc2e9c9c1473e629d67862f83ddfe3c382a0a08585ee1dbfe5186ce on GUEST_ALPN
LAYER tls/alpn: negotiated ALPN = "cwth/guest/0"
LAYER request-write: ok — 185 bytes of HTTP/1.1 sent on the bi-stream (send half kept open until the response, as the native bridge does)
LAYER daemon-guest-admission: response received, 632 bytes
RESPONSE-STATUS: HTTP/1.1 200 OK
RESPONSE-BODY: … {"namespace":"browser-dial-ns","ops":[{"id":"ring-658e43cce7830b48","actor":"22a72bb42fc2e9c9c1473e629d67862f83ddfe3c382a0a08585ee1dbfe5186ce","person":"alex","seq":0,"ts_unix":1790105778,"voided":false,"payload":{"key":"22a72bb4…","kind":"introduce","person":"alex","reason":"browser-dial bd-1 measurement act"}}],"gaps":[],"held":1,"complete":true, …}
```

**What would falsify each layer.**
- *wasm bindgen / iroh-endpoint*: a toolchain without `wasm32-unknown-unknown`
  std, or a `wasm-bindgen` CLI whose version differs from the crate's, fails to
  instantiate — the inventory row's build reproduction is the positive control.
- *relay connect*: a browser with no route to `wss://usw1-1.relay.n0.iroh.link./`
  never reports `online`; the page's 120 s race would print `TIMEOUT` beside the
  last emitted LAYER line.
- *TLS/alpn*: a node that does not advertise `GUEST_ALPN` refuses the connect at
  ALPN negotiation — `commonwealth-transport/src/iroh.rs:1386`
  (`an_unrouted_guest_alpn_is_dropped_rather_than_falling_back`) is the watched
  negative.
- *guest-channel admission*: a bearer that is not a live grant earns `401`, and a
  live grant whose `permits_path` does not cover the path earns `403`
  (`client_auth.rs:313-324`); a route the surface does not mount earns `404`
  (measured live, below).
- *request transport*: on the FIRST attempt the page called `send.finish()`
  immediately after `write_all`, and `read_to_end` returned `Ok(0 bytes)` with no
  status line. Holding the send half open until after the response — the flow
  `HttpBridge::pump` uses (`commonwealth-transport/src/iroh.rs:875`, `finish()`
  only after the local side is done) — returned the 200. This cost one run and is
  recorded because it is the one path fact a wasm client must get right and the
  iroh API does not make it obvious.

**Side finding — the deployed daemon is a 5-day-old process, so its
`/v1/rail/log` 404s; that is a node-version fact, not a dial fact.** The browser
dialed the operator's live daemon too (grant `Qwen3.5-4B-UD-MTP-Q6_K_XL;
rail:mesh-measurements`): `/status` → `HTTP/1.1 200 OK`, `/v1/models` →
`HTTP/1.1 200 OK` (grant-scoped), `GET /v1/rail/log` → `HTTP/1.1 404 Not Found`
(`content-length: 0`). The 404 is explained, not hand-waved: `ps -o lstart -p 1971`
= `Thu Sep 17 09:40:56 2026`, and `/proc/1971/exe` reads `(deleted)` — the
process predates `cc8a4c3ee rr-2-guest-door` (2026-09-19 14:38:46), the commit
that added `Guest` to `ClientSurface::serves_rail_routes`. A native control on the
same grant (`svrn mesh use`, exercised on both the full dial string and a
relay-only one) verified `/v1/models` over the tunnel, so relay and admission are
not in question. Per the shared protocol the deployed daemon was never restarted;
the current-binary throwaway node above is what returned 200.

**Concurrent foreign product work — named so the diff is not misread.** The tree
held uncommitted foreign edits under `sovereign-cli-llm/` (`mesh_media.rs`,
`ring_cmd/mod.rs`, new `mesh_media/origin.rs`, `ring_cmd/serve.rs`) recorded by the
inventory row as a concurrent peer's. Mid-session that peer committed them to
`main`: `5825a2fa3 cli: svrn ring serve + svrn mesh media origin — the room's last
two config edits become verbs` and `7fef0a9dc runbook: 100% cli`. So
`git diff --stat <BASE>..HEAD -- sovereign/crates commonwealth/crates` now shows
those two commits' +678 lines. They are not this row's and not this campaign's;
this row's own change over those paths is zero.

**Clause (d):** `git diff --stat -- sovereign/crates commonwealth/crates` for this
row's own commits is empty. Committed artifacts are this ledger entry (plus its
render) and the queue mark.

</details>
