# Rings — the inventory

> **The design moved to its own repository.** svrngs — the product this work
> became — stands apart from commonwealth-ai by operator direction of
> 2026-10-03, and its design is `docs/DESIGN.md` in the svrngs repository
> (`~/dev/svrngs`). svrngs links no crate from this repository; it forks the
> ring slice from here and owns it (operator, 2026-10-03, over shipping
> `cw-rails` as a sidecar) — `commonwealth-rail-core` first, as svrngs'
> `svrngs-core` at `e1a43bcd2`. What remains below is the inventory of the ring code in
> commonwealth-ai as of 2026-10-02 — the rail, the ring routes and sync, the
> phone shell, ring-doc and ring-runtime, `ring_cmd` and `mesh_media` — which
> svrngs replaces and which retires as it does. The design record — rationale,
> prior art and history — stays here in `docs/RING_APP_LIBRARY.md`.

## Retirement list

The strategy toward this repository is svrngs' `docs/DESIGN.md` §4, "The
fork" (operator, 2026-10-03). Ring code here is forked by svrngs and then
deleted, each item when the svrngs unit or demo replacing it passes (svrngs
DESIGN §8); the deleting commit names it and its measurement. Nothing here
retires on a date or a promise.

| Retires | Replaced in svrngs by | When this svrngs bar passes |
|---|---|---|
| `commonwealth/crates/commonwealth-rails/src/ring_sync` and `ring_routes` — group membership, sync and routes | `svrngs-node`, forked (U1) | U1: a node in the group but no mesh syncs; a node outside it is refused |
| `sovereign/crates/sovereign-cli-mesh/src/ring_cmd` | the `svrngs` command (U2, U3) | Demo A, two computers, one group |
| `sovereign/apps/ring-runtime` and `sovereign/crates/sovereign-contracts/src/guest_pages/ring_shim.js` | the browser runtime (U5) | Demo B, split the pizza |
| `sovereign/apps/ring-doc` | the Doc reference app (live collaboration) | that class's demo |
| the ring use of `sovereign/crates/sovereign-cli-mesh/src/mesh_media` — lending a library to a group; its use within one owner's mesh stays | Films, on Jellyfin (lent services) | that class's demo |
| the ring shell in `sovereign-mobile` | svrngs' phone app (waiting in its DESIGN §9) | its trigger, then its demo |
| the campaigns `ring-apps`, `ring-doc`, `ring-room` and `ring-guest` | svrngs' demos | the svrngs demo replacing each demo |

**Stays — the mesh.** The rail the work plane folds over (`commonwealth-rail-core`,
`commonwealth-rail`), the work plane and its donor loop, inference routing and
mesh membership solve one owner's machines, not a group of people, and may
diverge from svrngs freely.

**Forked, not shared.** `commonwealth-rail-core` stays here for the work plane,
and the door that forwards with a verified identity
(`commonwealth/crates/commonwealth-transport/src/iroh_identity_forward.rs`) stays for
the mesh; svrngs holds its own copies. No wire joins the two systems, so the
copies diverge freely. A fix to either is checked against svrngs'
`scripts/ports.py`, which lists every forked file beside its original here.

## The inventory (2026-10-02)

> **Superseded where svrngs' design differs; kept for status.** Every status below was
> checked against HEAD `571a9896b` on 2026-10-02: **built** runs today,
> **partial** runs with a named gap, **unbuilt** is not in code. Its register
> (§3.1), deeds and keepers (§3.4) and seq cuts are replaced by svrngs' DESIGN §6
> and §9; its units (§5) and bars (§6) are re-derived in svrngs' §8;
> rungs, primitives and existing bars stay in `quality/campaigns/ring-apps.toml`.

## 1. The model

A ring is a town of about twenty-five people who know each other's names. It
has a **register** (who is in), **civic books** (state the group must not lose
to one member leaving: the register, the treasury, the deeds) and **apps**
(whatever members offer each other, each run by its **host** on the host's own
machine — a laptop or a homelab server alike). The town is open while hosts are
awake and closed when nobody is. Members arrive from the chat they already use
through a **bridge**, or from a ring link, and walk into any app with their
identity. A **keeper** holds copies of what it does not author — the journal,
blobs, an insured app's state — so an app someone would miss survives its host
sleeping or leaving. The model is the town's clerk: it carries chores and never
orders, decides, scores, or makes a gesture in anyone's name.

