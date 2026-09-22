# ring-runtime — the guest page an iroh link opens

The browser half of the ring guest story. A person with a plain mobile browser
— no app, no key, no shared network with the wall — scans a guest QR, the page
loads from one static origin, and it **dials the wall directly over iroh's
relay**, speaking the rail on the `GUEST_ALPN` the daemon already accepts. The
relay forwards ciphertext; nothing proxies HTTP and no TLS is terminated for
anyone.

This is the "browser relay-only" half of `docs/RING_APP_LIBRARY.md` ("The page
is an iroh endpoint") and the courier shape of `docs/THE_LINK.md`. It was
measured WORKED at the pinned iroh before it was a product:
`ralph/DECISIONS.md` browser-dial-2 (every layer answered; a live node returned
`HTTP/1.1 200 OK` carrying a real rail row).

## Build

```bash
scripts/build-ring-runtime.sh
```

Needs `wasm-bindgen` on PATH (`cargo install wasm-bindgen-cli --version
0.2.128`) and the `wasm32-unknown-unknown` target. The script writes a
deployable directory to `target/ring-runtime/site/`.

## Deploy

Upload `target/ring-runtime/site/` to the guest runtime's static origin —
today `svrnme.sh`, beside the landing page and installers (`THE_LINK.md`). It
is a file on commodity static hosting: no server, no certificate to manage, no
data, and no traffic after first load. Browser storage and passkeys are
per-origin, so a mirror is a *different* place to a guest's browser, not a
copy — pick one origin and keep it.

## Point a link at it

```bash
svrn mesh grant --model <id> --wall --ttl 2h \
  --url https://svrnme.sh/ --qr-svg wall-qr.svg
```

The QR now carries `iroh=<dial string>` in the fragment beside the token, so
the page dials the wall wherever it is. Same-network guests are unaffected:
the wall's own door still serves the page over HTTP, and a grant without a
dial string is byte-identical to what it was.

## Why this exists as a separate crate

It only builds for wasm32 and only runs in a browser, so it stays out of the
repo's Cargo workspace (`[workspace]` in its `Cargo.toml`) and out of the
daemon: the daemon's side of this is the `GUEST_ALPN` listener it already
serves.
