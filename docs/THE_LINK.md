# The link is the protocol

Entry, reach, and what may never be load-bearing.

> Design record, 2026-09-21. Companion to `RING_APP_LIBRARY.md`, which
> specifies the library a ring app is written against. This document covers
> the other half of the front door: how a person arrives — with nothing
> installed, on any network, from a piece of paper if that is what is at
> hand — and the rules that keep every server on that path replaceable.
> What is built says so. What is design says so.

## The stance

A server on this path might claim three jobs: **carry** the record, **serve**
the record, **be** the record.

The record is an append-only journal of signed acts. Every member's node
holds a copy; any copy suffices; a part that is missing reports itself; a
correction follows the data wherever it has gone. Truth does not live at a
location, so no server can hold it hostage. That is what demotes every server
on the entry path to a courier. It is not a ban on servers — it is the
removal of the one job that creates custody.

Four rules for every piece of shared infrastructure, no exceptions:

1. It holds no state that members' nodes cannot reconstruct.
2. It sees no plaintext — for a code origin, not even which ring was opened.
3. It is named in the link, so it is swapped without coordination.
4. Its claim to be non-load-bearing is proven by turning it off.

A default is fine. A dependency is not. The project hosts a default origin
(today `svrnme.sh`, beside the landing page and installers) — as one courier
among several, never as the only one.

## The two tiers

**Members hold keys.** A member's acts are signed by their own key; the key
is the authorship, and the roster binds keys to people. A key nobody can
issue is also a key nobody can revoke, and that is the point — membership is
not permission handed out by a party, it is authorship.

**Guests hold grants.** A guest arrives with a link that carries a scoped,
expiring grant. The guest's browser does not sign acts: every act a guest
produces is signed by the host's node, with the guest carried as a display
name the host vouches for. The record honestly contains *"the host vouches
the guest said X"* — never *"the guest said X."* Grants are short-lived,
in-memory, and revocable by the host, which is exactly what makes them
suitable for strangers: the guest tier is where this system's credentials can
be killed, and it is the only one.

Graduation — installing a node and continuing with one's own key — is the
person's own confirmed act on their own machine. Nothing writes a member in.

## The ladder

Entry is a ladder, not a door. Each rung works when every rung above it is
down, and each rung's failure mode is landing on the rung below.

**Rung 0 — no infrastructure at all.** The LAN door: someone in the room runs
a node, guests' browsers on the local network open ring pages directly from
it. Plain HTTP, guests only, the host signs. Built.

**Rung 1 — inert data: the checkpoint.** A checkpoint is a journal prefix
plus a digest, stating "the record was exactly here, complete through these
marks, these authors." Any static host can serve it; a page that receives one
verifies it locally — signatures, order, voids, and precisely what is *not*
claimed — before anything else happens. No connection is needed. Data cannot
refuse anyone, because data is not a party. Designed; defined alongside the
protocol work; nothing writes checkpoints yet.

**Rung 2 — a live dial.** With the checkpoint verified, the page may upgrade
to live: dial the host's node end-to-end through a relay, present the grant
from the link, and speak scoped rail routes. Relays forward ciphertext by
construction. The link names several, in preference order; any one of them
suffices, and which one answered is nobody's business but the two endpoints'.
The dial is the one piece of open engineering: a browser tab speaking the
dial directly, unverified today.

**Rung 3 — install.** A small node of one's own. This is not an entry
mechanism; it is graduation — the point where a person stops being a guest of
someone's grant and starts being an author.

**Below the ladder, never:** anything that terminates plaintext at a party
that matters. The refused shape is a reverse tunnel — a relay we operate
proxying readable traffic to a node — because the relay would read every
credential and every journal body, and the link's central property (the
grant rides the URL fragment, which a browser never sends to a server) would
die at our own door. A courier that can read is not a courier.

The page verifies before it connects: bundle hash first, checkpoint digest
second, dial third. Live is an upgrade, not a precondition. Reachability is
therefore not an availability cliff; it is this ladder, and every rung of it
reports honestly where it is.