## 2. Invariants

Each is held by code, not by review (ARCH principle 10). A unit that breaks one
is wrong whatever else it passes.

| # | Invariant | Held by | Status |
|---|---|---|---|
| I1 | Order and void are decided once, in `admit` (`commonwealth-rail-core/src/admit.rs:438`); nothing above re-derives them | the rail | built |
| I2 | Membership is one function of a seed plus `Admit`/`Remove`; every reach check asks it | `membership.rs:78-166` | partial: reach asks mesh membership (§3.9) |
| I3 | Civic-book state is a pure fold of admitted ops; `op.ts_unix` is the only clock | the five laws (LIB §4) | unbuilt as a library |
| I4 | An app has one writer at a time: the host the deeds name | the door's write check (U12) | unbuilt |
| I5 | No surface returns a value about a member (stance 7) | gate U17 | unbuilt; subjects exist (D9) |
| I6 | A gesture act carries a person's confirmation (stance 9) | gate U18 | unbuilt |
| I7 | An act from a chat may render back into that chat; a ring-origin act reaches a chat only as a link; nothing crosses transports | the bridge's outbound check (U15) | unbuilt |
| I8 | Absence is reported: an asleep host is a row, a partial journal carries gaps | fan-out rows, `RailGap` | built |
| I9 | The log carries no heartbeats; liveness comes from claims | `commonwealth-media/src/apps.rs:131` | built |
| I10 | Peer-authored text renders as text | CSP + views as structure (U6) | partial (D3) |
| I11 | Shared infrastructure holds nothing members cannot rebuild, sees no plaintext, is swappable, and is proven by being turned off | LIB §17 | design |

## 3. Primitives — contracts and state

### 3.1 The register

- **Acts.** `Admit{person, key}` (`commonwealth-rail-core/src/lib.rs:346`) and
  `Remove{key, through_seq}` (`:364`), line version 2. Built.
- **Fold.** `membership(ops, seed)` → bindings, standing, counted, voided
  (`membership.rs:78-166`): a commutative void set, then one walk in
  `(ts_unix, actor, seq, id)` order. Built, with defect D1.
- **Seed.** A registered source, else `roster.json`, else the mesh
  (`commonwealth-rail/src/lib.rs:152-190`; the default at
  `commonwealth-rails/src/rail.rs:107-137`). Built. A register per ring that is
  not the mesh is unbuilt.
- **Writer.** None on purpose; the generic append doors accept membership acts
  (D2). The rule (LIB §9): any member admits and removes; recovery is by
  `Correct`; a removed key's later acts are gaps.
- **Views.** Every signed act now carries its author's view digest
  (`admit.rs:661-687`, stamped at `commonwealth-rail/src/journal.rs:257`).
  Nothing reads it beyond verification; fork detection from it is U3.

### 3.2 Civic books — the fold library

- An app record is `{initial, reduce(State, Payload, Op), pending(State) → [Effect],
  view(State, Ctx)}`. `reduce` and `pending` converge; `view` is local (LIB §3).
- The library — reducers `ledger`, `doc`, `poll`, `presence`, `rota`; wrappers
  `validated`, `once`, `upcast`, `byKind`, `combine`; `kind(name, schema, fn)`;
  `money` as integer cents; `diffFold`; the five laws as property tests (LIB
  §4–6) — is unbuilt. Built: one app-specific fold,
  `sovereign-cli-mesh/src/ring_cmd/templates/expenses.js:193`, with about twenty
  tests (`expenses.test.mjs:58-246`) run by `scaffold.rs:181`; byte-identical
  copies sit in `sovereign/apps/ring-doc/` and `my-doc/`.
- The page SDK `window.ring` is built (`sovereign-contracts/src/guest_pages/ring_shim.js:99-170`):
  `namespace`, `log`, `record`, `correct`, `live.send`, `live.drain`, `ask`,
  `fold`, `guest`. The runtime loop `ring.run(app, root)` (LIB §12) is unbuilt.
