# Rings — the build spec

> **Normative for building, 2026-10-02.** The one document orders, campaign
> bars and ralph rows are written from. The rationale, prior art and history
> stay in `docs/RING_APP_LIBRARY.md` (the design record, cited here as LIB §n);
> rungs, primitives and existing bars stay in `quality/campaigns/ring-apps.toml`;
> strategy and entry in `docs/internal/rings/`. Where this disagrees with any of
> them, this wins and the other is corrected. Every status was checked against
> HEAD `571a9896b` on 2026-10-02: **built** runs today, **partial** runs with a
> named gap, **unbuilt** is not in code.

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
  signs; the person is a name in the payload), browser key, installed key — each
  a key under the same person.
- The browser crate `sovereign/apps/ring-runtime` (its own workspace; artefacts
  in `landing/ring/wasm/`) holds no rail-core and signs nothing. Browser signing
  is unbuilt.

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

## 4. Names

`Keeper` is `ra-7`'s copy-holding role: it holds what it does not author and
can never introduce. An app's runner is its **host**. The user-facing verb for
becoming a keeper of an app is *insure*. `Deed`, `Shop`, `Keeper` and `Insure`
are defined nowhere in code (`sovereign code converge noun`, 2026-10-02); mint
each once, in the crate that owns it. LIB §22 predates this section and calls
the host a keeper; this section wins.

## 5. Units

One unit is one to three ralph rows. Work in order; `Depends` binds. `Rung` is
the `ring-apps` rung it lands, or `new`.

| Unit | Rung | Depends | Deliverable | Lands in | Bar |
|---|---|---|---|---|---|
| U1 membership-cut | ra-5, ra-11 | — | the fold honours `Remove.through_seq` | `commonwealth-rail-core/src/membership.rs:152` | `ra-retired-key-is-refused`, `ra-membership-is-order-free` |
| U2 membership-writer | ra-5 | U1 | `svrn ring admit` / `remove` and one route that writes them; the generic append doors refuse membership acts | `sovereign-cli-mesh/src/ring_cmd/mod.rs`, `commonwealth-rails/src/rail.rs:280` | town-membership-door |
| U3 fork-view | ra-8 | — | contradictory view digests from one actor become a reported gap, beside the existing `RailGap::SequenceFork` (`admit.rs:109`) | `commonwealth-rail-core/src/admit.rs` | `ra-fork-is-reported` |
| U4 reach-by-register | new | U2 | dial a roster key with no mesh row (measure first); sync fans to roster ∩ reachable; `member_check` admits any key on a roster the node holds | `ring_sync/journal.rs:153-164`, `acceptor.rs:49-63` | town-one-register |
| U5 allow-names-ring | new | U4 | `ring:<id>` in every allow list; the knowledge route asks the register; D5 fixed | `apps.rs:297`, `config.rs:88`, `published_origins.rs:49`, `routes_internal/knowledge.rs` | town-one-register |
| U6 peer-text-is-text | new | — | CSP in `serve_file`; scaffold views build structure, never `innerHTML` | `host-kit/src/shell/files.rs:38`, `ring_cmd/templates/app.js:42,57,89` | town-peer-text-is-text |
| U7 fold-library | new | — | the library extracted from the expense fold and ring-doc, served beside the shim, the five laws as `node --test` | `sovereign-contracts/src/guest_pages/` | town-five-laws |
| U8 one-manifest | ra-10 | — | the manifest schema, read by `svrn run` and written by `svrn ring new` | the schema crate the api-gate guards; `run_cmd.rs`; `scaffold.rs` | town-app-generality |
| U9 deeds-fold | new | U2, U8 | the deeds as a Rust civic book beside `membership`, with permutation and turn-order tests | the crate the layer gate allows beside `PublishedApps` | town-first-refusal |
| U10 copies | ra-7 | U8, U9 | the host ships whole copies to keepers on change, by hash, with age | `commonwealth-media/src/apps.rs` | town-host-off |
| U11 keeper-door | ra-7 | U10 | a keeper serves the last copy read-only; reach falls back to a keeper when the host's claim lapses | `commonwealth-media/src/origins.rs:446` | town-host-off |
| U12 one-writer + handover | ra-7 | U9, U11 | the door refuses writes from a node the deeds do not name; `Handover` with the final copy | the door's write path | town-one-writer, town-handover |
| U13 first-refusal | ra-7 | U12 | `Release`, `Refusal`, `Take`, `Pass`; dormant apps kept | the deeds fold, the door | town-first-refusal |
| U14 bridge + Discord | new | U2 | bridge core and Discord adapter on one host: greeting, register upkeep, guest grade, expense commands, clerk proposals behind Confirm, link-only fallback | a new crate, named after `converge noun Bridge` | entry-n-plus-one, entry-removal-latency |
| U15 origin-gate | new | U14 | a planted ring-origin act rendered into a transport fails the build | the bridge core | entry-origin-gate |
| U16 invite | ra-11 | U2 | one invite with expiry and use count; `svrn ring invite`; D4 fixed | `ring_cmd`, `commonwealth-rails/src/join.rs:62` | entry-one-link |
| U17 no-member-value gate | ra-9 | decision O1 | no surface returns a per-member series; the subjects in D9 handled | the surfaces in D9 | entry-no-member-value |
| U18 gesture gate | new | U7, U14 | gesture kinds declared; an unconfirmed gesture act refused | the library's `kind`, the bridge | entry-gesture-needs-person |
| U19 journal-acquirer | ra-12 | — | `[acquire] type = "ring-journal"`, indexing what the fold shows | `corpus-engine` acquirers | elder-cites-an-act |
| U20 ask-performer | ra-12 | U7 | the `pending` performer, `ask` first, the answer as a fulfilment act | the runtime and the lending node | elder-one-answer |
| U21 browser-key | ra-11 | U16 | rail-core in wasm against golden vectors; client-signed append through `RingJournal::ingest` | `sovereign/apps/ring-runtime` | entry-one-link |
| U22 provisioner | new | U4 | per-member Jellyfin accounts reconciled from the register, beside today's shared viewer | `sovereign-cli-mesh/src/mesh_media/` | entry-revocation |
| U23 uninsured-notice | new | U9, U14 | the clerk lists a leaving host's uninsured apps to the ring | the bridge's clerk | — |