## The link, specified

**As built today** (guest links minted by `svrn mesh grant`, QR-encodable):

```
<base>/<ring/<ns>/ | ring/>#token=<grant>&exp=<unix>[&s=<label>]
```

- The page path is composed by the minting verb: one app (`--rail <ns>`) or
  the wall index (`--wall`). A base already spelling a page path is kept as
  typed.
- **The token is read only from the fragment.** A grant anywhere else — path
  or query — is not read; the parser refuses it. `exp` on the link is
  advisory for the person; the grant store's own expiry is the only decider.
- `s=` is a human label for the link's summary line. It is not a scope.

**Target form** (adds three fields; unknown parameters are ignored by old
pages, which is the forward-compatibility rule):

```
https://<any-gateway>/<ring/<ns>/>#token=…&exp=…&s=…&hash=<b64url>&at=<marks>&iroh=<dial>
```

| Field | Carries | Verified by | State |
|---|---|---|---|
| `token`, `exp` | the guest grant | the host's grant store, per request | built |
| `hash=` | SHA-256, base64url, of the runtime bundle | the page, before first render | owed |
| `at=` | the digest marks — `<actor-hex>:<n>` pairs, comma-separated | the page, against any payload it holds or fetches | designed |
| `iroh=` | the host's dial string (relay hints and addresses embedded, as join links already carry) | nothing — a dial is a try | param form built for joins |

Two rules keep these honest. **`at=` is a commitment, not a payload**: the
checkpoint document below is fetched — from a courier, a mirror, an
attachment — and `at=` is the small, QR-encodable statement that makes
whatever arrives checkable. A link with only `at=` and no reachable payload
still opens and says what it knows: the record stood at these marks, and
this page does not hold the data. And **`iroh=` reuses the join link's
parameter rather than inventing a courier list**: the dial string already
names its relays and addresses, one field, already parsed by every link
reader we ship.

## The page's bootstrap, specified

One ordered sequence; each step's failure degrades to the rung below instead
of failing the page:

1. **Load and pin.** Fetch the bundle; check `hash=` before first render. A
   mismatch is a red page that names the mismatch — never a fallback to
   unverified code.
2. **Verify against `at=`, if present.** Whatever the page holds — a cached
   copy, a fetched checkpoint document — is checked locally, no network:
   admit against the roster the checkpoint carries, recompute the digest,
   compare marks. On success the page renders the record as of the marks,
   gaps listed. With no payload at all, the page states the marks and says
   it holds no data — absence reported, never defaulted.
3. **Dial, if `token=` is present.** Using the `iroh=` dial string's relay
   hints and addresses in order: build the endpoint, dial the host's node,
   present the bearer over the guest channel, speak the guest door's
   ordinary HTTP. First success wins; the page displays which courier
   answered.
4. **Claim the session.** The person types a name; the page holds a session
   handle and presents it with every request. A lapsed handle answers 409
   with "claim a name again" — the handle is not a credential, so re-claiming
   is safe.
5. **Live loop.** Fetch, fold, render, append — the runtime's ordinary cycle,
   inside the grant's scope and the grant's lifetime.

## The checkpoint, specified

One self-describing document — the fetchable artefact of rung 1, servable by
any courier, mirrorable by anyone, emailable as a file. `at=` on a link
carries its `digest` field alone:

```
{ "v": 1, "ns": "<namespace>", "created_unix": <n>,
  "roster": { "<person>": ["<hex key>", …], … },
  "digest": { "<hex key>": <mark>, … },
  "ops": [ <journal lines, verbatim> ] }
```

Verification is four steps against primitives that already exist as public
exports of the rail's core:

1. Parse `ops` as ordinary journal lines — no reinterpretation. A verifier
   holding its own roster may substitute it; the embedded one is the
   self-contained fallback.
2. `admit(ops, roster, ns)` — signature, roster, sequence, fork and void
   rules all run. Any fork gap refuses the checkpoint.
