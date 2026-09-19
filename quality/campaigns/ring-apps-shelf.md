# ring-apps — the shelf

What we pull in from the open-source world, per primitive row of
`quality/campaigns/ring-apps.toml`, so the campaign builds the ECOSYSTEM and
not CRDTs, blob stores or codecs from scratch (operator direction
2026-09-17). Three read-only research passes on 2026-09-17, every version and
date checked live against crates.io, npm, GitHub and primary sources on that
day; anything older than that is stale by definition and should be re-checked
before an order cites it. Licence filter: this repo is AGPL-3.0, so MIT /
Apache / BSD / MPL / GPL-2.0-or-later / LGPL are in; GPL-2.0-ONLY, FSL,
proprietary and "no licence file" are out.

Verdicts: **adopt** (take it as is), **adapt** (take the format, the
algorithm or a slice; not the dependency), **avoid** (with the reason, so it
is not re-proposed).

## Three findings that change the architecture, not just a library choice

**1. The page is never the node.** A browser cannot be a peer, permanently:
iroh in wasm is relay-only ("all connections from browsers ... flow via a
relay server"; hole-punching cannot be ported), the W3C p2p-webtransport
track was archived read-only on 2026-06-23, PWAs have no UDP, iroh-gossip has
no wasm target in CI, and iroh-blobs in wasm is MemStore-only. So "page over
rails" means: a native shell (Tauri 2) holds the iroh endpoint, the journal,
the blob store and the gossip lane, and the static page talks to it over the
local bridge. A browser-only reader gets a relay-connected, read-mostly view.
This must be stated in the architecture doc, or the independence claim is
false on every phone and in every tab.

**2. The deploy verb largely exists.** `sovereign meshapp publish|install|dev|
list`, a curated `meshapp-registry.toml` with sha256 integrity, `_sdk/` JS and
`catalog.json` are in the tree. The registry's own comment names the gap
("cryptographic signing (ed25519) is the next step"), and `_sdk/bridge.js` is
read-only and corpus-bound (no rail append, no roster, no subscribe). ra-10 is
therefore: sign the registry entry, move distribution from a URL to
rail-gossiped digests, widen the bridge. Not a new subsystem. (ARCH 11.)

**3. Bump iroh and collapse the dalek split before building on any protocol
crate.** We pin iroh 1.0.2; current is 1.2.0 (2026-09-09). iroh-gossip 0.101 /
iroh-blobs 0.103 / iroh-docs 0.101 are the 1.x-compatible releases (all
2026-06-15) and p2panda-net wants `^1.0.3`. The lock carries two
`ed25519-dalek` majors (ours 2.2.0, iroh's 3.0.0-rc.0; stable 3.0.0 shipped
2026-07-06). iroh's `PublicKey` is a 32-byte Ed25519 key, so the roster key
can BE the node id once the split is gone. This is rung ra-23, first on the
ladder after the in-flight pair.

## L1 bytes

### journal — landed (the rail). Shelf: nothing to pull; two schemas to borrow.

| Borrow | What | Why |
|---|---|---|
| p2panda-core (MIT/Apache, 0.7.1 2026-08-21) | Header+Body, per-author backlink + seq, Ed25519, PruneFlag | structurally our rail already on iroh; READ its LogSync for the digest sync, do not adopt (would re-derive act ids — a wire break) |
| in-toto/DSSE statement shape (Apache-2.0) | `subject` digest, `predicateType`, `predicate` | the envelope for a signed package act; sigstore itself is avoided (Fulcio/Rekor = OIDC + public log; our roster is the identity and the rail is the log) |

### blob — open. Shelf: iroh-blobs with a fork budget; bao-tree as the named escape hatch.

| Candidate | Licence | Version | Verdict | Why |
|---|---|---|---|---|
| iroh-blobs | MIT/Apache | 0.103.0, 2026-06-15 | **adopt, fork budget** | bao-encoded verified range streams in 1024-byte chunks, HashSeq, downloader with `ContentDiscovery`, `Observe`, provider `AbortReason` as the members-only gate. README still says "not production quality"; no commit on main since 2026-06-15; open: #252 FsStore::load hang, #247 GC deletes wanted blobs, #256 HashSeq children swept, #254 progress lost on SIGKILL, #260 teardown deadlock, #261 redb corruption. Every one is a keeper failure mode. Treat as vendored. |
| bao-tree | MIT/Apache | 0.16.1, 2026-08-26 | **adopt** | standalone, released AFTER iroh-blobs went quiet; the fallback is bao-tree over raw iroh QUIC streams, and we write only the request framing |
| blake3 | CC0/Apache | 1.8.7 (lock 1.8.5) | adopt | |
| Kubo/IPFS | MIT+Apache | v0.43.1 | avoid | wrong shape (global DHT daemon) |
| Hypercore/Hyperblobs | MIT/Apache | 11.36.1 | avoid | JS-only parallel ecosystem; mine Pear's design (below) |
| Willow / iroh-willow | Apache | willow-rs ARCHIVED 2025-10-23 | avoid | dormant |

Arithmetic to carry: IROH_BLOCK_SIZE 16 KiB, outboard ≈ size/256 (0.39%); a
verified random-range audit of a 1 GB share ≈ 1 KiB proof + one 16 KiB block.

### gossip — open. Shelf: iroh-gossip behind a trait; direct fan-out is the default at ring scale.

| Candidate | Licence | Version | Verdict | Why |
|---|---|---|---|---|
| iroh-gossip | MIT/Apache | 0.101.0, 2026-06-15 | **adapt, eyes open** | HyParView+Plumtree on an existing Endpoint, persists nothing, `proto` module usable sans-iroh. No functional commit since 2026-06-15, 38 open issues; #86 (open since 2025-08-01) is a timing-dependent partition with THREE peers — our first-app topology; #131 oversize publish FAILS SILENTLY against a 4096-byte default (ARCH 6 violation we would inherit). Raise `max_message_size`, guard size ourselves. |
| direct fan-out | — | — | **default impl** | HyParView's defaults are 10,000-node PeerSim parameters; at n≤10 every pair already holds an authenticated connection. Fan out to N-1 with an `(origin, seq)` dedup set. Gossip buys membership/healing the connections already bought. |
| libp2p-gossipsub | MIT | 0.50.0 | avoid | drags libp2p-swarm, which owns dialing |
| foca (SWIM) | MPL-2.0 | 2.0.0 | adapt, membership only | sans-io |
| chitchat, memberlist, zenoh, async-nats, sile plumtree/hyparview (2018) | mixed | | avoid | converging KV / owns transport / server / seven years stale |

Fact that shapes the doc: y-protocols awareness `outdatedTimeout` is 30 s, so
presence CANNOT ride the 60 s digest sync. The live lane is not optional.

### stream — open. Shelf: iroh gives it; the browser never gets it.

Raw QUIC stream / datagrams over iroh from the native shell. WebRTC data
channels are the only browser-to-browser transport and need signalling plus
TURN (a server by a longer road). An iroh WebRTC transport exists as one
external crate; not official.

## L2 substrate

### identity — partial. Shelf: frost-ed25519, age, a SEPARATE X25519 roster key, WebAuthn PRF.

| Candidate | Licence | Version | Verdict | Why |
|---|---|---|---|---|
| frost-ed25519 | MIT/Apache | 3.0.0, 2026-04-23 | **adopt** | NCC-audited 2023; a ring acting as one produces a signature verifying against ONE ordinary Ed25519 key, so the roster entry never changes. Interactive: fine for a deliberate vouch, wrong for 2am recovery. |
| age / rage | MIT/Apache | 0.12.1, 2026-07-14 | **adopt** | multi-recipient wrap, unknown stanzas ignored; `maintenance = experimental` in manifest despite 4.7M downloads |
| vsss-rs | MIT/Apache | 6.0.1 | adapt | "currently under audit ... use at your own risk" |
| sharks | MIT/Apache | 0.5.0, 2021 | **avoid** | RUSTSEC-2024-0398, no patched version |
| webauthn-rs | MPL-2.0 | 0.5.5 | adapt, thin slice | server lib |
| fast_qr / rqrr | MIT / MIT-Apache-ISC | 0.14.0 / 0.11.0, 2026-09 | adopt | `qrcode` stale since 2024-07 |
| ed25519-dalek / x25519-dalek / curve25519-dalek | BSD-3 | 3.0.0 / 3.0.0 / 5.0.0, 2026-07-06 | adopt | the unification target for finding 3 |

Design decisions the shelf forces:
- **Recovery is a rail act, not a crypto primitive.** Keybase, Argent (now
  Ready), Safe and Apple all converge on: a quorum of people with standing
  vouch a NEW key; nobody reassembles the old one. Add Argent's cancellable
  delay window and Apple's contact-issues-a-code framing. This is ra-5 as
  already written.
- **Passkeys unlock, never sign.** Platform authenticators are ES256, sign
  `authenticatorData||SHA256(clientData)`, non-exportable. Use WebAuthn PRF
  (Chrome 147, Firefox 148, Safari 18, iOS keychain) to unwrap the local
  Ed25519 key; `largeBlob` is NOT usable on Android. PRF is credential-bound,
  so a lost device loses the wrap — which is why recovery-by-vouch is the
  backstop.
- **Do not reuse the signing key for encryption.** `to_montgomery()` works and
  the dalek docs say not to; and if the roster key is also the encryption key,
  rotating a compromised encryption key rotates the IDENTITY. Carry a separate
  X25519 key on the roster row, introduced by a signed act. Avoid `crypto_box`
  (18 months on a prerelease).
- **QR invites: Signal's direction, iroh-tickets' encoding.** An iroh ticket
  is reusable by anyone and leaks the IP. The JOINER generates the ephemeral
  key and puts the public half in the QR; the inviter scans and replies
  encrypted to it; a photographed QR is worthless after expiry. Encode with
  iroh-tickets' kind-prefix + base32 postcard convention.

### membership — open. Shelf: Nostr's repost kinds as the Carry schema.

Carry (ra-2) adopts NIP-18 kind 6 / kind 16 (generic repost carrying `k` for
the reposted kind) as the payload shape — we would invent this badly. Keep the
rail's Ed25519 signature; Nostr's `id`/`sig` (BIP-340 over secp256k1) are NOT
adopted. Nostr events carry no backlink or seq and `created_at` is
self-asserted — which is exactly WHY the schema drops onto a total order
without a fight.

### doc — open. Shelf: Yjs in the page, yrs in the node, Tiptap 3, y-protocols awareness. No adapter framework.

| Candidate | Licence | Version | Verdict | Why |
|---|---|---|---|---|
| Yjs / yrs | MIT | 13.6.32 (2026-08-04) / 0.28.0 (2026-09-17) | **adopt** | 300 KB JS, NO wasm (Automerge 3.64 MB, Loro 3.24 MB wasm). Updates are commutative, associative, idempotent: our total order is a convenience, not a dependency; `mergeUpdates` compacts without loading the doc. Measured: one keystroke 18–19 B; nine batched 27 B; 4-paragraph invite 375 B full state; ~38,000 words fit ONE 64 KiB act. |
| Loro | MIT | 1.16.1, 2026-09-10 | adapt, engine 2 | `shallow-snapshot` truly trims history (273 KB full → 63 KB shallow on 104,852 chars); `EphemeralStore` exists symmetrically in Rust. The engine to move to when history size bites. |
| Automerge | MIT | 3.5.0, 2026-09-16 | adapt (architecture), avoid (engine) | its StorageAdapter (`[docId,"incremental",hash]` → `[docId,"snapshot",hash]`) IS our journal + compaction described by someone else; but sync is point-to-point, the broadcast adapter is "for experimentation", `automerge-repo` latest is `2.6.0-alpha.3`, and `@automerge/prosemirror` has 0 commits in 6 months |
| Diamond Types | ISC | 1.0.0, 2022 | avoid | plain text only, crate "quite out of date" |
| Peritext | research | — | n/a | the algorithm INSIDE Automerge's and Loro's rich text |
| Tiptap 3 core + `@tiptap/y-tiptap` | MIT (LICENSE read) | 3.31.3 / 3.0.9, 2026-09 | **adopt** | the paid thing is Tiptap Collaboration, a hosted server we do not need; `prosemirror-tables` 1.8.5 is CRDT-bound for free |
| y-protocols awareness | MIT | 1.0.7 | **adopt** | two pure functions over `Uint8Array`, imports only lib0+yjs; presence 49 B, name+colour+cursor 235 B |
| y-prosemirror | MIT | 1.3.7, 2025-07 | adapt | npm 14 months behind a `2.0.0-11` repo; y-tiptap wraps it |
| y-codemirror.next | MIT | 0.3.6 | adapt | plain text |
| BlockNote | MPL-2.0 core; `xl-*` GPL-3.0 OR proprietary | 0.54.2 | adapt | React-only |
| Milkdown, Lexical+@lexical/yjs | MIT | 7.22.1 / 0.50.0 | adapt | markdown-first / needs a Provider shim |
| `@automerge/prosemirror`, `@automerge/automerge-codemirror`, `y-sync` (rust, 2023) | | | avoid | 0 commits / 451 weekly dl / superseded by `yrs::sync` |
| y-websocket, y-webrtc (2023), y-libp2p (2021), automerge-over-iroh (none mature), loro-dev/iroh-loro (21 stars) | | | avoid | server / stale / PoC |
| iroh-docs | MIT/Apache | 0.101.0 | avoid | a SECOND durable signed log next to the rail; read its range-based set reconciliation paper if the 60 s digest ever needs improving |
| TinyBase | MIT | 9.7.1, 2026-09-11 | adopt, scoped, app 2 | `createCustomSynchronizer`/`createCustomPersister` are documented seams (`send`→append act, `registerReceive`→digest tick); but `MergeableStore` is per-cell LWW, NOT a sequence CRDT — boards and lists, never prose |
| Jazz, ElectricSQL, Zero, PowerSync (service FSL), DXOS (FSL), Ditto (proprietary), Replicache (archived 2026-06-10), Triplit (dormant), Liveblocks (SaaS), Fireproof (licence contradictory) | | | avoid | require a server, or licence |

The durable adapter is ~50 lines, not a framework: `doc.on('update', bytes =>
appendAct(bytes))`; replay = `applyUpdate` per act in any order; compaction =
a snapshot act via `encodeStateAsUpdate`, minted node-side by yrs.
Calendar/kanban/spreadsheet bound to a CRDT: nothing exists in the open (the
one Yjs spreadsheet is $3,500); `prosemirror-tables` covers "who's bringing
what".

**The one piece of code the shelf does not supply:** the rail proves which key
appended an act; the Yjs `clientID` inside it is self-asserted, and y-protocols'
own spec says awareness is unauthenticated. Something must bind `clientID` to a
roster key at act validation AND at gossip ingress. That is ours, and it is the
doc rung's first negative leg.

### store — open. Shelf: age's payload format; content-address the ciphertext payload ONLY; Reed–Solomon on keepers; Tahoe's leases.

| Stage | Choice | Why |
|---|---|---|
| encrypt on device | age STREAM payload: ChaCha20-Poly1305, 64 KiB chunks, nonce = chunk counter + last flag, seekable by chunk (`StreamReader: Seek`); or `chacha20poly1305` 0.11.0 + `aead` `StreamBE32` for no beta dep | seekable counter mode is MANDATORY — any chained mode silently kills range requests and with them the library and the atlas |
| address | BLAKE3 over the ciphertext payload only; the age HEADER (X25519 stanzas, ~130 B each, 25 members ≈ 3.3 KiB) lives in a RAIL ACT | a member joining must not change every blob's hash and invalidate every share; the act cap and age's chunk size are both 64 KiB |
| dedup | Tahoe's per-grid convergence secret mixed into the key | convergent encryption without it is a hash oracle to the world |
| code | `reed-solomon-simd` 3.1.0 (MIT+BSD-3; Leopard GF(2^16), 1–32768 shards, 10 GiB/s) on KEEPERS, systematic | the phone never encodes: it ships ciphertext, the keeper cuts shards and never sees plaintext. Systematic ⇒ data shard i IS byte range [iS,(i+1)S) of the blob, auditable against the one root hash; only parity shards need their own hashes. wasm32 has no CI evidence — verify if ever needed. |
| avoid | `reed-solomon-erasure` ("looking for new maintainers", 2022); `rusty_erasure` (621 downloads, unverifiable claims); novelpoly (non-systematic) | |
| audit | one verified random range request per probe (~17 KiB) | the Proof of Retrievability Tahoe specified and never built; cheaper than Storj's because our shares are individually hash-addressed |
| leases | Tahoe-LAFS lease model as recurring signed acts: fixed duration, renewed weekly, a share dies only when ALL leases expire; `servers_of_happiness` = "the smallest number of servers you expect online" — our k | expiry derivable from the log, no GC daemon. Tahoe is GPL-2+/TGPPL Python: cite the design, take no code |
| gateway | one LAN HTTP endpoint `hash + Range → 206`, never 200 | no sink speaks iroh; MapLibre Native OOMs on a 200 to a range request |

### play — open. Shelf: remux not transcode; hls.js byterange; Jellyfin is closed to us.

| Candidate | Licence | Version | Verdict | Why |
|---|---|---|---|---|
| Jellyfin | **GPL-2.0-ONLY** (bare GPLv2, no or-later) | | **avoid** | cannot be vendored, forked or linked into AGPL-3.0; API interop at arm's length only. Clients Streamyfin (MPL) and Findroid (GPLv3) are fine. |
| gstreamer-rs | bindings MIT/Apache, core LGPL-2.1+ dyn-linked | 0.25.3, 2026-06-29 | adopt for stream-copy REMUX on the keeper | touches no codec library, so the GPL codec question never arises |
| Symphonia | MPL-2.0 | 0.6.1, 2026-08-13 | adopt as keyframe INDEXER | demuxes MP4/MKV; no muxer |
| hls.js | Apache-2.0 | 1.7.1, 2026-09-16 | adopt | `EXT-X-BYTERANGE`: a computed `.m3u8` points every "segment" at a byte range inside the ONE existing file — bytes and hash unchanged. Do not layer AES-128 on top (CBC alignment). |
| casting | Cast Default Media Receiver (no registration); DLNA via `rupnp` 3.0.0 (MIT/Apache); AirPlay free in Safari via `x-webkit-airplay` | | adopt | floor is a browser on an HDMI stick. Unverified: Safari MKV; whether Cast accepts plain `http://` LAN URLs |

MKV is the enemy, not the transport: no Chrome `<video>` support, not on
Chromecast's list (Firefox 145 added it). HEVC in Chrome is GPU-only.
Album: `image` 0.25.10 thumbnails; transcode HEIC to JPEG/WebP at ingest
(Chrome delegates HEIC to the OS, Linux ships no HEVC decoder, Firefox never).

### realtime — open. Shelf: GGRS over iroh datagrams; Galène on a keeper, called a server.

| Candidate | Licence | Version | Verdict | Why |
|---|---|---|---|---|
| GGRS | MIT/Apache | 0.13.0, 2026-06-26 | **adopt** | rollback netcode, transport-agnostic: one `NonBlockingSocket` over iroh datagrams, and iroh's public-key dialling IS the signalling — no server. The single highest-value item in this layer. |
| lightyear | MIT/Apache | 0.30.1 | adapt | the client-server alternative |
| matchbox | MIT/Apache | | avoid | exists to get WebRTC through a signalling server iroh already replaces |
| boardgame.io | MIT | v0.50.2, **2022-11** | avoid dep, copy two ideas | the move-reducer contract `(G, ctx, args) => G` + INVALID_MOVE, and the turn/phase machine, ~200 lines; the journal has NO hidden state, so cards/bidding/fog need commit-reveal acts or a keeper filtering views |
| Galène | MIT | 1.2.1, 2026-09-08 | adopt for calls | single Go binary, modest resources — a keeper can run it. **It is a server**: calls stop when the keeper does and the keeper sees call metadata. Say "keeper-hosted", never "peer-to-peer". |
| LiveKit / mediasoup / Janus / Jitsi | Apache / ISC / GPL-3 / Apache | | avoid | 4–8 cores / a library not a server / operational surface |
| mesh WebRTC | | | audio-only ≤5–8 | video mesh collapses past 4 (9 participants ≈ 70% CPU at min resolution) |

### discovery — open (ra-4 funded). Shelf: Nostr NIP-51 sets as the curation layer; Blossom for blobs.

NIP-51 lists/sets are what the vouch chain drives ("follow" = filter by a
curator's set). NIP-96 is deprecated upstream in favour of NIP-B7/Blossom
(sha256-addressed blobs) — forced on us anyway by the 64 KiB act cap.

### availability — open. Shelf: nothing to pull; Tahoe's `servers_of_happiness` and lease model (above).

### guarantees — open. Shelf: nothing to pull; `kernel_types::Judgement` rows in-repo. THREAT_MODEL.md is the prose to cite from.

### commons — open. Shelf: ValueFlows vocabulary; Tahoe leases; nothing else exists.

| Candidate | Licence | Verdict | Why |
|---|---|---|---|
| ValueFlows | CC-BY-SA-4.0 (vocabulary, pushed 2026-08-31) | **adopt the vocabulary** | Agent / Economic Event / Resource / Process — do not invent nouns; hREA (Apache-2.0, Rust, Holochain-bound) proves it implements |
| Ostrom / SustainOSS working group | CC-BY prose | design citation only | no code artifact exists anywhere |
| Filecoin PoRep/PoSt | | **avoid** | sealing is tens of minutes per sector + a SNARK + chain deadlines, built for adversarial strangers with stake; a ring is 25 vouched people and a signed audit act with loss of standing is the whole mechanism |
| Cyclos (GPL-2.0-only / proprietary), Circles (blockchain), fiatjaf/trustlines (2019) | | avoid | licence-incompatible / chain / dead |

### compute — open. Shelf: nothing to pull for v1 (search and inference are in-repo hubs).

## L3 deployment — open. Shelf: Tauri 2 + uniffi; DSSE envelope; Pear's mechanics; no package format exists.

| Candidate | Licence | Version | Verdict | Why |
|---|---|---|---|---|
| Tauri 2 | Apache/MIT | 2.11.5, 2026-07-01 (3.0.0-alpha.1 2026-09-15 — do not start there) | **adopt** | the only shell where "static page + Rust node in one process" is the native shape |
| uniffi | MPL-2.0 | 0.32.1, 2026-09-09 | adopt for native edges | iroh ships first-party Swift/Kotlin bindings via uniffi (`computer.iroh:iroh` 1.0.0 on Maven) |
| Capacitor, Dioxus, Flutter+frb, React Native | | | avoid | all of Tauri's work plus a hand FFI / re-authors the UI, killing the static-page premise |
| Pear runtime (Holepunch) | Apache-2.0 | pushed 2026-09-17 | avoid dep, **mine the design** | `pear stage <channel>` appends files to an append-only log keyed by channel+package; version = `<fork>.<length>.<key>`; `pear release` MOVES A POINTER (rollback = a correction, not a delete — our model exactly); production release is multisig (FROST collapses that to one Ed25519 sig); peers seed by running the app; clients fetch only the blocks they need |
| Web Bundles / SXG | | wpack concluded; Chrome removed navigation 2023; GTS stops SXG certs ~2026-09-30 | **avoid** | both retiring. There is NO off-the-shelf signed static-site package; reuse the DSSE statement shape as the payload of a signed rail act |
| OCI artifacts | | | avoid | a registry is a server |

Phone constraints to budget: Android foreground service (iroh's Kotlin docs:
shutdown/persist on background, re-bind on foreground); iOS = the node is
OFFLINE while backgrounded unless Apple grants a Network Extension
entitlement; push is a third-party Tauri plugin (#11651), not core.
iroh-inside-Tauri-mobile has no primary-source reference: unproven.

## Per app — the stacks

| App (rung) | Take | Build | Deciding fact |
|---|---|---|---|
| doc (ra-13) | Yjs + yrs, Tiptap 3 + y-tiptap, prosemirror-tables, y-protocols awareness; Loro as engine 2 | ~50-line update→act adapter; snapshot act; clientID→roster binding | no wasm; commutative updates; 38k words per act |
| feed (ra-19) | Nostr kinds 0/1/6/16/7/30023 + NIP-51 as act payloads, Blossom for photos, `nostr` crate types, `nostr-relay-builder` façade at `ws://127.0.0.1`, Coracle (MIT, Svelte) UNMODIFIED | the rail→relay index (author/kind/tag/time filters) | Nostr clients couple only to a 7-message WS surface; the schema assumes no order |
| chat (in feed / ra-6) | MDK/OpenMLS for forward-secret groups, NIP-44 message format, chatscope or ~400 lines Svelte | nothing on transport — it is the rail + live lane | MLS gives add/remove + post-compromise recovery, the one thing a signed journal lacks. Matrix avoided: a room is a DAG with state-resolution v2, a partial-order merge we do not need. MDK's licence and cadence are UNVERIFIED. |
| studio (ra-15) | Tone.js pinned to an explicit `next` (npm `latest` 15.1.22 is 17 months stale), native Web Audio nodes only, wasm Opus at 96 kbps stereo, `.sb3`-shaped project act (manifest + assets by content hash), DAWproject (MIT) write-only export | the grid act `(bpm, bars, time_signature, root_key)` | GRID NOT CLOCK: Endlesss delays each player by one loop so latency is musically irrelevant; a 2-bar loop at 120 BPM @96 kbps is 46.9 KiB — rides the act; longer → blob ref. No SharedArrayBuffer (COOP/COEP); AudioWorklet distorts on cheap Android. WAM avoided (no licence file). |
| call + arcade (ra-16) | GGRS over iroh datagrams; Galène on a keeper; boardgame.io's reducer + phase machine copied | commit-reveal for hidden state | browser cannot be a peer; SFU is a server and is named as one |
| library (ra-18) | LAN gateway + `<video>` on faststart MP4/H.264; hls.js byterange playlist; gstreamer-rs remux; Symphonia indexer; Cast DMR + HDMI browser floor | the gateway, the keeper hot-copy policy | Jellyfin GPL-2.0-only closes reuse; remux keeps us out of GPL codecs |
| atlas (ra-21) | PMTiles (CC0 spec; `pmtiles` 0.24.0 MIT/Apache `AsyncBackend{read(offset,len)}` over iroh, no fork; MapLibre Native BSD-2 / GL JS 6.10.0); `pmtiles extract --bbox` (measured: Berlin z0–15 in 11.9 s, 61 requests, 80 MB); `zim` crate 0.5.0 (Apache/MIT, n0 author) with a byte-source trait; libzim is GPL-2.0-OR-LATER (GitHub label wrong) | a ~235 KB sparse pointer sidecar for ZIM; an offline geocoder (`osmium tags-filter` + tantivy) since none exists | ZIM assumes local disk: cold article ≈ 50 round trips over a 241 MB pointer array (measured 0.48–1.51 s/probe over the internet ≈ 25 s/article; fine at LAN RTT, unusable over a relay); kiwix-js refused remote ranges (#659). Start at `top_maxi` 7.8 G, not `all_maxi` 119 G. Native app, not browser. `© OpenStreetMap` is required (ODbL). |
| utilities (ra-22) | in-repo: `svrn ring new` expenses template, claims store | | |

## The risks, ranked across all three passes

1. **The browser is never a peer** (finding 1). Mitigation: the shell holds the node; state it in the architecture.
2. **iroh-blobs and iroh-gossip are both unattended since 2026-06-15**, 0.x against iroh 1.2, with open bugs on exactly our topology and our keeper. Mitigation: vendored-fork posture; bao-tree + raw QUIC and direct fan-out as the named escape hatches; bump iroh first (ra-23).
3. **Self-asserted ids inside opaque acts** (Yjs clientID, awareness payloads, Nostr `pubkey` field). Mitigation: bind to the roster key at validation and ingress; this is the doc rung's first negative leg and the feed's too.
4. **ZIM over ranges is an RTT problem** bandwidth cannot fix. Mitigation: the sparse sidecar is a feature with a pre-registered p50/p99 cold-article bar.
5. **Thin, pre-1.0, or licence-ambiguous pins**: p2panda (aquadoggo archived, 1k–10k downloads), MDK (licence unverified), `age` (`experimental`), Fireproof (three licences). Mitigation: none of these is load-bearing in the recommended stacks except MDK for chat, which is verified before ra-6's order is drafted.

## Verification gaps stated by the workers

MDK/OpenMLS licence and cadence; iroh-inside-Tauri-mobile; Safari MKV; Cast
receiver on plain-http LAN URLs; `reed-solomon-simd` on wasm32; one worker's
WebSearch budget ran out at 200 calls so a few secondary items rest on one
source. Re-verify before an order cites any of them.