- Effects are data: `pending` returns `{id, want}`; a fulfilment is an ordinary
  act carrying that `id`, folded under `once` (LIB §5).
- The register and the deeds are civic books read by Rust (the door enforces
  them), so they are Rust folds beside `membership`, not library apps. The
  library is for civic books only the page reads.

### 3.3 Apps and the one manifest

- An app is any HTTP server reached by key. The caller arrives as
  `X-Mesh-Member`, `X-Mesh-Node`, `X-Mesh-Pubkey`, client-sent `x-mesh-*`
  stripped (`docs/PUBLISH_AN_APP.md`). Built. A static page served by the door
  is an app with no server.
- Publication is a claim with a TTL, renewed while the child lives (`svrn run`,
  `sovereign-cli-mesh/src/run_cmd.rs:342`, `:463`, `:481`; the in-memory store
  `commonwealth-media/src/apps.rs:131`). Built.
- **One manifest** for every app. The deployment primitive's manifest
  (`ring-apps` rung `ra-10`) and LIB §22's shop manifest are this file:

```toml
[app]
name   = "tools"
code   = "git+<url>#<rev>"     # or an image digest, a flake ref, a bundle hash
run    = ["python", "app.py"]  # omitted for a static page
writer = "host"                # or "ring": state is a civic book on the rail

[app.state]                    # writer = "host" only
path      = "data/"
snapshot  = ["sqlite3", "data/tools.db", ".backup $OUT/tools.db"]
restore   = ["cp", "$IN/tools.db", "data/tools.db"]
read_only = "methods"          # or "none"
```

  `code` makes an app insurable. Without `snapshot`, state is copied only at a
  handover, with the app stopped. `read_only = "methods"` lets a keeper's door
  serve `GET` and `HEAD` and refuse every other method. No manifest exists
  today: `svrn ring new` scaffolds files without one (`ring_cmd/scaffold.rs:26-33`),
  and nothing anywhere knows an app's state, code hash or snapshot.

### 3.4 Deeds and keepers

- **Acts**, folded as a civic book:

| Act | By | Effect |
|---|---|---|
| `Open{app, manifest, insurable}` | host | opens or updates an app; the latest per host wins |
| `Insure{app}`, `Withdraw{app}` | member | becomes or stops being a keeper of the app; valid only while it is insurable; the rail's order of `Insure` is the order of first refusal |
| `Handover{app, to, snapshot}` | host | names a keeper as the next host, with the hash of the final copy |
| `Release{app}` | host | gives the app up; opens first refusal |
| `Refusal{app}` | any member | opens first refusal on an app unreachable longer than the ring's period; any later act by the host closes it |
| `Take{app, snapshot}`, `Pass{app}` | keeper | accepts the app in its turn, or passes |

  A host's `Remove` opens first refusal on every app it hosts. Turns are
  computed from the `op.ts_unix` of the act that opened first refusal.
- **Behaviour.** Host awake: the host serves and ships copies to keepers on
  change. Host asleep: a keeper serves the last copy read-only, labelled with its
  age. Handover: no writes lost. First refusal: the first valid `Take` becomes
  host; with none the app is dormant and every keeper keeps the last copy.
- **One writer.** A node takes writes for an app only while the deeds name its
  key as host. No lease: renewing one would put heartbeats on the log (I9). The
  residual case — a host cut off from the ring taking local writes during a
  `Take` — is reported on rejoin, never merged.
- **Copies are blobs.** v0 ships a whole copy to each keeper; erasure coding is
  the blob primitive's (`ra-3`). The keeper role is `ra-7`'s: one role for the
  journal, blobs and insured apps.
- All unbuilt.

### 3.5 Door and sandbox

- The guest door adds CSP (`script-src 'self'`) to every response
  (`sovereign-daemon/src/guest_door.rs:388-409`, policy
  `sovereign-contracts/src/egress.rs:438-449`). Built. `serve_file`
  (`host-kit/src/shell/files.rs:38`) sets only Content-Type, so `svrn ring show`
  and `meshapp dev` serve without one (D3).
