# Rings — the build spec

> Two parts. **Part I** is the design, normative from 2026-10-03. **Part II** is
> the inventory of 2026-10-02 — what runs today, checked against HEAD
> `571a9896b` — kept for its status and its defects, and superseded wherever
> Part I says otherwise. The rationale, prior art and history stay in
> `docs/RING_APP_LIBRARY.md` (cited as LIB §n); strategy and entry in
> `docs/internal/rings/`. Where any of them disagrees with this document, this
> wins and the other is corrected.

# Part I — The design

> **Normative for building, 2026-10-03.** What the built system is held to,
> written for the operator's direction of 2026-10-03: lean, stateless layers,
> OSS reuse, and architecture that makes whole classes of edge cases impossible
> rather than guarding them. It went through nine red-team rounds; their
> findings are folded in, and the decisions they forced are in D11. Where Part II
> disagrees, Part I wins, and Part II's units (§5) and bars (§6) are re-derived
> from D7 and D9 before child campaigns are written.

## D0. What it must do well, and nothing else

**The pitch, which every person-facing surface is held to:** *Keep talking
where you talk; keep what your group owns — money, plans, files, the library —
on your own devices, not a platform's.*

**The product is the iOS of groups.** A runtime, an SDK, an app model with
permissions, and distribution, so that developers build the apps a group uses
(operator, 2026-10-03: "we want community to build the apps, we want to provide
the platform"). It runs on every device a person has. We build one **seed** per
primitive — the smallest app that proves it — and never the finished apps.

Two rules hold every surface to the pitch.

- **No universe to explain.** A person sees the group by its own name ("the
  house", "the party"), people by their names, apps by their names, and
  *phone*, *computer*, *the chat*. Internal words — ring, member, guest, key,
  sync, journal, host, relay — appear in nothing a person reads (D9
  plain-words). The app on the home screen needs a name that is not "ring"
  (owed, D11).
- **The most general surface, the fewest features.** Everything a group does is
  an app on D0a, so the system learns by adding apps, never by adding platform.

### D0a. The SDK — what every app gets, and nothing more

An app is a static bundle with a signed manifest and a module of pure exports.
Its screen runs sandboxed and reaches only `window.group`:

| Term | What it gives | Seed |
|---|---|---|
| `group.me`, `group.people` | who is asking, and who is here — each person tagged by how they arrived (phone or chat) | every seed |
| `group.add(payload, {to})` | write to the group's record; `to` seals it so only those people can read it | Expenses, Tap |
| `group.undo(id, replacement)` | correct an earlier act of this app, never erasing it | Expenses |
| `group.state()` | the app's state — its exported reducer folded over its own acts by the runtime, the state of record — delivered as changes: `{value, complete, asOf}`; a screen may also receive this app's acts in the order they arrive on this device and keep its own view — a cache, never the state of record (the Doc's editor document) | every seed |
| `group.live` | an ephemeral channel, within this app, to the people who have this group open right now — cursors, presence; checked against the register, never written to the record; a declared capability | Doc |
| `group.files.put/get`, and `add(payload, {files})` | bytes beside the record, named by hash in the act's envelope (so the runtime knows which act holds which file), within the group's limits | Album |
| `group.start(name, people)` | a new group: an invite act in this group; each person accepts when they next open the app, on that device, completing when someone already in the new group next syncs; this app comes installed; a person with no key (chat only) is refused by name | Event |
| `localStorage` | private storage — a preloaded, capped shim over the shell — on this device, for this app in this group only | Tap |
| `group.open(name)` | a service someone in the group lends (the Library), opened in its own view through the member door | Library |
| `group.request(name, path)` | read a lent service's API as this person — GET only, by construction — through the shell's port and the member door, never the frame's network | Films |
| `group.play(name, paths, {on: here \| screen})` | the shell's own player for a lent service's media — an HLS playlist and a plain file — here, or on a TV in the room; the app sends play, pause and seek and sees state, never the TV or the URL | Films |
| exports `init`, `reduce`, `propose`, `say`, `due` | pure functions the runtime calls: the app's state from its acts; a proposal from a chat command; a text view; what is due at a time | every seed; Expenses for `due` |

**The manifest**, signed by the app's developer key, names the app, its version,
the SDK version it targets — which pins the fold engine: the QuickJS-wasm bytes
by hash — metering, memory maximum, stack size and the fold's own loop all
inside those bytes — and the segment size K and per-segment fuel budget — the kinds of act it writes, the earlier versions'
acts it skips (cumulative: a version that drops a skip is flagged), its chat
command schema (the one place commands are declared), and **the capabilities it
uses** — sealed payloads, files, private storage, the live channel, starting
groups, lent services and their APIs, playing on a TV, the chat. The runtime refuses any capability not declared, and the
group sees each one as a plain sentence when the app is installed.

**Install and update.** Anyone in the group may install an app; everyone sees
who did. Every install act — the first one too — carries the manifest signed by
the app's lineage key (its developer key and name), the version and the bundle
hash, and every runtime verifies it before running the app; the bundle travels
as a file. A different key is a different app. Before an update runs, the
runtime folds the group's acts under both versions and shows each person, on
their next open, what would change; an update that adds a capability asks each
person again. Each person consents
the first time they open an app, and the app's frame shows its name, version
and who installed it. Reverting is installing the previous version; it voids
nothing — acts a newer version wrote stay in the record, and people see "some
entries need a newer version of Expenses"; a later version may skip acts by their
version stamp, which its preview shows.

**What an app sees.** Every act is stamped by the runtime with the app's lineage
and version. An app reads and corrects only its own lineage's acts, plus the
people list. It reports "N acts from a newer version" rather than guess.

**Seeds:** Expenses and Doc (built, moved onto the SDK), Album, Event (a new
group with a when-and-where vote), Tap, Library, Films (a lent Jellyfin, browsed
and played on a TV).

**WHY_THEY_JOIN on this surface:** the post-party question is Tap — sealed
payloads that name no recipient and are padded to one size, uniform
participation, a two-party AND in Tap's reviewed library, and per-event keys in
private storage (the two-tap cap is a social rule, not one code can enforce
over hidden inputs). The address is the chat plus `group.start`. Money is the
record plus `due` for reminders without a model — the bridge posts and
fulfils them; the app, when opened, shows the person named what is due to them
and writes nothing, so in a phones-only group a reminder reaches only someone who
opens the app. Free things are lent services.
Context is one record per group and storage scoped to a group. The grapevine
needs acts across groups, a person-scoped store and the local model — additive
in a later SDK version, precluded by nothing here.

### D0b. Six jobs

- **J1 Keep the group's state** in an order every copy computes the same way,
  correctable without erasure. No platform holds it.
- **J2 Let anyone in the group's chat act on it**, with no account and no key.
- **J3 Let a person act from their phone, anywhere, with their own key.**
- **J4 Let a person lend a service they already run.**
- **J5 Tell the truth about absence** — a closed group, a missing act, the age
  of a copy, a partial balance.
- **J6 Let a developer build, install and run an app** on the SDK, on every
  device, without changing the platform.
- **J7 Make leaving cheap.** A group's whole state is a plain directory every
  member's device already holds. If a group goes wrong — its keeper lost,
  captured, frozen — its people refound it from any copy, within hours, with
  their apps, history, balances and files, and without whoever they leave out.
  Recovery inside a group is not attempted; leaving it together is the exit.

**Not goals.** Groups open to strangers. A service that keeps groups for people
(E0's test groups excepted, and said so). A device keeping a group its owner is
not in. Keeping someone's server-side app alive after they leave. A browser as a
client off the house WiFi. Push. Moving money. Recovering a group from inside
it — J7 is the exit. A node protocol published for other implementers, or agent kits, before
an outside builder asks.

## D1. The shape

Each group has exactly one durable shared thing — its journal, with the files
it names — held by the devices of the people in it. Everything else is a pure
function of the journal, a disposable cache, a stateless pipe, or someone
else's software lent through a door.

**One kind of node.** A phone, a laptop and an always-on box run the same node
core: a key, a full copy, the same ingest rule, the same sync, dialling and
accepting while running. They differ only in services, which follow from the
device:

| Service | Where |
|---|---|
| hold a copy, sign, sync both ways, run apps | any node while running |
| keep the group open | a node that is always on |
| be the keeper, sequencing removals | one Person key; the always-on node's when there is one |
| be the meeting point (its direct address in the invite) | an always-on node reachable from outside |
| lend a service | a node running that service |
| the bridge, and running apps' `propose`, `say`, `due` for it | the always-on node holding the chat's token |

A group of phones alone is open while at least two of its apps are open. A
group with an always-on computer is open while that computer is.

## D2. Layers

| Layer | Owns | Durable state |
|---|---|---|
| L0 reach | iroh endpoints, port mapping, the project's relay (the only relay) | none |
| L1 record | canonical form, signatures, the ingest rule, order, void, gaps, the register; journals and files | the journal and its files |
| L2 app logic | apps' reducers and handlers, run by the runtime's sandbox | none |
| L3 doors | the member door to lent services; every group route checking its group's register | none |
| L4 nodes | the node core, packaged for a computer or inside the app | a device's key, its copy, declared secrets for its services |
| L5 apps | bundles; lent services | lent services only, on the lender's device |

**Rule:** no layer above L1 holds state the group would lose if that component's
disk were wiped.

**Order** is decided once, in `admit`, by `(ts_unix, actor, seq, id)` on the
author's clock. Every signer stamps `ts_unix = max(now, its last, the ts of the
`Admit` that admitted it)`. Removal is sequenced by the group's keeper and
names the removed key's acts by hash, never by time.

## D2a. Code architecture

Held to `docs/RUST_ARCH.md`: **types encode the domain, ownership encodes the
architecture, I/O lives at the edge.**

**A sans-IO core.** `rail-core` is one deep crate with a narrow interface: a
pure, synchronous state machine over a group's journal, in `quinn-proto`'s
shape. `decide(&journal, msg) → event` and `apply(&mut journal, &event)` cover
ingest; `fold(&journal) → Fold` covers order, voids, the register and
completeness; `sync` returns what to send and in what order. `now` and
verification arrive as arguments; it never touches a socket, a disk, a clock or
a key. Its modules — `chain`, `ingest`, `register`, `admit`, `sync` — are
internal; tests drive it with golden journals and no async runtime.

**Types make the illegal states unrepresentable,** so whole classes of the
design loop's findings cannot be written:
- *Parse, don't validate.* Bytes from the network become a `VerifiedAct` only
  through `parse`, which checks the signature over the id and derives the id;
  `ingest` accepts nothing else. Ids, actors, persons and groups are newtypes —
  an `ActId` is 32 bytes and has no constructor from `Op::new`'s short id.
- *Closed enums for the domain.* Act kinds, membership acts, ingest outcomes
  (`Held`, `OfferAgain(reason)`, `Refused(reason)`), standing, completeness,
  capabilities and screen protocols (`AirPlay`, `Cast`, `Dlna`) are enums, so a
  new variant breaks every `match` that ignores it.
- *Keeper acts are their own type.* `Remove` and `Keeper` are variants of a
  `KeeperAct` enum that parses only from the keeper's chain; no other chain can
  carry one.
- *A redemption is built from its invite.* `Admit::redeeming(&invite, …)` copies
  the `Invite`'s person and role, so a redeeming `Admit` cannot name others.

**Ownership is a tree.** The shell owns one **journal actor** per group — a task
that owns the store and an `mpsc` receiver, behind a cheap cloneable handle —
and it is the one writer (U2). Routes, the bridge and the runtime send it
messages; nothing shares the store through `Arc<Mutex<_>>`. Async lives only in
the shells.

| Crate | What it is | State |
|---|---|---|
| `rail-core` | the sans-IO state machine and fold above | none |
| `rail` | the journal actor and its store | the journal |
| `ring-node` | the shell: iroh accept and dial, the group, claim and serve-to-a-screen routes as `tower` services | none durable |
| `fold-engine` | wasmtime over the pinned engine bytes, `fold_segment(state, acts) → (state, complete)`; an engine is data, not a type | none; seed caches are the caller's |
| `door` | the member door as a `tower` stack: header injection, credential mask, refused paths and play count as `Layer`s | declared secrets |
| `bridge` | Discord, concrete, behind its five calls | declared secrets |
| keystore | the one trait: iOS keychain, Android keystore and desktop each implement it | keys |
| shells | cw-rails, the app and the bridge executable wire the above | none of their own |

**Abstraction only where it is real.** A trait appears where a second
implementation exists or at a genuine I/O boundary — the keystore today, a
chat adapter when the second one is written — and nowhere else: no `Transport`,
`Engine` or repository trait threaded through the core. Errors are `thiserror`
enums in the crates and `anyhow` only in the three shells; refusal reasons are
designed like events.

**Classes made impossible, not guarded.** C1's three choices remove the
register's recurring holes by structure: a set of git-like acts cannot diverge
or be spliced; a fork that voids nothing needs no handling; removals with one
decider cannot race, cycle or need correcting. The types above remove the
field-binding and wrong-chain cases. What a gate still enforces is structure:
`quality/ARCH_LAYERS.toml` gains each new crate with `[[forbid]]` rows —
`rail-core` and `fold-engine` link no I/O (the purity gate), and `ring-node`
depends on `rail-core`, `rail` and transport only, not commonwealth-core, media,
host-kit or peer-wire.

**The map is written down.** The ring crates get an `ARCHITECTURE.md` in
rust-analyzer's style: the crates, how a message flows from an iroh stream
through the journal actor to a fold, and the invariants each crate owns.

## D3. Packaging and reach

**The node core** is one set of crates: rail-core, oplog, oplog-types,
kernel-types, rail and transport with the workspace hack removed, and a
`ring-node` crate cut from cw-rails holding sync, the group routes and the
acceptor — replacing the seams through which those routes reach
commonwealth-core, commonwealth-media, sovereign-peer-wire and host-kit today,
each of which pulls the hack back in. It builds for iOS, Android, macOS and
Linux (D9 same-core).

- **On a computer:** cw-rails is the core plus the services the computer
  qualifies for. The bridge is one more executable. The svrn daemon is an
  optional co-tenant for inference; it holds no group state and mounts no group
  route.
- **The app** is the core plus a shell (Tauri 2) plus the runtime, on iOS,
  Android and desktop alike.

**Keys and reach.** A person signs in each group with a key of that group's own
(RING_SPEC U26), so a cut in one group never touches another and the house
cannot link the party by signing key; someone in both groups can recognise the
same device by its endpoint, and no one else can see either register. Each device has one iroh endpoint key, carried
on every `Admit` as `endpoint` and co-signed, so the register is also the
address book: nodes dial a person's devices by endpoint. The endpoint key lives
in the keychain this-device-only without a biometric gate (iroh needs it in
process at launch); signing keys may ask for one. The invite carries the group
id, the starter's endpoint, the meeting point's direct address if there is one,
the project's relay, and an `Invite`'s id with the private half of its one-time
key.

The always-on box is the meeting point, with its port mapped by iroh. The
project's relay is the only relay: a small server running iroh's relay, seeing
only ciphertext, swappable by changing the invite, and proven non-essential for
direct paths by being turned off. Two phones on one WiFi meet through it; local
discovery would need Apple's multicast entitlement (D8).

## D4. State

| State | Lives on | Written by | If lost |
|---|---|---|---|
| journal, beginning with its genesis act | every device of the people in the group | their keys; the bridge's key for chat speakers | rebuild from any full copy |
| the group's directory: `genesis.json`, `journal.jsonl` (one signed act per line, append-only), `files/<hash>` — bundles are files | every device, readable and copyable with no tool; no key or private storage in it | the journal actor | it is the copy (J7) |
| files | fetched on demand; the always-on node fetches all; bytes kept only while an unvoided act names them in its envelope | `group.files.put` | refetch from any holder; else the app says it is missing |
| group id | `H(domain ‖ starter key ‖ nonce ‖ name ‖ H(legacy roster))`, the last term only for a migrated group; carried in the genesis act | the starter, once | — |
| a person's signing key per group | the device's keystore, this device only | the device | another device of theirs admits a new key; a cut key is never re-admitted |
| per-group encryption key | the device; its public half on `Admit`, co-signed | the device | rotate by an act its signing key signs |
| the device's endpoint key | the keychain, this device only | the device | a new endpoint, announced by an act each of its signing keys signs |
| the bridge's key | the always-on node, admitted with role Bridge | setup | cut and admit a new one; the guest book follows the role |
| declared secrets: chat token, capability token, channel map | the node running the bridge | the operator | re-issue |
| the guest book, keyed by platform user id | the journal | the bridge, from people who act in the chat | rebuild from the journal |
| private storage | the device, keyed by group and app lineage | the app | the app's own loss |
| installed apps | install acts; bundles as files | anyone in the group | the journal |

**Must not exist**, each a second decider or copy:

1. membership anywhere but the register — `roster.json` (migrated into the
   genesis), mesh membership as a group's roster, gate or peer set, registered
   namespaces that derive their roster from the mesh (`origins.rs:156-176`);
2. the svrn daemon's group routes and frozen `mesh.json`; its LAN guest door;
3. the attestation route;
4. a waiting store for ops that cannot be ingested yet;
5. a membership view inside every act — the sync digest keeps `View`;
   acts do not;
6. two app surfaces — `window.meshApp` folds into `window.group`, reusing
   meshapp's pack and install;
7. two fold traversals — the runtime owns the one;
8. three copies of `expenses.js`.

## D5. Components and contracts

**C1 rail-core.** Each part below is one module's contract (D2a).

*Three choices that make whole classes of edge cases impossible.* Nine rounds of
red-teaming kept finding holes in rules that defended assumptions the structure
did not need. These three remove the assumptions instead:

1. **The journal is a grow-only set of acts, linked like git commits.** Each act
   names `prev`, the id of its author's previous act; `seq` is its height. A copy
   holds an act only once it holds the act's `prev`, as git holds a commit only
   with its parent. Copies converge because set union does — there are no slots,
   holes or floors to reason about, and a withheld branch can never be spliced in.
2. **Forks void nothing.** Two acts by one key with the same `prev` are two acts,
   both kept and both folded, as if written one after the other. A reinstalled
   phone, a restored backup or a deliberate equivocation makes a second branch
   and changes nothing else — so there is no fork marker, copy bound, fork
   evidence or reinstall path.
3. **Removal, the one act that does not commute, has one decider.** Admitting,
   inviting and writing commute and stay decentralised. Each group has one
   **keeper** — a role held by one Person key, the starter's at first — and only
   the keeper's chain carries `Remove`s. Anyone may ask for a removal; the keeper
   sequences every valid request mechanically, in the order it receives them,
   naming the removed key's heads as it holds them. So removals have one order:
   no mutual removal, no cycle of removals, no race between removers, no cut to
   correct.

Compaction is cut in v1 (D8), so nothing deletes the history removal reads.

*Ids and the chain (`chain`).* An act's id is
`H(domain ‖ actor ‖ seq ‖ prev ‖ ts ‖ H(body))`, 32 bytes, and the signature is
over the id. The ring has its own constructor; `Op::new`'s short id is never an
act's id. Acts migrated from a legacy journal keep their legacy ids, so a
`Correct` still finds what it voids.

*The ingest rule (`ingest`)*, the same on every path:
1. the signature verifies over the id, and the id derives from the act — else
   refused;
2. the actor is bound: named in the genesis or by an `Admit` from a bound signer;
3. its `prev` is held, or it is the key's first act, and `seq` is `prev`'s plus
   one;
4. `ts_unix ≤ now + skew`, and `ts_unix` is at least its `prev`'s and the least
   among held `Admit`s naming the key.

An act failing 2-4 is not held and is offered again. Once a key's `Remove` is
held, ingest holds only that key's acts its heads reach.

*Admit (`admit`).* Order is `(ts_unix, actor, seq, id)`; `Correct` voids. A
removed key's acts count only where the `Remove`'s heads reach them by `prev`;
acts of a key whose every `Admit` fails count nowhere. Both are the **refusal
class**, separate from absence, so they never make a group incomplete. A head a
`Remove` names that is not yet held is absence.

*The register (`register`).* Membership acts, none of which can be voided:
- `Invite{redeem_key, role: Person | Bridge, via, person_id, expiry}` — what a
  link hands out; `redeem_key` is a one-time public key, and the link carries the
  act's id and its private half. `via` binds the redeeming device to a chat user
  id; `person_id` makes it another device of that person and counts only when the
  issuer is that person's key.
- `Admit{person_id, name, key, role, enc_key, endpoint, invite}` — signed by a
  standing key; co-signed by the admitted key, its device's endpoint key and,
  when it redeems an `Invite`, that `Invite`'s one-time key. Without `person_id`
  it mints a person whose id is the `Admit`'s own id. A redeeming `Admit` carries
  exactly its `Invite`'s `person_id` and `role`; otherwise naming an existing
  person needs that person's key as the signer. An `Admit` naming an unminted id
  is refused, and a key named by several `Admit`s belongs to the person of the one
  with the lowest id. So no one can become, or pre-claim, someone else.
- `RemoveRequest{key}` — any standing person's act, which the group sees.
- `Remove{key, heads, request}` — only in the keeper's chain, naming the removed
  key's heads as the keeper holds them when it sequences the request. A request
  whose signer an earlier `Remove` cut is not sequenced. A request to remove the
  keeper is sequenced by first handing the role to the requester.
- `Keeper{key}` — only in the keeper's chain: hands the role to another
  standing Person key; setup hands it to the always-on node when there is one.

An act of the register counts when its signer is standing at it: not removed,
or reached by the heads of the `Remove` that removed it. Rotating an `enc_key` or
`endpoint` is an act signed by the signing key and co-signed by the new endpoint
key. A Bridge-role key signs only `Invite`s with `role: Person` and `via` set. A
person with no usable key is admitted again as a new person, which the group
sees. A cut key is never re-admitted.

**Redeeming an invite.** A node holding a standing Person key signs the `Admit`
for a device that presents an `Invite`'s id and an `Admit` body co-signed by the
new key, the endpoint key and the one-time key — if the `Invite` is unexpired by
its clock and not already redeemed in its fold. The fold counts the `Admit` only
while the `Invite` counts. Two redemptions signed concurrently on different nodes
both count, and the group sees both.

**The line the register holds:** copies converge; outsiders cannot act; a
removed key can neither extend its standing nor admit anyone; no one can become
an existing person; removals have one order. Accepted, and said in D10: the
keeper is trusted to sequence and can delay or refuse a request (insider
capture, whose exit is refounding, J7); a removed key's acts the keeper had not received
when it sequenced the removal do not count; while the keeper's device is away,
removals wait, and if it is lost, removals stop — admitting and writing go on —
until the group refounds (J7).

*Genesis and continuation.* The genesis act carries only the id's preimage — the
starter's key, a nonce, the name — so it cannot be equivocated; everyone else
arrives by `Admit`. A node refuses a group until it holds a genesis that hashes
to the group id.

A genesis may **continue** an earlier group, and this one mechanism is both
refounding (J7) and migrating a group from before genesis. The preimage then
also names the earlier group's id, its heads as the founder holds them, and the
people carried over by their earlier `person_id`s. The new group folds the
earlier journal up to those heads — read-only history — before its own acts, so
every app's state, every balance and every file carries over unchanged, and
nothing after the heads does. A carried person joins by redeeming an invite with
an `Admit` co-signed by one of their keys from the earlier group, which proves
they are the same person; someone without one joins as a new person, and the
group sees it. The founder chooses who is carried over, and the people choose
whether to come: a group escapes a captor by not inviting them. A legacy roster
is the same thing with no earlier journal.

**C2 node core.** The journal store, files, and:
- **sync** over iroh, the one route between nodes. The acceptor admits the union
  of the standing endpoints in the registers the node holds, and every group route —
  sync, checkpoint, live — checks its own group's register. The sender sends in
  causal order: genesis; each act after its `prev` and after the `Admit` that
  binds its key; and it rotates the actor it starts from each chunk, choosing
  among acts the receiver can hold. Progress is proved by "each chunk's first act
  is holdable by the receiver". The digest lists each actor's heads, so only what
  is missing travels;
- **the claim route**, the only prefix the acceptor serves to an endpoint in no
  register: a new device redeems an `Invite` there (C1). The issuer's own device
  can always redeem its `Invite`; any other node can once the `Invite` has
  synced to it, so a link names the issuer's endpoint and the meeting point. The
  always-on node holds its owner's Person key in each group, since the Bridge
  key signs only `Invite`s. It
  is rate limited. An unstamped loopback caller is refused;
- **serve to a screen**, accepted by an always-on node only from devices of its
  own person: serve title T, for TV address A, through the member door;
- **`log`**, whose people are the register plus the guest book;
- on the node running the bridge, the **bridge append** on loopback:
  `{op, on_behalf_of}` with a capability token, signed by the Bridge-role key.
  Folds honour `on_behalf_of` only from a Bridge-role key.

**C3 member door.** `group.open(name)` opens a lent service in its own view, for
a person the group's register admits, tunnelled to the lender's node over iroh
(the forwarding body in `iroh_identity_forward.rs`), stripping client `x-mesh-*`
and injecting the verified ones. Video over the project's relay is capped, and
the app says so.

*A TV in the room.* Every way a TV takes a film from a phone — AirPlay (Apple TV
and the AirPlay 2 TVs from Samsung, LG, Sony, Vizio and Roku), Google Cast
(Chromecast, Google TV, Android TV) and DLNA's "play to" — ends with the TV
fetching an HTTP URL itself. That URL is the lingua franca, and the platform
makes exactly one: `group.play(..., {on: screen})` has a node on the TV's WiFi
serve one title, at a path no one can guess, over plain http on its real
address (never loopback, never through a resource loader), with CORS for Cast's
receiver, until playback stops; behind it is the member door's tunnel to the
lender. The phone is always the sender — the AirPlay route, the Cast session,
the DLNA control point — and the remote.

The URL's origin is the person's own always-on node when it is on the same WiFi,
so the phone can sleep; otherwise it is the phone. The phone hands the origin to
its own node over the group route below and finds it by the register's endpoint
and a direct dial, which brings iOS's Local Network alert (D10.12). With the
phone as the origin, AirPlay keeps playing through a lock under the app's
declared background mode for media playback, while Cast and DLNA stop when iOS
suspends the app about thirty seconds after locking — so the app says "keep
Films open — the film comes through this phone" before it starts. The URL serves
the one TV picked: by its address for Cast and DLNA; for AirPlay, which gives
the app no receiver address, the first device other than the phone to fetch it.
A route change mints a new URL.

The three protocols are adapters, as chat adapters are: the system route picker
for AirPlay, the Cast SDK's dialog (Bonjour; no multicast entitlement), and a
UPnP control point for DLNA — Android only, since SSDP on iOS needs Apple's
multicast entitlement (D8). The app gives an HLS playlist for AirPlay and Cast
and a plain file for DLNA, where seeking restarts the file at a time; Cast falls
back to the plain file if its receiver refuses plain-http HLS. For Jellyfin both
are the lender's own transcode endpoints, with the bitrate in the URL, capped to
the measured path, trickplay off, text subtitles as a WebVTT track and image
subtitles burned in, and a play session id per play, so one person's seek never
stops another's transcode. The lender's door counts its own concurrent plays
and refuses past the lender's limit. The TV needs no app, account, code or key.
A TV that takes no push — a PlayStation, a Fire TV Stick, a TV with none of the
three — is named as such, with "play here" offered instead.

*A lent service's credential never leaves the lender's node.* The lending
declaration names the secret itself, not only the header that carries it. The
door injects the header, refuses any request that carries the secret, asks the
service for uncompressed bodies, and replaces the secret wherever it appears in
a response body with a same-length mask — across reads and chunk boundaries, so
lengths and byte ranges stay exact. Jellyfin writes its token into playback
info and into the HLS playlists' subtitle, trickplay and variant URLs, and reads
the `Authorization` header before any `ApiKey` in the URL, so a masked URL still
plays. A lending declaration also names the paths the door refuses; Jellyfin's
refuses `/Sessions/Logout`, which would sign the whole group out.

**C4 the runtime.** One sandbox model on every device.
- *The fold runs headless, in one engine build, on every device:* the app's
  exported `init` and `reduce`, and `propose`, `say` and `due`, run in one
  pinned QuickJS compiled to wasm with its own maths library, inside wasmtime
  (Pulley's interpreter where JIT is forbidden; NaNs canonicalised), with no
  imports and JSON in and out. Fuel is metered by instructions injected into the
  module when it is built, so the cost table is inside the hashed bytes and
  wasmtime's own fuel is off; the memory maximum is declared in the module; and
  QuickJS's own stack limit is the one that binds — the host's stack running
  out means "could not run here".
- *The engine build is pinned,* because each of these defaults breaks
  determinism silently: bulk memory operations are metered by length (or the
  engine is built without them); a failed memory grow traps inside the
  allocator, so any out-of-memory is a breach whatever the reducer catches; the
  shadow stack is sized and placed first, so an overflow traps instead of
  corrupting data, with QuickJS's stack limit below it and the host's stack
  above what either backend needs; and the fold's own loop — removals, seed
  parse, per-act calls, the exactness check — runs inside the hashed bytes, not
  in shell code. A breach is read from the engine's own fuel counter. `Date`, random, `Intl`, the
  locale-dependent string methods, `WeakRef` and `FinalizationRegistry` are
  removed. State is JSON only, and exactly: a state that does not survive a JSON
  round trip unchanged (`-0`, `NaN`, `Infinity`, `undefined`, `Map`, `Set`,
  bytes) at a segment boundary is a breach, named for the developer; the dev
  harness folds at K = 1 to catch one that a boundary would hide.
- *Segments, so completeness is a function of the journal alone:* the fold runs
  in fixed segments of K acts by order position, each in a fresh instance seeded
  with the previous segment's JSON state and given its own fuel and memory
  budget. Each segment's seed state is a disposable cache on the device. A new
  act re-runs the last segment; a late act re-runs from its own.
  A segment that breaches its budget halts and is marked incomplete — the fold
  never skips the act. A device that cannot instantiate the engine at all (its
  memory is reserved at the fixed size, with no guard region) says "could not
  run here", which is not a breach.
- *The engine is part of the SDK version.* Every shell build carries every engine
  that any released SDK version names, and engines arrive only inside the shell
  — never as a group file, since loading a precompiled module is unsafe on
  untrusted bytes. A shell that lacks the engine an app's SDK version names
  shows "needs a newer version of the app" and folds nothing, rather than fold
  with another engine; a shell may retire an engine found unsound, with the
  same message. Precompiling for Pulley, and QuickJS's start-up snapshot, are a
  per-build cache. Folds run one at a time, and the memory maximum fits the
  oldest supported iPhone's headroom. JavaScriptCore is
  not used for folds: it has no public execution or memory cap, and its
  locale-dependent built-ins would let two phones compute different balances.
  The fold is the state of record; a screen receives changes and may keep a
  cache.
- *With a screen:* every bundle is served from one URI scheme,
  `ringapp://<install-id>/`, with a CSP that allows no network, frames, forms or
  navigation. Each app runs in `<iframe sandbox="allow-scripts">`. `window.group`
  is a MessageChannel port, and the app's identity comes from the shell's own
  map, never from the message. Camera access is `<input type=file>`.
- The runtime owns the one fold traversal. Golden journals fold to byte-identical
  state on every platform, under a swapped locale too (D9 fold-parity).
- A lent service's view gets no shell command, like a bundle's frame (D9
  frame-cannot-ipc covers both).

**C5 bridge.** An adapter and nothing else: discord.js on the gateway, so it is
outbound-only, behind a five-method seam. It renders command schemas from
manifests and sends each invocation to cw-rails' handler runner. It posts the
results with embeds, `@everyone` and role mentions suppressed (a `due` item may
mention the person it names), and posts each `due` item once with a fulfilment
act. It never loads app code. It keeps the guest book from people
who act or are mentioned in the chat, and a removal there sticks until a person
re-adds them.

Binding works like this:
- The bridge writes an `Invite` with `via` set to the speaker and sends them its
  link, which only they can see.
- The app redeems it at the always-on node.
- The chat shows "Ama's phone joined".

A bound speaker acts through the chat as themselves, labelled chat-origin. The
bridge checks that its own acts were admitted and says in the chat when they
were not. Its greeting says it reads the channel, whose computer keeps the group
open, and, for E0, that the data sits on the operator's computer.

**C6 the app.** The runtime with a screen, on iOS, Android and desktop, holding
several groups and never showing one inside another. It:
- keeps the key in the keystore;
- syncs both ways while open;
- installs by hash after verifying the signed manifest, with consent on first
  open and again when an update adds a capability;
- shows each app's name, version and who installed it;
- previews an update's effect before it runs;
- shows each app's `due` items to the person they name when it opens, and writes
  nothing for them.

After a reinstall or a restore from backup, the app syncs and keeps signing with
the same key from its synced heads; an act it lost that a peer still holds is a
second branch, which voids nothing (C1). Android deletes keystore keys on
uninstall, so a person whose only device is an Android phone is admitted again
by their own other device or, failing that, as a new person; the app says so at
setup, since an app cannot see an uninstall coming.

On Android, the sandbox ships only once frame-cannot-ipc passes; otherwise each
app gets its own native web view.

**C8 browser.** A static install page with no group data.

**C11 setup.** `svrn group start`, five nominal steps:
1. install;
2. create the Discord app and paste its token;
3. run setup, which finds the application, prints the invite link, lists the
   channels, enables the service and warns if the computer sleeps;
4. open the link;
5. pick the channel.

The host-setup bar measures the real count.

`svrn group refound <directory>` — or "Start this group again" in the app — makes
the continuing genesis from a copy, lists the people to carry over, and prints
one invite link per person for the chat; the always-on node is set up as in the
five steps above.

**C12 the developer surface.**
- *The reference:* the manifest and permissions; every SDK call with its limits;
  what order, undo, complete and missing mean; determinism and the five laws
  (determinism, environment independence, non-interference, idempotence,
  totality — RING_APP_LIBRARY §4) and a sixth, no module globals: `reduce`
  depends only on the state and the act; handler budgets;
  what a seal hides and does not hide; storage scope; file limits (per file and
  per person, set by the group) and deletion — the act's author may undo a
  file, which erases its bytes everywhere;
  who keeps a started group; install, update and consent; the sandbox; the dev
  loop; the test harness.
- *The seeds* as worked examples.
- *A dev loop that is the install path:* `svrn app dev` puts the bundle and
  writes an install act on save, in a dev group with the developer's phone, which
  reloads on sync.
- *A harness:* the five laws, the update preview, and fold-parity.

## D6. The narrow waist and the edges

**Closed:**
- the journal wire (canonical form, signing bytes, derived ids, the sync digest,
  gaps);
- the ingest rule, the refusal class, and the register's rule;
- admit, order and void;
- origin by role;
- SDK v1 (`window.group`), the manifest schema and permissions;
- the sandbox rules.

`commands`, `propose`, `say` and `due` are provisional until a second chat
adapter exists.

**Open:** apps; chat adapters; lent services.

## D7. Adopt, adapt, build, avoid

**Adopt:** iroh 1.x and its relay server; Tauri 2 with its barcode and biometric
plugins; AVKit's AirPlay route picker and the platform players; the Google Cast
sender SDK; a UPnP AVTransport control point; wasmtime with QuickJS (Javy); discord.js 14; Yjs in pages; age's X25519
recipient stanzas for seals; a pure-JS curve library for app cryptography; the
platform keystores.

**Adapt:** p2panda-auth's strong removal; the hash-linked author log (SSB,
p2panda); LiveStore's
versioned event names; webxdc's two sandbox requirements; meshapp's pack and
install.

**Build, by stage:**
- *Stage 0, the register:*
  0. rail-core reshaped into the sans-IO state machine (D2a) — modules
     `chain`, `ingest`, `register`, `admit`, `sync` behind `decide`, `apply`,
     `fold` and `sync` — behaviour preserved, before any rule changes; the
     journal actor; the layer-gate rows; `ARCHITECTURE.md`;
  1. genesis, the group id and continuation — refounding and legacy migration
     as one mechanism — and the group's directory;
  2. ids over `(actor, seq, prev, ts, H(body))` with the signature over the id;
     causal ingest; the digest by heads;
  3. the register (U1): `Invite`; `Admit` with person id, role, encryption key,
     endpoint and invite, co-signed; `RemoveRequest`, the keeper's `Remove` and
     `Keeper`; one membership writer (U2); retiring the fork marker; and the docs
     that describe seq cuts, floors or fork voiding — `RailAct::Remove`,
     `RailAct::Seal`, `RailAct::Admit`, `SignedOp.view`, the `sig.rs` and
     `membership.rs` module docs;
  4. group routes that check their own register; the acceptor's union of
     endpoints; the claim route redeeming `Invite`s; ra-23's single ed25519
     major.
- *Stage A, the runtime and SDK:*
  5. the node core out of the workspace hack, building for every platform;
  6. the app: shell, sandbox, permissions, key custody, two-way sync, several
     groups;
  7. SDK v1 with its engine pinned by hash, fold segments, and the handler
     runner;
  8. files;
  9. install and update;
  10. seals and private storage;
  11. `group.start`;
  12. the dev loop, the reference and the harness;
  13. the seeds;
  14. the project's relay, port mapping and the invite;
  15. the member door for `group.open`: admission by the register and the
      tunnel to the lender;
  16. `group.request` (GET only) and `group.play`: the shell's player and its
      media background mode, the URL on the WiFi served by the person's own
      always-on node or the phone, the serve-to-a-screen route, the AirPlay,
      Cast and DLNA adapters, the door's credential mask, play count and refused
      paths — and the forwarding module's doc, which says responses are never
      parsed; the Films seed.
- *Stage B, the chat:*
  17. the bridge append and the Bridge role;
  18. the bridge;
  19. setup and the bridge's single executable.
- *Alongside:*
  20. D4's removals.

**Avoid:**
- an existing local-first system as the substrate;
- Chat SDK; Dex and lldap; matterbridge; Matrix as a hub; Discord Activities;
- a home-screen web app as the client; browser keys;
- a tunnel that sees plaintext; a VPN app on the phone;
- Bun or Deno permissions as the sandbox for community code;
- Tauri's iframe isolation on Android without frame-cannot-ipc;
- yrs; server-held expense apps.

## D8. Cut, each with the trigger that brings it back

| Cut | Trigger |
|---|---|
| the town: deeds, keepers, insurance, handover, first refusal | a lent service a group would miss, counted |
| the LAN guest door | a group asks for browser guests on its WiFi |
| compacting act bodies | a group's journal on a phone passes a size measured to matter; it returns as a keeper-signed checkpoint of the fold, not a floor per key |
| a TV's own Jellyfin app (Fire TV, Xbox, webOS, Tizen, non-AirPlay Roku) pulling from a door the person's own computer serves on its WiFi — an app and a login on every TV | people whose TV takes no push ask, counted |
| `Remote-User` and per-person accounts on lent services | a named service that needs them |
| acts across groups, a person-scoped store, local-model access (the grapevine) | a grapevine seed is wanted |
| the clerk, the Elder, reminders a model writes | E0 passes |
| the node protocol for other implementers; agent kits | an outside builder asks |
| erasure coding, gossip, streams, push | an app needs one, named |
| a remote browser view | install friction at first contact, measured |
| public App Store listing | the age question is answered and 4.7.4's public index has an answer for group-private apps (TestFlight and a signed APK until then) |
| local discovery on one WiFi | Apple's multicast entitlement is granted |
| host packaging for StartOS and Umbrel | host setup is the measured blocker |

## D9. Bars

Each bar is watched failing first, on the input named after the arrow.

**Stage 0**

| Bar | What must hold | Failing input |
|---|---|---|
| one-decider | Plant `roster.json`, mesh membership, a registered namespace and the guest book so that they disagree. Every door, `log` and fold answers with the register. | today's mesh-gated sync |
| converge | Deliver the same acts in any order and any batching, forks included: every copy holds the same set and reaches the same fold and completeness. | today's first-seen-wins at a taken seq |
| causal-ingest | An act whose `prev` is not held is not held; a branch withheld under a published act and released after its key's removal counts nowhere. | `prev` carried but unchecked (v12) |
| forks-void-nothing | A key that signs two acts on one `prev` — by reinstall, restore or on purpose — voids neither, and every copy folds both. | the fork marker swallowing an `Admit` |
| removal-is-sequenced | Concurrent requests, two people asking to remove each other, and a request signed after its signer's removal: the keeper's order decides, no one else is cut, and the removed key's later acts count on no copy. | today's two removers racing (v13's third-party cycle) |
| keeper-handoff | After `Keeper`, removals continue under the new keeper; with the keeper's device gone, admitting and writing continue, and the app says removals wait and offers to refound. | a keeper hard-coded to the starter |
| refound-from-a-copy | From one member's directory, a 25-person group whose keeper is lost — and one held by a captor — is refounded with its apps, history, balances and files; carried people prove continuity with their earlier keys; the captor, not invited, can act in neither the new group nor its history past the heads. | today: no path but starting empty |
| group-is-a-directory | A group's state on a device is `genesis.json`, `journal.jsonl` and `files/`, readable with a text editor and copyable with `cp`; a copy folds to the same state on another device, and holds no key or private storage. | a journal readable only through the daemon |
| refusal-not-absence | A removed key that mints keys never leaves the group incomplete. | `UnknownSigner` counted as absence |
| genesis-certifies | A forged genesis for a group id, or a migrated genesis whose roster does not hash into the id, is refused. | a group with no genesis being accepted |
| cross-group | On a shared node, someone in group A cannot read group B's checkpoint or live lane. | today's checkpoint route |
| fresh-node-progress | A fresh phone syncs a group whose Bridge history is over 4 MB and whose `Admit` sits at a high seq, with a fork early in it. | today's hex-ordered sender |
| invite | An `Invite` that does not count, an expired one, a second redemption at an honest node holding the first, or an `Admit` redeeming it without its one-time key admits no one; two concurrent redemptions both count and the group is shown both. | a claim token outside the record (v10) |
| person-id | Neither a newcomer nor someone already in the group can become or pre-claim an existing person — including by redeeming a link with another `person_id` or `role`. | first admission decided by timestamp (v10) |
| admit-co-signed | An `Admit` that re-labels a person, swaps their encryption key, or grants Bridge without the admitted key's signature, or an endpoint rotation without the new endpoint key's, is refused. | today's admitter-only `Admit` |
| first-contact | A new phone's first claim lands only by redeeming an `Invite`; an unstamped loopback caller is refused. | today's open join route |

**Stage A**

| Bar | What must hold | Failing input |
|---|---|---|
| same-core | A closure test passes, and `cargo check --target aarch64-apple-ios` succeeds. | a phone syncing through `HttpBridge` |
| frame-cannot-ipc | On iOS and Android, a planted bundle — and a lent service's view — reaches no shell command, network or storage; `group.live` is the one path out, and only for a bundle that declares it. | a bundle loaded in the window |
| lent-service | A person outside the group is refused at the member door; video over the relay is capped and the app says so. | a door that admits by mesh membership |
| fold-parity | Golden journals fold to byte-identical state on every platform and on two builds of the app, under a swapped locale and timezone and any batching and arrival order, with a fold that breaches its budget marked incomplete everywhere; folding at K and at K = 1 agrees where both complete. | transcendental maths from the platform's library; a fold near the memory cap; one instance per arriving batch; a wasmtime upgrade between the two builds; recursion at the stack limit; a reducer that catches out-of-memory |
| fold-speed | On the oldest supported iPhone, under Pulley with injected metering, a full refold plus an update preview of 3,000 Expenses acts finishes within a time set before the first measurement, and so does the first fold after a shell update. | unmeasured today; a reducer looping `TypedArray.set` over a large buffer |
| permissions | An undeclared capability is refused, and an install whose manifest is not signed by the lineage key it names never runs. | no manifest check; an install naming another developer's key |
| update-preview | A hostile update's effect on past acts is shown before it runs, and one that adds a capability asks everyone again. | silent activation |
| file-erased | Void a photo, sealed or not, and sync: no node still holds its bytes. | bytes kept forever |
| phones-only | Two phones and no computer start a group, write on both, and fold the same, through the relay. | a phone that can only dial |
| relay-off | With the relay off, direct paths keep syncing and the rest say so. | a relay hard-coded in the app |
| rebuild-from-a-phone | Wipe the always-on node. One phone's copy restores the group, genesis included. | membership held in `roster.json` |
| reinstall-keeps-person | Reinstall the app, restore the phone from its own backup, move it by Quick Start with the old one still in use, or join from a phone whose clock is behind its admitter's, then act: the same person, nothing voided, nothing silenced. | the fork marker; a same-device restore |
| play-on-a-tv | On an Apple TV, an AirPlay 2 TV, a Chromecast with Google TV and (from Android) a DLNA-only TV, a film from a library lent from another house starts from the phone within ten seconds, with nothing installed or typed on the TV; its URL serves only the TV picked and is gone when playback stops. | today's member door, loopback and relay-capped |
| finishes-a-film | For each of those TVs, with and without the person's own always-on node, a two-hour film plays to the end while the phone locks at minute 5 and switches app at minute 20 — or the app said beforehand, in plain words, that it would not. | the phone as the only server |
| credential-stays-home | Grep every byte the TV, the phone's app frames and the record receive during browsing and playback: the lender's token appears nowhere, and a sign-out from the lent view leaves the library playable. | today's door, which passes bodies through |
| refound-in-hours | A group of five, its keeper's phone gone, refounds from the app and is back to writing within two hours, every person carried over. | measured against today's flow |
| films-journey | Four of five people who own one of those TVs start a friend's film on it within two minutes of first opening Films. | measured against today's flow |
| developers | At least two of three outside developers each run an app that is not a seed, using at least two of sealed payloads, files and `group.start`, in a real group of three people for a week, with no platform change and no more than five questions the reference should have answered. | today's docs |

**Stage B**

| Bar | What must hold | Failing input |
|---|---|---|
| N+1 | Four of five strangers in the chat settle an expense within three minutes, with no account and no key prompt. | measured against today's flow |
| phone-to-own-key | Four of five phone-only people go from the bot's link to their first act signed by their own key within five minutes, TestFlight included. | measured against today's flow |
| double-confirm | A Confirm retried after a bridge restart produces one act. | a Confirm with no interaction id |
| bridge-holds-no-app | The bridge never imports app code. | an import of an app module |
| plain-words | An allowlist covers the shell, the seeds, the bot, setup output and the iOS permission strings. | "ring" in the greeting |
| host-setup | Three of four people, unassisted, go from install to a first chat act within fifteen minutes. | measured against today's flow |

**Phase 1**

| Bar | What must hold | Failing input |
|---|---|---|
| two-copies-before-live | A group's commands register only after another of its nodes has synced. | registering immediately |
| second-app rate | At least 40% of groups still active at eight weeks run a second app; kill under 15% (STRATEGY_RINGS). | counted by hand, with consent |
| key-holding share | Half of active people hold a key by week eight. | counted by hand, with consent |

## D10. Risks

1. The always-on node is the group's availability. When it is asleep, the chat
   shows Discord's own "did not respond". A phones-only group is open only while
   two apps are open.
2. Whoever holds the Bridge key speaks for every chat speaker, and can write
   `Invite`s that admit new phones in their name.
3. Discord reads the channel, can revoke the bot, and is the only path for
   people without the app.
4. A deep link from a stolen bot token can point a phone at another node. A
   leaked invite link admits whoever redeems it; concurrent redemptions both
   count, and the group sees each.
5. TestFlight expires at 90 days; external testing and public listing meet Apple
   4.7.
6. The node core on iOS and accepting connections in the foreground are
   unmeasured; Delta Chat's foreground accept is the precedent. So is wasmtime's
   Pulley on iOS ("supported but less well tested", no iOS CI) and its speed
   (wasmtime's own doc: about ten times slower than native).
7. Clock skew past the bound refuses acts.
8. The project's relay is a cost and a dependency, kept swappable.
9. Files are as safe as the devices holding them.
10. A frozen or captured group is not recovered from inside; its people refound
    it from any copy (J7), losing only what came after the heads they carry.
11. The keeper sequences removals: it can delay or refuse one, removals wait
    while its device is away, and a lost keeper stops removals (not admitting or
    writing) until the group refounds (J7). A removed key's acts the keeper had
    not received when it sequenced the removal do not count.
12. iOS shows a Local Network alert for direct paths on one WiFi, and macOS 15
    applies the same rule to launch agents, which this project installs.
13. Debugging an app inside a TestFlight build may be blind (`isInspectable` is
    unmeasured).
14. Install flapping: reverting and re-installing moves acts in and out of the
    fold for everyone; the brake is removing whoever does it.
15. Apple's current terms permit downloaded code in any engine (DPLA 3.3.1(B))
    but bind mini apps to 4.7: filtering, reporting and blocking (4.7.1), no
    native APIs (4.7.2), consent each time (4.7.3), a public index with universal
    links (4.7.4 — unresolved for group-private apps), and age gating (4.7.5);
    the App Store build carries no catalogue of apps.
16. A film on a TV plays at the lender's upload speed (about 30 Mbps on an
    average US line, so about three 1080p viewers) and transcode capacity (a
    lender without hardware transcoding serves few), and over the relay it is
    capped. A burned-in image subtitle costs a full transcode. Without the
    person's own always-on node on the WiFi, Cast and DLNA need the phone kept
    open; AirPlay through a lock needs the media background mode, measured by
    finishes-a-film. Whether third-party AirPlay 2 TVs fetch the URL themselves,
    and whether Cast plays plain-http HLS, are unmeasured.
17. A long-lived app's state grows toward the fixed memory size, or its reducer's
    cost grows with the state until a segment runs out of fuel, and then it stops
    folding for the whole group; one pathological act can do the same. The remedy
    is a new app version or a new SDK version. The reference says so, and the dev
    harness reports the memory and fuel headroom left per segment.
18. Someone in the group can post acts timestamped far in the past (no earlier
    than their own admission), and each forces every open device to refold from
    that act's segment.
19. iOS keeping keychain items across an uninstall is observed behaviour, not a
    documented guarantee; if Apple changes it, iOS reinstalls work as Android's
    do.

## D11. Decisions

By the operator, 2026-10-03:
- **Copies:** one for E0 only, and two before a Phase 1 group goes live.
- **Cut:** the town; the kit and the node protocol; the clerk and the Elder;
  per-person service accounts.
- **Removal:** anyone may remove anyone, a removed key cannot remove, and there
  is no recovery inside a group.
- **Exit is cheap:** "If your ring gets fked you should have the state to
  reconstitute it (with dotfile elegance) within hours." A group's state is a
  directory every member holds, and refounding from it is J7. With that, one
  keeper per group sequencing removals is accepted.
- **Install:** anyone may install.
- **Push:** none.
- **The pitch and its two rules.**
- **Scope:** WHY_THEY_JOIN must be possible; the product is the platform, not
  the apps; we are building the iOS of groups (an analogy, not "an iPhone app
  first").
- **A file primitive; keep the Library.**
- **E0** runs on the operator's always-on computer.
- **Reach:** the box is the meeting point, and the project runs the only relay.
- **Devices:** a server and a phone are the same kind of thing, and phones-only
  groups are in the first stage.
- **Apps declare their capabilities, and the group sees them.**

Owed by the operator: the app's name.

By this design, open to the operator's veto:
- one node core in every package;
- a group kept only by the devices of the people in it;
- the Bridge role, with the bridge holding no app code;
- the journal is a grow-only set of git-like acts, and forks void nothing;
- removal has one decider per group, the keeper, handed off by act;
- membership acts cannot be voided; an `Admit` is signed by a standing key and
  co-signed by the key it admits;
- no compaction in v1;
- a signing key per person per group, and an endpoint key per device;
- the LAN guest door is cut;
- `commands`, `propose`, `say` and `due` are provisional.

## D12. Order of work

Stage 0 (the register) → Stage A (the runtime and SDK) → Stage B (the chat, on
the operator's computer) → Phase 1 (twenty groups). Each stage starts only when
the bars of the stage before it hold.

The design loop stops here. It ran nine rounds; the TV path converged, the
runtime's last findings were engine build settings, and the register's recurring
holes were removed by structure in v16 rather than patched. A finding during the
build is fixed in the one module that owns its bar.

# Part II — The inventory (2026-10-02)

> **Superseded where Part I differs; kept for status.** Every status below was
> checked against HEAD `571a9896b` on 2026-10-02: **built** runs today,
> **partial** runs with a named gap, **unbuilt** is not in code. Its register
> (§3.1), deeds and keepers (§3.4) and seq cuts are replaced by Part I's C1 and
> D8; its units (§5) and bars (§6) are re-derived from Part I's D7 and D9;
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
| U21 phone-member | ra-11 | U16 | the ring app: `sovereign-mobile` with a persistent, keystore-wrapped key introduced under the person; `window.ring` served by the shell over iroh to the host; rail-core native; client-signed append through `RingJournal::ingest`; a read-only remote teaser in `ring-runtime/web` | `sovereign-mobile/src-tauri`, the door, `sovereign/apps/ring-runtime/web/app.js` | phone-to-own-key, off-the-wifi (`RING_ENTRY.md` "Phones") |
| U22 provisioner | new | U4 | per-member Jellyfin accounts reconciled from the register, beside today's shared viewer | `sovereign-cli-mesh/src/mesh_media/` | entry-revocation |
| U23 uninsured-notice | new | U9, U14 | the clerk lists a leaving host's uninsured apps to the ring | the bridge's clerk | — |
| U24 ring-header | new | U5 | the door adds a verified `X-Mesh-Rings`, stripping client copies | `commonwealth-transport/src/iroh_identity_forward.rs` | proto-ring-header |
| U25 sharing-table | new | U5 | the person's lend table across rings, ring groups, and the "share here too?" prompt on join | the node's config owner, and the desktop | proto-nothing-by-default |
| U26 per-ring-key | ra-5 | U2 | an optional signing key per ring, bound to the person by one `Admit` | `ring_cmd`, the keystore | proto-records-unlinkable |
| U27 carry-consent | ra-2 | O6 | Carry refuses another author's act without their consent or their ring's rule | the Carry act, when `ra-2` builds it | proto-carry-consent |
| U28 protocol-v1 | ra-23 | U1, U3 | the journal protocol published as v1: signing bytes, canonical form, sync digest, gaps, membership, each with golden vectors | `commonwealth-rail-core/fixtures/`, `docs/internal/rings/reference/RAIL_PROTOCOL_SKETCH.md` | proto-second-implementation |
| U29 connector-kits | new | U8, U14, U22 | one kit per edge in §10's order, each with schema, conformance, `svrn new <kind>`, a dev loop, a reference, a skill | `.claude/skills/`, the schema crates | proto-thirty-minute-brick |

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
- U21 `git grep -n -i ephemeral -- sovereign-mobile/src-tauri/src/iroh_bridge.rs` → `:14`
- U22 `git grep -n 'const VIEWER_NAME' -- sovereign/crates/sovereign-cli-mesh/src/mesh_media/viewer.rs` → `:52`
- U24 `git grep -n -i x-mesh-rings -- '*.rs'` → nothing
- U25 `git grep -n -i -E 'sharing_table|ShareTable|LendTable' -- '*.rs'` → nothing
- U26 `git grep -n -i -E 'per_ring_key|ring_signing_key' -- '*.rs'` → nothing
- U27 `git grep -n -E 'Carry *\{|enum CarryAct' -- '*.rs'` → nothing
- U28 `git ls-files commonwealth/crates/commonwealth-rail-core/fixtures` → only `view_golden.json`
- U29 `git ls-files | grep -E '\.claude/skills/(ring-app|bridge-adapter|provisioner)'` → nothing

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

**`ring-protocol`** — the narrow waist, many rings, the edges:

| Bar | Claim | Watched failing with |
|---|---|---|
| proto-ring-header | an app lent to two rings sees which ring the caller came through; a client-sent `X-Mesh-Rings` never reaches it | the strip removed |
| proto-nothing-by-default | a person joining a new ring shares nothing with it until they choose | a lend defaulting to every ring |
| proto-records-unlinkable | two rings' journals under per-ring keys share no signing key for one person | one key reused |
| proto-carry-consent | carrying another author's act without consent or a ring rule is refused | the check disabled |
| proto-second-implementation | an implementation from the published spec alone, sharing no code with ours, verifies and orders the golden journals identically | a canonical-form change without a version bump |
| proto-thirty-minute-brick | per edge: a fresh agent session holding only the kit's skill ships a brick that passes conformance in under 30 minutes, reading no platform source (`LEGO_KIT.md`) | — (a timed run per kit) |

The two `elder-` bars in §5 (an Elder answer resolves to a signed act; every
node folds one recorded answer) belong to a fourth campaign when U19 starts.

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