**Premises** — each prints this today, run from the repo root:

- U1 `git grep -n 'RailAct::Remove { key, \.\. }' -- commonwealth/crates/commonwealth-rail-core/src/membership.rs` → `:152`
- U2 `git grep -n -E 'Some\("(admit|remove)"\)' -- sovereign/crates/sovereign-cli-mesh/src/ring_cmd/mod.rs` → nothing
- U3 `git grep -n 'kind.view' -- commonwealth/crates/commonwealth-rail-core/src` → only `admit.rs:408`, `sync.rs:174` and tests (verification)
- U4 `git grep -n 'm.status == NodeStatus::Online' -- commonwealth/crates/commonwealth-rails/src/ring_sync/journal.rs` → `:159`
- U5 `git grep -n '"ring:' -- commonwealth/crates/commonwealth-media/src commonwealth/crates/commonwealth-rails/src sovereign/crates/sovereign-contracts/src/setup_config_iroh.rs` → nothing
- U6 `git grep -n innerHTML -- sovereign/crates/sovereign-cli-mesh/src/ring_cmd/templates/app.js` → `:42`, `:57`, `:89`
- U7 `git grep -n -E 'diffFold|byKind\(|upcast\(' -- '*.js' '*.mjs' ':!**/node_modules/**' ':!**/vendor/**'` → nothing
- U8 `git grep -n -E 'writer *= *"(host|ring)"' -- ':!*.md'` → nothing
- U9 `git grep -n -E 'enum DeedAct|struct Deed|Deeds' -- '*.rs'` → nothing
- U10 `git grep -n -i -E 'state_dir|code_hash|app_snapshot' -- commonwealth/crates/commonwealth-media/src/apps.rs sovereign/crates/sovereign-cli-mesh/src/run_cmd.rs` → nothing
- U11 `git grep -n -i read_only -- commonwealth/crates/commonwealth-media/src/apps.rs commonwealth/crates/commonwealth-media/src/origins.rs` → nothing
- U14 `git grep -n -i -E 'serenity|teloxide|twilight|discord_' -- '*Cargo.toml' '*.rs'` → nothing
- U16 `git grep -n -E 'Some\("invite"\)|max_uses|use_count|remaining_uses' -- '*.rs'` → nothing
- U17 `git grep -n 'fn mesh_get_contributions' -- sovereign/crates/sovereign-desktop/src-tauri/src/mesh_commands.rs` → `:520`
- U18 `git grep -n -i gesture -- sovereign/crates/sovereign-contracts/src/guest_pages sovereign/crates/sovereign-cli-mesh/src/ring_cmd` → nothing
- U19 `git grep -n -E 'RingJournal|commonwealth[-_]rail' -- 'corpus-engine*' sovereign/crates/sovereign-core sovereign-recipes ':!corpus-engine/xtask'` → nothing
- U21 `git grep -n commonwealth-rail-core -- sovereign/apps/ring-runtime/Cargo.toml` → nothing
- U22 `git grep -n 'const VIEWER_NAME' -- sovereign/crates/sovereign-cli-mesh/src/mesh_media/viewer.rs` → `:52`