- Isolation is an opaque-origin `<iframe sandbox="allow-scripts">`, the runtime
  in the top frame, deterministic `Date` and `Math.random` injected (LIB §17).
  Unbuilt.

### 3.6 Joining and identity

- Today there are two links: the member `join_link`
  (`commonwealth-rails/src/api.rs:286`) and the guest link from `svrn mesh grant`
  (`sovereign-cli-mesh/src/mesh_guest.rs:352`), a short-lived bearer. No use
  counts, no `invite` verb (D4).
- Target (LIB §16): one invite — a secret with an expiry and a use count on the
  minting node; redeeming it writes one `Admit`. Tiers: guest (the host or bridge
  signs; the person is a name in the payload) and installed key — the ring app on
  a phone or the desktop app — each a key under the same person. A browser holds
  no key in anyone else's ring (LIB §15): it is a guest on the house WiFi and a
  read-only teaser elsewhere (`RING_ENTRY.md` "Phones").
- The browser crate `sovereign/apps/ring-runtime` (its own workspace; artefacts
  in `landing/ring/wasm/`) holds no rail-core and signs nothing, and stays so.
  The phone app is `sovereign-mobile` (Tauri 2), whose key is ephemeral today
  (`sovereign-mobile/src-tauri/src/iroh_bridge.rs:14`) and whose host,
  `sovereign-server`, was deleted.

### 3.7 The bridge

- One core and one adapter per transport. An adapter supplies join and leave
  events, inbound messages and commands, outbound posts with buttons, a stable
  speaker id, and an embedded surface where one exists.
- The bridge is a member a person admitted. At first it writes `Admit` and
  `Remove` for channel joins and leaves; later it projects the register onto the
  chat's role. Guest-grade acts carry `on_behalf_of` (in the signed body,
  `admit.rs:661-687`). It is retired by stopping, never by `Remove`, which would
  void everyone it admitted.
- Unbuilt: no Discord, Telegram or Nostr code or dependency exists.

### 3.8 Effects and `ask`

- A page can already call `window.ring.ask(q)` (`ring_shim.js:144`; refused under
  dev). The performer of `pending` effects is unbuilt. Target: `ask` is an
  effect; the member lending inference performs it; the fulfilment carries a
  `kernel_types::Answer` and the model's identity, so every node folds one
  answer. Only answers the ring keeps travel this way.

### 3.9 Reach

- Sync dials online mesh members and keeps those the roster names
  (`commonwealth-rails/src/ring_sync/journal.rs:153-164`; the filter at
  `ring_sync.rs:287`), so a roster key that is not a mesh member is never dialled.
  The acceptor's `member_check` (`commonwealth-rails/src/acceptor.rs:49-63`)
  admits any non-removed mesh member. Partial.
- Allow lists are per node and per kind: `app_allow`
  (`sovereign-contracts/src/setup_config_iroh.rs:165`, enforced
  `commonwealth-media/src/apps.rs:297-298`), media (`rails.toml [media] allow`,
  `commonwealth-rails/src/config.rs:88`), `offer_allow` (`:210`, enforced
  `sovereign-daemon/src/published_origins.rs:49`, `:109`). The knowledge route
  admits any verified member and reads no sharing flag
  (`sovereign-daemon/src/routes_internal/knowledge.rs`). These are the second
  deciders I2 removes.
- Fan-out shares one core (`mesh-reach/src/fanout.rs:142`) under separate
  per-peer adapters for origins (`commonwealth-media/src/fanout.rs:334`) and
  knowledge (`sovereign-daemon/src/routes_knowledge.rs:459`). Built.

### 3.10 A person in many rings

A person is the intersection of their rings, and only they see the
intersection.

- **Isolation by construction.** One journal and one register per ring; nothing
  written in one ring is rendered in another (I7, extended from transports to
  rings).
- **The sharing table.** The person's node holds, privately, what they lend to
  which rings: an app, a library, a corpus, a calendar, each with the rings it
  goes to. It is the person-side view of U5's allow lists, and the default for
  a new ring is nothing. A *ring group* ("close: house, family") is a local label
  for sharing to several rings in one tap; no ring learns it exists. Joining a
  ring offers "share this here too?" from the table, computed on the device.