3. Recompute `digest(ops)` and compare it to the stated `digest`. This one
   comparison *is* the completeness claim: the computed marks are contiguous
   from each actor's sealed floor by construction, so equality says the
   document holds every act through every mark it names — checkable, not
   asserted.
4. Render the fold of the admitted acts; gaps beyond the marks are named on
   screen.

The honest property: nothing in the document is trusted. Every field is
re-derived or refused, so a checkpoint served by a hostile host is worth
exactly what its signatures are worth. A node-side verifier needs nothing
new — ingest, admit and digest are shipped; only the export verb and the
in-page verifier are owed.

## The grant, specified

- **Mint.** `svrn mesh grant --wall` (every app the door's registry declares)
  or `--rail <ns>` (exactly one, refused elsewhere by name), with a TTL. The
  minting verb composes the page path and renders the QR; the token never
  appears in a path or query.
- **Bounds.** The grant store checks liveness and expiry per request; the
  scope fixes the path set (rail append/log/live, models, chat); the rail
  route refines per request — a namespace the grant does not name is refused
  with a sentence, not a generic 403.
- **Authorship.** Every guest act is signed by the host's node; the guest is
  a vouched display name on the act. The record contains *"the host vouches
  the guest said X"*, and the wall renders exactly that.
- **Revocation.** Grants are in-memory and TTL-capped: expiry, the revoke
  route, or a daemon restart ends them. This is the one credential in the
  system that can be killed, and it is why it is the one handed to strangers.
- **One accepted risk, stated.** The LAN door is plain HTTP by design, so the
  token crosses the venue's network readable by anyone sniffing it. The
  grant's TTL, its narrow scope and the host's signature on every act are the
  mitigation; the room's WiFi is the trust environment a room accepts.
  Secure-context needs (client-side crypto) belong to the https gateway tier,
  never to the LAN door, which does no client-side crypto.

## The dial, specified — the one open question

Everything above the dial is built. The dial is what a stock browser on
another network cannot yet do, and the design pins it to the smallest form:

1. The page builds an iroh endpoint, relay-only — WebSocket transport to a
   `relay=` hint.
2. It dials the host's node id, carried in the link the way join links
   already carry a dial string.
3. On the guest channel the acceptor already serves, the page speaks the
   guest door's ordinary HTTP — the same requests the LAN page makes, bearer
   and session handle included. End-to-end encrypted between page and node;
   the relay forwards ciphertext.

Success bar: log and append from a stock browser on mobile data, nothing
installed, token still fragment-only. If the wasm core cannot do this at
acceptable size, the fallbacks are the local helper (installed guests) and a
second dial transport as the escape hatch — the plaintext-terminating proxy
stays refused either way.

**One unmeasured number gates both page-side rungs.** Checkpoint verification
in a page and the dial both run the compiled core, and its wasm size has
never been measured; a first load on mobile data pays it before anything
renders. The node-side paths avoid the question entirely — which is why the
demo addendum below verifies on a node — and the measurement belongs to the
spike's first hour, not to a later audit.

## The demo slot

The ring-room demo already walks this ladder; the slot is narration plus two
additions, in cost order.

| Run-of-show step | Rung it demonstrates |
|---|---|
| 1 scan to name | entry with no internet involved |
| 2 guest edit attributed | the two tiers, as one wall line |
| 3 ask citing the Halo | a live dial for members, through couriers |
| 5 uplink cut | **the ladder itself**: the rung above fails, the room says so, the record heals byte-equal |
| 6 member-only-by-vouch | the boundary held |

**Add now — the checkpoint addendum to the offline leg.** During the cut, the
wall exports a checkpoint of the doc's journal; after the heal, the Halo
verifies the export by the five steps above against its own replica. New bar:
`ra-room-checkpoint-verifies` — an export made while the room was cut proves
itself on a machine that was never asked. Nearly all built (ingest, admit,
digest are shipped); the export verb is the only new code, and the offline
leg already has both endpoints and the cut.

**Stretch — step 7, "the phone forgets the venue WiFi."** Stock browser,
mobile data, the same printed link. Runs only if the dial lands first; on
stage it is an attempt with the checkpoint as its honest fallback, never a
bar that could silently not-judge.

**Watch-outs.** The QR as minted today is a room-address link: it is rung 0
only, dead paper to any phone not on the venue WiFi — say that, or print the
checkpoint-bearing form. And step 6's affirmative half (a phone becoming a
member by introduce) stays COULD-NOT-JUDGE: that is graduation, rung 3, and
this demo shows the boundary held, not the crossing.