## 6. Bars

`ring-apps` holds eight bars against a cap of nine (`scripts/co-lineage.py:93`,
`MAX_BARS`, a load error). Its ninth is `ra-fork-is-reported` (U3): two acts
from one actor whose views contradict are a reported gap, watched failing with
the view check disabled. Every other new bar goes in one of two child
campaigns, each watched failing first on the named input.

**`ring-town`** — the register, reach, the door, the library, deeds and keepers:

| Bar | Claim | Watched failing with |
|---|---|---|
| town-one-register | one `Remove` ends reach to every kind of app — app, media, offer, corpus, inference, work — within 60 s | any one allow list left on its own names |
| town-membership-door | membership acts are written only through the membership door; an `Admit` sent to a generic append door is refused | the refusal removed |
| town-peer-text-is-text | a payload carrying `<img onerror>` renders as text under the door, `ring show` and `meshapp dev` | `serve_file` without CSP |
| town-five-laws | determinism, environment independence, non-interference, idempotence and totality hold for every library reducer | a reducer reading `Date.now()` |
| town-app-generality | a one-file Flask app on sqlite, a civic-book page and a stock self-hosted server are insured by manifest alone, no change to any | a manifest without `code` |
| town-host-off | an insured app answers a `GET` from a keeper, labelled with the copy's age, and refuses a `POST` naming the host | insurance off; `read_only` ignored |
| town-handover | zero writes lost across a handover under a steady write load | the final copy skipped |
| town-one-writer | no node the deeds do not name accepts a write | the door's deed check disabled |
| town-first-refusal | a host removed; keepers offered the app in `Insure` order; with no taker the last copy survives on every keeper | turns ordered by arrival instead of rail order |

**`ring-entry`** — the bridge, joining, the gates:

| Bar | Claim | Watched failing with |
|---|---|---|
| entry-n-plus-one | four of five strangers settle an expense and open a title within three minutes of joining the chat, zero accounts, zero key prompts | — (a run, counted by hand) |
| entry-removal-latency | a member banned in the chat cannot have an act admitted 60 s later | the leave handler disabled |
| entry-origin-gate | zero ring-origin acts rendered into a transport | a planted violation |
| entry-bridge-seam | a second adapter lands with no change to the bridge core | — (the second adapter's diff) |
| entry-one-link | one link takes a phone from a QR to its first signed act; four of five non-technical people in under two minutes | two links |
| entry-revocation | one `Remove` disables every provisioned account | the provisioner off |
| entry-no-member-value | no route, CLI or UI returns a per-member series | a planted per-member route |
| entry-gesture-needs-person | an unconfirmed gesture act is refused | a planted auto-confirmed vouch |
| entry-flip-rate | at least a quarter of bridged rings move the register home within six months; kill under 5% | — (a strategic count, by hand) |

The two `elder-` bars in §5 (an Elder answer resolves to a signed act; every
node folds one recorded answer) belong to a third campaign when U19 starts.

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

## 9. Not in this spec

The library's laws in full and its prior-art verdicts (LIB §4, §18); the
superseded HTTPS door and keeper node (LIB §17); the strategy, entry, forms and
seed budget (`docs/internal/rings/`); the media, studio, call, feed and map apps
and the blob, gossip, stream and realtime primitives (`ring-apps` rungs `ra-3`,
`ra-6`, `ra-13`–`ra-21`). Each app rung adds its units here when it starts.