- **The door names the ring.** A caller reaching an app lent to several rings
  arrives with a verified `X-Mesh-Rings` header — the rings the door admitted
  them through, client-sent copies stripped like every `x-mesh-*` header — so
  one library can keep the family album from the house.
- **Names are per ring.** The register holds a person per ring.
- **The person's own cross-ring view** (their week across every ring) is local
  `view` work on their device and never travels.
- **Carry is the only move between rings** (`ra-2`): a person re-posts an act
  into another ring under their own key, marked carried. Carrying someone
  else's act needs that author's consent or a rule their ring set (O6).
- **Leaving is per ring.** `Remove` is scoped to one register; a sealed ring
  ends every lend made to it.
- **The honest limit.** One node shows one transport key to the peers it syncs
  with in every ring (LIB §15), so peers who compare notes can tell it is one
  machine. A per-ring signing key keeps the records unlinkable and is offered
  for rings where that matters (a party, dating); the app says what it does not
  hide.
- Depends on U4: today a node syncs one mesh's online members.

## 4. Names

`Keeper` is `ra-7`'s copy-holding role: it holds what it does not author and
can never introduce. An app's runner is its **host**. The user-facing verb for
becoming a keeper of an app is *insure*. `Deed`, `Shop`, `Keeper` and `Insure`
are defined nowhere in code (`sovereign code converge noun`, 2026-10-02); mint
each once, in the crate that owns it. LIB §22 predates this section and calls
the host a keeper; this section wins.

## 5. Units and 6. Bars

Re-derived on 2026-10-03 into svrngs' `docs/DESIGN.md`, whose §8 orders the work now; the E-numbers below are that design at svrngs tag `e0-bot`. Where
the old units went: U14 and U15 → E0.1 and E0.4; U7 → E0.3, for Expenses only;
U2 and U16 → E1.3; U26 → E1.3, where a key per group is the default; U4 and U5
→ E1.4; U21 → E1.5; U6 → E1.7; U8 → E3.1. Retired: U1 and U3, since cuts are
the keeper's and forks void nothing. Cut, with their triggers in D8: U9-U13 and
U23 (the town), U17-U19 (the clerk and the Elder; U20's performer is E2.3's Ask), U22 (per-person accounts),
U24, U25 and U27 (lending to several groups, carrying), U28 and U29 (the
protocol and the kits). The `ring-apps` bars `ra-retired-key-is-refused`,
`ra-membership-is-order-free` and `ra-fork-is-reported` give way to E1's
`removal-is-sequenced`, `converge` and `forks-void-nothing`.

## 7. Defects found by this inventory

Each is a ralph row as it stands.

- **D1.** The membership fold ignores `through_seq`
  (`membership.rs:152`), so a cut is positional — the reading
  `lib.rs:352-357` forbids, because backdating dodges it. → U1.
- **D2.** No deliberate writer for `Admit`/`Remove`; the generic append doors
  (`commonwealth-rails/src/rail.rs:280`, `sovereign-daemon/src/routes_rail.rs:491`)
  accept them from any rostered signer. → U2.
- **D3.** `svrn ring show` (`ring_cmd/show.rs:231`) and `meshapp dev`
  (`sovereign-cli-llm/src/meshapp_cmd.rs:411`) serve with no CSP, and the
  scaffold writes peer text, `payload.description` included, through `innerHTML`.
  → U6.
- **D4.** `commonwealth-rails/src/join.rs:62` tells users to run
  `svrn mesh invite`, which does not exist. → U16.
- **D5.** `svrn mesh media offer` writes `[iroh] media_allow`
  (`mesh_media/offer.rs:29`) while enforcement reads `rails.toml [media] allow`.
  → U5.
- **D6.** `/v1/rail/live` is implemented twice: `commonwealth-rails/src/rail.rs:904`,
  `:917` and `sovereign-daemon/src/routes_rail_live.rs:205` (ARCH principle 8).