## Honest costs

- **Code supply.** A page is code, and code is opinion until verified. The
  hash pin mitigates; the only cure is installing. Said on the page, not in
  a footnote.
- **The dial speaks a third-party core.** Libraries cannot refuse anyone;
  versions can. The hedge is a second dial transport (browser-native,
  interchangeable lookups, end-to-end encryption as standard) — same trust
  class as a relay, more work. Escape hatch, not plan.
- **Printed links outlive their couriers.** The checkpoint answers it, and it
  is on the link by default.
- **Availability is the members'.** No keeper node exists, and none may: a
  ring is as available as its most available member, and the page says so at
  the moment someone reaches for "invite."

## Status

| Piece | State |
|---|---|
| LAN door, per-namespace guest pages, scoped grants, session handles, QR + fragment link | built |
| Guest act semantics (host-signed, vouched name, grant-scoped routes) | built |
| Checkpoint definition and node-side verification primitives (admit, digest, missing-from) | built |
| Checkpoint export verb + in-page verifier | owed — the demo addendum needs only the verb |
| Link `hash`/`at`/`iroh=` fields | designed; grammar built with `token`/`exp`/`s`, and the `iroh=` dial param already parsed for join links |
| Dial from a stock browser tab | the one open question; unverified |
| Default origin serving the runtime bundle | hosting exists; bundle owed |

## Evidence

| Claim | Where |
|---|---|
| Link grammar as built: token, exp, summary — fragment only | `commonwealth/crates/commonwealth-discovery/src/deep_link.rs:224-238` |
| A token outside the fragment is not read | `deep_link.rs:243-259` |
| Page path composed by the minting verb; `/ring/<ns>/`, `/ring/` index | `sovereign/crates/sovereign-cli-llm/src/mesh_guest_link.rs:16-27` |
| Grant liveness and expiry checked per request | `sovereign/crates/sovereign-daemon/src/client_auth.rs:258-331` |
| Session handle header; 409 on a lapsed handle | `sovereign/crates/sovereign-daemon/src/routes_guest_session.rs:59`, `client_auth.rs:282-330` |
| A namespace the grant does not name is refused with a sentence | `sovereign/crates/sovereign-daemon/src/routes_rail.rs:100-117` |
| Guest acts signed by the host; guest is a vouched payload name | `sovereign/crates/sovereign-daemon/src/routes_rail.rs:407-439` |
| The checkpoint's four verification steps use shipped exports | `admit` (`rail-core/src/admit.rs:307`), `digest` (`rail-core/src/sync.rs:133`), re-exported from `rail-core/src/lib.rs:91-95` |
| The acceptor forwards the guest channel to the guest listener | `sovereign-daemon/src/daemon.rs:3637` |

Deeper design context: `docs/RING_APP_LIBRARY.md` (runtime, sandbox, join,
reach); `docs/internal/RAIL_PROTOCOL_SKETCH.md` (the checkpoint and digest
against the protocol's laws); the demo contract this document slots into is
`docs/RING_ROOM_RUN_OF_SHOW.md`.

Evidence, with file references, lives in the repository
(`docs/internal/RAIL_PROTOCOL_SKETCH.md` for the checkpoint and digest
definitions; `docs/RING_APP_LIBRARY.md` §11–17 for the runtime, the join flow
and the door).