- **D7.** `oicp-types/src/origin.rs:132` cites `sovereign_daemon::origin_fanout`,
  which no longer exists.
- **D8.** The expense fold is copied byte for byte into `sovereign/apps/ring-doc/`
  and `my-doc/`. U7 removes the copies.
- **D9.** Per-member values reach users today: `POST /v1/ledger/contributions/current`
  (`commonwealth-rails/src/ledger.rs:214-220`), `GET /internal/contribution/view`
  (`sovereign-daemon/src/routes_internal/mesh_admin/contribution.rs:218-240`), and
  the desktop's per-peer rows (`mesh_commands.rs:520-550` → `MeshSettings.svelte:247`).
  → U17, after O1.

## 8. Decisions owed by the operator

- **O1.** `ra-9` ("commons") plans storage GB-hours *per person via the roster
  join*, rendered by `svrn mesh balance`. Stance 7 and LIB §21 forbid a
  per-member value inside a ring. One of them gives way; U17 waits on it.
- **O2.** The town's settings: how long an app may be unreachable before first
  refusal, and each keeper's turn. Per ring, with what defaults.
- **O3.** Whether reporting, never merging, a cut-off host's writes is enough
  at twenty-five people (§3.4).
- **O4.** Whether the bridge may move between nodes, which puts a chat
  platform's bot token on more than one member's machine.
- **O5.** Removal under contention — seniority, an arbiter, or mutual
  destruction with "a cut party may not cut back" (LIB §19 step 3). U1 makes the
  cut sound; this decides who may make it.
- **O6.** Carrying another member's act into a different ring: the author's
  consent per act, a rule each ring sets, or both. U27 waits on it.

## 9. Not in this spec

The library's laws in full and its prior-art verdicts (LIB §4, §18); the
superseded HTTPS door and keeper node (LIB §17); the strategy, entry, forms and
seed budget (`docs/internal/rings/`); the media, studio, call, feed and map apps
and the blob, gossip, stream and realtime primitives (`ring-apps` rungs `ra-3`,
`ra-6`, `ra-13`–`ra-21`). Each app rung adds its units here when it starts.

## 10. Protocols and extension points

A protocol goes wherever two implementations by different people must agree;
everywhere else an extension point — a registry with a conformance suite — is
enough. Open what varies by group; keep closed what must be identical for
anyone to trust anyone (ARCH principle 9).

**The narrow waist** — frozen, versioned, small, with golden vectors, so a
second implementation (a phone, a browser signer, another language) can exist:

- the journal wire: signing bytes, canonical form, sync digest, gaps
  (`RAIL_PROTOCOL_SKETCH.md`; v1 is U28, forced by the first implementation
  that is not ours — the phone app signs with rail-core natively);
- membership: one fold with the permutation property (after U1);
- the link: `<gateway>/#ring=…&via=…&relay=…` (`docs/THE_LINK.md`);
- what an app receives: the `X-Mesh-*` headers, `X-Mesh-Rings` included;
- the `[app]` manifest (§3.3) and the deed acts (§3.4).

**The wide edges** — open sets, each shipping `LEGO_KIT.md`'s kit:

| Edge | Who extends it | Conformance |
|---|---|---|
| apps, by manifest | anyone who can write a web server | town-app-generality |
| bridge adapters | whoever lives on a chat platform | entry-bridge-seam |
| provisioners | operators of self-hosted services | entry-revocation |
| civic-book reducers | agents writing from a sentence | town-five-laws |
| recipes and acquirers | anyone with a source | the recipe test and the profile's bank |
| model providers | anyone serving the OpenAI-compatible wire | the OICP manifest |
| relays, gateways, keepers, discovery | anyone with a box | I11, proven by turning it off |

**Closed:** admission, order and void (I1); the membership rule; origin labels
(I7); the two gates (I5, I6). An extension that reaches these can break what
every other extension trusts.

**When to open an edge:** after two in-house instances prove its seam, never
before. In order: the manifest (after two unlike apps are insured by it), then
bridge adapters (after Discord and one more), then provisioners (after Jellyfin
and one more), then the infrastructure roles. U29 builds the kits in that
order.
