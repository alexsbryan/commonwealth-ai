# The ring app library — a design from first principles

> **DRAFT — not in force (2026-09-20; amended 2026-10-02: §20 held, lent,
> carried; §21 two gates; §22 the town — register, shops, deeds).** A design record for the library a
> ring app is written against. It supersedes nothing. What a ring is *for*
> lives in `docs/internal/rings/reference/RING_APPLICATIONS.md` (per-host, untracked); the
> primitive inventory lives in `quality/campaigns/ring-apps.toml`.
>
> **To build from, read `docs/RING_SPEC.md`** (2026-10-02): the normative
> spec — model, invariants, contracts with their state at HEAD, units, bars and
> defects. This document stays the record of why; where the two differ, the
> spec wins.

Two parts. Part 1 is the library. Part 2 is what composes it — the runtime,
the door, reach and joining — and is the only part that does I/O.

**Part 1 scope: the library and nothing above it.** Every export is a pure function,
a plain value, or a property test. The library performs no I/O, holds no
grant, knows no roster, and runs under `node --test` with no daemon present.
Who may sign an act is the rail's decision, made beneath the library before
it sees anything. Who performs an effect, how code reaches a machine, and how
a page is isolated are above it. None of that is designed here; section 9
says where each sits and what was already decided elsewhere.

The method is derivation. Start from what the rail guarantees and cannot take
back, and ask what each guarantee forces on the code above it. Where that
lands on the Elm architecture it is a result, not a starting preference;
where it departs from Elm the departure is marked.

## 1. What is given

**G1. The log is the only shared thing.** A ring's state is an append-only
journal of signed acts. The rail computes one total order and one void set,
and every node holding the same acts derives the same order. A node may hold
fewer: the journal arrives with `complete` and a list of gaps, and a journal
with gaps is a subset, reported as one.

**G2. Every node computes alone.** No coordinator, no primary, nothing to ask
what the state is. Each node folds its own copy.

**G3. The author is increasingly a model, and the reader is a person.**

As built, ordering and voiding are decided once, in `commonwealth-rail-core`'s
`admit`. The client's `fold` only traverses: it skips voided acts and
replacement-less corrections and walks the rest in the rail's order
(`sovereign/crates/sovereign-daemon/src/guest_door.rs:323`). The library sits
on top of that traversal and never re-derives what is beneath it.

## 2. What the givens force

**State is a fold.** G1 and G2 leave one way for N nodes to agree without
talking: each computes `state = fold(reduce, initial, log)` with the same
`reduce`. State that is not a function of the log is state two housemates can
disagree about. The reducer is not a taste borrowed from Redux; it is the
only shape that converges.

**The reducer sees three things and nothing else.** Convergence requires
`reduce(acc, payload, op)` to be a function of its arguments. The clock,
randomness, the locale, floating point, the iteration order of an unordered
container, the network and the roster all differ between nodes. `op.ts_unix`
is the only clock.

**Acts are forever, so the reducer is a function of every version.** An
app's second release folds its first release's acts. Evolution is part of the
reducer's type, not a migration run once.

**State is always state-as-of-a-possibly-partial-log.** Incompleteness is a
normal condition and travels with the state. Absence is reported, never
defaulted (ARCH principle 6).

**Effects cannot live in the fold.** The fold runs on every node and again on
every reload. The library's answer is in section 5, and it stays pure.

## 3. The program

An app is a record of three pure functions.

```
app = {
  initial : State
  reduce  : (State, Payload, Op) -> State     // converged
  pending : State -> [Effect]                 // converged; effects as data
  view    : (State, Ctx) -> View              // local; never converged
}
```

`reduce` and `pending` are functions of the log alone, so every node computes
the same values. `view` also receives `Ctx` — whatever is true only here: who
is looking, the names this node knows, completeness, the live lane. The
library fixes
the type of `Ctx` as an opaque argument and supplies none of it.

That split is the split between what converges and what does not, and it is
structural: nothing local is in scope in `reduce`. The scaffold template
warns in a comment that a split must never read "everyone in the ring"; here
there is nothing to read.

Against Elm: `reduce` is `update`, `view` is `view`. `pending` stands where
`Cmd` stands and is not the same thing.

## 4. The library is an algebra

SICP's test for a language: primitives, a means of combination, a means of
abstraction, and closure — combining things yields a thing of the same kind.
The library contains two kinds of value and no third.

**Reducers.** `ledger`, `doc`, `poll`, `presence`, `rota`. Each is a complete
`reduce` with its `initial` and its tests.

**Functions from reducers to reducers.**

| Wrapper | What it removes from app code |
|---|---|
| `validated(schema)` | an invalid act becomes a gap, never a throw |
| `once(keyFn)` | two acts with one idempotency key collapse to the first |
| `upcast(migrations)` | old versions of an act are lifted before `reduce` sees them |
| `byKind({...})` | dispatch on `payload.kind`; unknown kinds are counted, not fatal |
| `combine({a, b})` | two apps are one app |

Every result is a reducer, so every result can be wrapped, combined and
tested the same way. `ring-doc` is a document plus an expense book; it got
there by copying `expenses.js` and its test verbatim, 491 lines, because
there was no `combine`. With closure that is one import and one call.

**The envelope is fixed.** Wrappers need somewhere to report, and the Express
lesson is that a mutable grab-bag passed down a chain becomes the framework's
worst property. The accumulator is `{ state, gaps }` and nothing else;
wrappers append to `gaps` and never add fields. A gap in the envelope is one
the act alone decides. A warning that depends on who is looking is `view`'s.

**Laws, each a property test the library ships.**

1. *Determinism.* Folding one log twice gives deep-equal state.
2. *Environment independence.* The same fold under a different clock, locale
   and random seed gives deep-equal state.
3. *Non-interference.* `combine({a, b})` projected to `a` equals `a` folded
   alone. `combine` refuses, at construction, two children declaring one kind.
4. *Idempotence.* Under `once`, a duplicate-keyed act changes nothing.
5. *Totality.* Under `validated`, no payload makes `reduce` throw.

Determinism is closed under composition — a deterministic function of a
totally ordered sequence, composed with another, is one — so an app assembled
from library parts satisfies laws 1 and 2 by construction and the suite
confirms it. An author gets convergence testing without writing any.

**One declaration per act kind.** Redux's cost was saying "expense" in five
places. A kind is declared once —

```
kind("expense", schema, (state, act) => ...)
```

— and the creator, the validator, the reducer branch and a form description
are derived from it. This is porcelain over the bare `reduce`; the plumbing
stays usable without it.

**Values.** The rail constrains payloads to JSON objects of whole numbers and
strings, because two nodes must derive identical bytes. The library keeps
that: `money` is integer cents with a deterministic remainder rule, and there
is no float helper.

## 5. Effects, as data — where this is not Elm

Elm's `update` returns `(Model, Cmd)`. That works because there is one
runtime and each message is processed once. Here `reduce` runs on every node
and on every replay, so a command emitted *by a transition* is emitted N
times and again on reload. Edge-triggered effects cannot be made safe.

Make them level-triggered. An effect is not something a transition fires; it
is something the *state* still wants.

```
pending : State -> [Effect]
Effect  = { id, want }          // plain data; `id` derived from the calling act
```

A fulfilment is an ordinary act carrying that `id`, folded under `once`. Once
it is in the log, `pending` stops returning the effect. "Has this been done?"
is answered by the fold, so replay, crash and restart need no bookkeeping —
desired minus observed.

The library's part ends there: a pure function that returns descriptions, a
creator for fulfilment acts, and law 4 guaranteeing that duplicates collapse.
It never performs anything. Who performs an effect, under what grant, and
whose fulfilment is believed belong to the layer above.

## 6. Evolution

Every act carries `kind` and `v`. `upcast` lifts old versions before `reduce`.
The newest reducer folds the whole log; there is no per-era reducer.

The library ships one more pure function: `diffFold(journal, before, after)`
folds a real journal under two reducers and returns the differences in state.
A year of a ring's acts is the regression suite for the next release. What is
done with a non-empty diff is a decision for the layer above.

The promise, borrowed from SQLite's file format: a journal written today
folds in 2040. It is kept by golden journals in the library's own tests.

## 7. What the library does not contain

No I/O of any kind. No runtime loop, no sandbox, no wire client. No grants,
roles, roster or notion of who. No deploy, versioning policy or package
manager. No router, component framework, state container or ORM. Views are
userland: the library fixes `view`'s type and ships a few elements bound to
fold state — a schema-driven form, a list, a completeness banner — and the
escape hatch is the DOM.

## 8. Authoring by agent

The shape is isomorphic to Elm and Redux wherever the semantics match:
`reduce`, `combine`, higher-order wrappers. Models have read millions of
reducers and no ring apps; each borrowed name is authoring skill that costs
nothing. `act` stays `act`, because it is signed and correctable and an
`action` is neither. The judge is deterministic — an app's tests plus the
five laws — which bounds the search enough for a small local model.

## 9. What is beneath, what is above, what is already decided

**Beneath: membership.** The rail decides who may sign, in `admit`, before
the library sees anything. An act from a key the roster does not claim is a
gap in the rail's answer and never an op, so the fold cannot be handed one.
Under the 2026-09-18 amendment (`docs/internal/rings/reference/RING_APPLICATIONS.md`)
membership is computed from a seed plus `Admit` and `Remove` acts, behind the
one membership function `admit` calls, with the permutation property as its
contract. That is a fold with a law, one layer down — and deliberately not
written with this library, because it must be decided before an app's fold
and identically by every implementation of the rail. The library adds no
second filter over admitted acts (ARCH principle 8). A reducer receives
`person` on an admitted op and nothing else about who is in the ring.

**Above: effects out.** The library returns descriptions. Performing them is
the runtime's.

**Already decided, not reopened here.** The same amendment cut `App`, `Role`
and `Invite` acts and a policy enum. One rule — any member admits and
removes — with recovery by `correct`. App versions stay off the journal: the
host stamps the bundle hash on each record and the fold names acts written
under a different version; which version a ring runs is a human matter. An
earlier draft of this document proposed ordering the choice of reducer with
a deploy act; that contradicted the cut and is withdrawn. What the library
owes that decision is one wrapper: acts stamped with a different bundle hash
surface as gaps, never silently folded as if they were this version's.

Deferred, recorded so they are not lost: performing an effect and reassigning
one whose performer never returns; enforcing determinism at run time rather
than only testing it; origin isolation between rings. §20 takes up the first
for effects on lent things and for `ask`.

## 10. Bars, written before any of this is built

Measured 2026-09-20: the scaffold template is 674 lines (domain 239, tests
252, glue 135, HTML 48). `ring-doc` is 1,732 first-party lines, 491 of them a
verbatim copy of the template's domain and tests, 423 an adapter the shelf
priced at about 50.

- `ring-doc` re-expressed on the extracted library is under 500 first-party
  lines, with its adapter tests passing from inside the library.
- Over a fixed bank of ten one-sentence ideas on a named model: median app
  under 400 lines including tests, and at least 7 of 10 pass the five laws on
  the first attempt. Below 5 of 10, the shape is not as authorable as claimed.
- The third app needs fewer than 200 lines that are neither domain nor
  library. More, and the plane is cut in the wrong place.

Order of work: extract from `ring-doc` first. It is behaviour-preserving, it
is the inventory speaking (ARCH principle 11), and it yields the first
measurement.

## 11. Open questions

1. Roster-relative gaps. The template passes the roster into the fold
   (`templates/app.js:36`, `expenses.js:194`) and uses it for one thing: an
   `unknown_person` gap, marked `fatal: false` (`expenses.js:93-98`). Balances
   converge and the gap list does not — the author kept the money right by
   hand. Here that warning is `view`'s. Whether any app needs a
   roster-relative *fatal* gap is open; one that does cannot converge, and
   the library should refuse it rather than accommodate it.
2. Fold cost. Every load folds the whole journal. A memo keyed on the reducer
   and a log digest is a cost-only optimisation and is pure. Not measured.
3. Whether `view` belongs here at all, or the library should stop at
   `reduce` and `pending` and leave the third function to whoever renders.

---

# Part 2 — what composes the library

## The core, and the boundaries it does not cross at first

Operator direction 2026-09-20: express the core on its own terms, be honest
where its boundaries are, and do not cross them at first, so the core can be
scaled as far as it goes. Every irreducible limit met while drafting this
part — a gateway's name being taken over, browser storage siloed by origin
and evicted, a poster naming a dead domain, a keeper reading plaintext, a
public inbox on a home machine — came from one move: treating a stranger's
browser as a citizen. The core does not make that move.

**The core, six sentences.**

1. You are a key you hold, on a device you own.
2. A ring is its members' machines. Nothing else holds it.
3. The only way in is a person: an `Admit`, written by a member, in the
   rail's order.
4. A guest is a member's responsibility: no identity, in the room, through
   that member's node, signed by that member, for as long as the grant lasts.
5. Apps arrive through the ring, by hash, and are folds.
6. Infrastructure forwards and never holds.

**Three tiers: alone, guest, member.** *Alone* is described further down. A
*guest* is a browser on the room's network,
served by the host's own node over the LAN door — no install, no key, no
domain, no gateway, no third party; the guest trusts the page exactly as far
as they trust the person standing next to them. This is built. A *member*
runs a node — the desktop app or the phone app — with a key in the device's
keystore, admitted in person or over a channel the two people already share.
There is no browser-held identity in anyone else's ring.

**What invitation-only removes, rather than mitigates.** No public surface,
so no spam, no moderation queue, no sybil rate limits, and the acceptor
admits roster keys only. The inviter is present when the invite is made, so
the minting node is awake at redemption. No gateway origin, so no takeover,
no origin silo, no eviction, no secure-context problem: a keyless guest needs
none of what a secure context gates. No keeper, so nothing outside the ring
reads it.

**The boundaries, stated and not crossed at first.**

| Boundary | What it costs | What crossing it would need |
|---|---|---|
| Strangers | No public rings, posters or ads that land in an experience. The way in is someone who would vouch for you | §17's gateway, a public posture, a guest inbox |
| The browser as an identity | A phone member installs the app | §16's browser key and wasm signer; a gateway origin |
| Remote guests | A guest is in the room, on its WiFi. Remote means member | the page as an iroh endpoint (§17) |
| Always there | A ring sleeps when its members' machines do; one that must not leaves a machine on | nothing to cross — this is the design |
| iOS in the background | A phone-only ring syncs while apps are open | a push sender, which is a hub |
| Getting the software | App stores and downloads are not decentralised. Running is | — |

**Alone — the tier everyone starts in** (operator direction 2026-09-20:
everyone should be able to start as a friendless newcomer in a Safari tab,
get real value, and taste how much more a ring would be). This does not cross
the boundary, because the limits came from a browser acting in *someone
else's* ring. A ring of one touches nobody's node and nobody relies on its
key. The boundary is between alone and together, not between browser and
node.

- **What it is.** The runtime, `commonwealth-rail-core` and the fold as wasm,
  from the static origin, cached by a service worker. The page generates a
  key, founds a ring of one, and keeps the journal in the browser's own
  storage. Sentences 1, 2, 5 and 6 of the core hold as written; 3 and 4 have
  nobody to apply to. A public link, a poster or an ad may land here, because
  it lands in the newcomer's own empty ring and on nobody's machine.
- **The value, in thirty seconds.** Write something, log something, paste or
  pick a document — then ask it. The answer is the passage it came from, or
  "I can't answer that from what you've given me." That is Sovereign's
  sentence reduced to a tab: extractive, cited, and able to decline, carried
  in `kernel_types::Answer` with its four-way `Verdict`. It is search with an honest
  verdict, not the full pipeline, and the page says which. A downloaded
  in-browser model is optional and later; it is hundreds of megabytes.
- **The taste of more is the truth about what is missing**, mounted by the
  runtime like the gaps panel, never a nag (ARCH principle 6): *only this
  browser holds it, and Safari may forget it after a week unused* — a machine
  of yours would keep it; *answers are passages* — a house with a model would
  talk; *a roster of one* — people you would vouch for appear here.
- **Graduation loses nothing.** A journal is signed JSONL. Install a node,
  scan its LAN code, and the phone is a guest of your own node carrying ops
  `RingJournal::ingest` already accepts because the signatures verify. The
  browser key, as founder, admits the node's key under the same name. The
  ring of one becomes your first real ring with no migration.
- **Checked 2026-09-20** (`cargo check --target wasm32-unknown-unknown`, in
  an isolated copy; no wasm binary was built, run or sized).
  `commonwealth-rail-core` with `oplog` and `kernel-types` compiles for the
  browser in 9 s. Two things in the tree stop it today, both manifest-level:
  every member depends on `workspace-hack`, which carries `tokio = "full"`,
  and `mio` refuses the target — the in-workspace check fails there; and
  `kernel-types` takes `getrandom 0.3` unconditionally, which needs the
  `wasm_js` feature plus `--cfg getrandom_backend="wasm_js"`. `oplog` reads
  files (`oplog/src/lib.rs:89`); that compiles and cannot work in a tab, so
  the browser supplies its own log storage and hands ops to `admit`.
  `ingest`'s contract is as assumed: "No validation and no re-signing … anything
  wrong with it becomes a gap when `admit` reads it back." The consequence for
  graduation: the browser key's ops are `UnknownSigner` gaps on the new node
  until that key is its roster seed, and the amendment forbids a roster route
  — so adopting a journal is a confirmation the person gives on their own
  node, not something the phone can write. The answer schema is
  `kernel_types::Answer` and `Citation` (`kernel-types/src/answer.rs:351`,
  `:197`) with the four-way `Verdict` (`judgement.rs:91`), and `kernel-types`
  is already in this wasm tree; there is no `Claim` of the shape
  `FIVE_PROGRAMS.md` §3 sketches. Not needed by this tier and still
  unverified: iroh in the browser at the pinned `1.0`.
- **Together needs a house.** Two browsers cannot reach each other and a
  browser cannot be invited into. The first node is the threshold of having
  anyone else, and the page says so at the moment someone reaches for
  "invite".
- **Its limits, said on screen.** Eviction; no sync between your own devices;
  whoever controls the gateway's name can serve this tier new code, and the
  harm is bounded to the one person whose data it is. There is no telemetry,
  so this tier's conversion is measured only at the campaign level.

**What stays in scope from the sections below:** the runtime (§12), the
sandbox (§13 — a guest's text still renders on members' screens), the bundle
(§14), reach (§15), `Admit`/`Remove` and the one invite with a use count
(§16, installed apps only). **Beyond the boundary, kept as the record of what
crossing costs:** §16's browser key, and all of §17 except the LAN door.
The phone member surface is `sovereign-mobile`, which already carries an iroh
bridge (`src-tauri/src/iroh_bridge.rs`) and today speaks to the client
surface, not to rings.

Part 1's purity rule stops here. Everything below does I/O. It is drawn as
four layers, each owning one thing about itself (ARCH principle 12), and it
is scoped to what the first ring apps and the spot in
`docs/internal/rings/pitch/RING_SPOT.md` demand — nothing is here because it might be
wanted.

| Layer | Owns | Does not own | Lives |
|---|---|---|---|
| runtime | timers, `fetch`, the DOM, `Ctx`, the sandbox | grants, roster, versions | `packages/ring/runtime`, served by the daemon |
| door | serving a bundle, response headers, the session grant | identity | `sovereign-daemon/src/guest_door.rs` |
| rail | admission, membership, order | reach | `commonwealth-rail`, `commonwealth-rail-core` |
| reach | whom to dial for a ring | who is a member | `sovereign-mesh/src/ring_sync.rs` |

## 12. The runtime

`ring.run(app, root)`. Each item replaces code both apps write by hand today
(`templates/app.js`, 135 lines; `ring-doc/app.js`, 315).

- **The loop.** Fetch the log, fold, render, patch. Polling stays — the wire
  has no tail op and `log` takes no cursor — but a refold happens only when
  the log changed.
- **`Ctx`.** `me`, names, `complete`, `gaps`, `namespace`, `live`, unwrapped
  once. The `.members` trap `initial()` throws on disappears.
- **Gaps are the runtime's.** It mounts rail gaps, app gaps and the
  incompleteness banner outside the app's root. "Never hide either panel" is
  a comment in the template today; here the app cannot.
- **The write door.** `dispatch(act)` runs the same `validated` schema the
  reducer uses, refuses fatal gaps, records, refolds. No optimistic state:
  the rail assigns `seq` and `ts_unix`, so a local guess at order is wrong.
- **The live lane.** Drain on an interval, throttle sends, surface as
  `Ctx.live`.
- **Limits are values.** `ring.limits = { actBytes, liveBytes }`, so an app
  adapts to the 64 KiB and 4096-byte caps instead of meeting a refusal.
- **Served modules.** Library and runtime ship as ES modules beside the shim
  from real `.js` files; `RING_SHIM` stops being a string in a `.rs` file.
  (Done by 2026-10-02: `RING_SHIM` is `include_str!("ring_shim.js")`,
  `sovereign-contracts/src/guest_pages/shim.rs:41`.)
- **The test entry.** `node --test`, the five laws, `diffFold` against
  `svrn ring log <ns> --json`.

Out, by demand: the effect performer (no app asks for one — ring-room reaches
the house AI through `chat ask` in its demo script), so `pending` is rendered
as wanted and unperformed, never dropped; and the debounced writer, which has
one caller and stays in the `doc` adapter. §20 names the first apps that ask
for the performer — the Elder, the clerk and a provisioner — and where it runs.

## 13. The sandbox — two layers, neither remembered

Checked 2026-09-20: `templates/app.js:42,57,89` and `ring-doc/app.js:275`
write peer-authored strings through `innerHTML`, and no response on the ring
page path carries a Content-Security-Policy. Any roster member can run script
in every other member's page, where the guest bearer sits in `location.hash`
(note `1f0b0fce`). An `escape()` helper in the template would be a remembered
rule the next generated app forgets.

**The browser's layer.** `serve_file` — the one function both the door and
`ring show` serve through — sends `default-src 'self'` with no inline script.

**The language's layer.** Hardened JavaScript: `lockdown()` once, then
`reduce` and `pending` evaluated in a Compartment whose only endowments are a
`Date` that answers `op.ts_unix` for the act being reduced and a
`Math.random` seeded from `op.id`. Replaced, not removed — Temporal's lesson —
so model-written code that reaches for a clock still converges, and law 2
still checks it. `view` runs in a second Compartment holding a structure
builder and no `document`; text enters as text nodes. No `fetch` exists in
either. Risk: `lockdown()` breaks libraries that touch primordials, and the
vendored Yjs bundle is a candidate, so the `doc` adapter runs outside as
trusted library code; if that is not enough the fallback is a worker.
Licence, size and maintenance of SES are unverified.

Two requirements are taken verbatim from the webxdc messenger specification
(read 2026-09-20), because they are what makes running someone else's app
safe: the host "MUST deny all forms of internet access" and "MUST isolate all
storage and state of one … app from any other." The second needs one origin
per ring bundle. Paths cannot give that; a port per ring can on the LAN door,
a subdomain per ring can on an HTTPS door. Decided in §17: neither — a
sandboxed frame with an opaque origin.

Bar: a payload carrying `<img onerror>` renders as text in both apps, watched
failing first.

## 14. The bundle

A zip with `index.html` at its root: one artefact, one hash. The 2026-09-18
amendment has the host stamp that hash on each record; Part 1 section 9 owes
it the wrapper that surfaces acts stamped otherwise as gaps.

## 15. Reach — every ring live at once

As built, a node has one active mesh and parks the rest, and `ring_sync`
dials only that mesh's `Online` members (`ring_sync.rs:227-234`). A key on a
ring's roster with no member row in the active mesh is never dialled. The
limit is the sender's address book; rings carry no mesh id on disk or in a
signed op, and one iroh endpoint is bound per node from one key.

How to reach a key is already a statement the key makes about itself.
`commonwealth-core/src/dial_sig.rs` signs `DOMAIN || pubkey || version ||
relay || addrs` under the node's own key, with no mesh id, a monotonic version
and three pinned attack tests. iroh's pkarr lookup is already wired
(`commonwealth-transport/src/iroh.rs:152`).

1. Measure: dial a roster key that has no member row. Untested.
2. `ring_sync` fans out to (the ring's roster) ∩ (reachable keys). This also
   closes the recorded defect that sync ignores the roster
   (`ra-ring-reaches-only-members`).
3. The iroh acceptor's `member_check` (`daemon.rs:4238`) widens from "member
   of the active mesh" to "a key on any roster this node holds". A behaviour
   change; its own test, watched red.

Multi-mesh is not built. A mesh shrinks toward what it owns — lending — until
the `fabric.mesh` singleton stops mattering. Switching rings is then not a
node operation; it is which page is open.

Decision owed: publish relay URL only or direct addresses too, and n0's DNS or
a self-hosted one.

## 16. Joining — one invite, one link

As built there are two links. The guest link works in a phone browser and
grants no identity: the host's key signs every guest act and the guest is a
name in the payload. The member link grants identity and is inert to a phone
camera. Neither store has a use count.

- **One invite.** A secret on the node that minted it, with an expiry and a
  use count, as the amendment specified. The QR carries the inviter's key
  fingerprint, the secret and the ring (SecureJoin's shape). Redeeming writes
  one `Admit{person, key}`.
- **In a browser** the page generates the key. It signs its own acts with
  `commonwealth-rail-core` compiled to wasm — the crate does no I/O, and a
  hand-written canonicaliser in JavaScript would be a second decider. The
  client-signed append reuses `RingJournal::ingest`, which already takes ops
  signed elsewhere.
- **In the installed app** the same link deep-links and admits the node's
  key. Installing later admits a second key under the same person, which the
  roster already models.
- **The bearer is a session; the key is the identity.** A returning guest
  signs a challenge for a fresh bearer.
- **One verb for the host.** `svrn ring invite <ns>` and a desktop button;
  the door configures itself while an invite is live. Today it is five flags
  and two hand-edited config keys, and `--model` is required for a rail-only
  grant.

## 17. Reaching a node from a phone — no hubs

Operator direction 2026-09-20: no load-bearing nodes, and no TLS or public
domain to operate. Dumb, interchangeable relays that ship out of the box, as
iroh's do, are fine. That withdraws the HTTPS door and the keeper node
proposed in the first draft of this section (kept below, marked, because the
reasoning about secure contexts still holds).

**The page is an iroh endpoint.** iroh builds for the browser relay-only. The
runtime — iroh, `commonwealth-rail-core` and the fold, compiled to wasm —
dials the inviter's node by public key through a relay, on the `GUEST_ALPN`
the acceptor already serves, and speaks the rail over that stream. This is the
bridge `mesh_guest.rs` runs as a local proxy, moved into the page. Traffic is
end-to-end encrypted between the phone and the node; the relay forwards
ciphertext. Nothing terminates TLS for anyone and nothing proxies HTTP.
Unverified against the pinned iroh, and the wasm size is unmeasured.

**One static origin serves the runtime and nothing else.** It is a file on
commodity static hosting: no server, no certificate to manage, no data, no
traffic after first load (a service worker caches it). It exists because a
browser needs a secure context for `crypto.subtle`, service workers and
passkeys. It is load-bearing the way a name is, not the way a hub is — but it
IS sticky: browser storage and passkeys are per origin, so a mirror is a
different identity silo. Installed nodes never touch it.

**Defaults, not dependencies** (operator direction 2026-09-20: a domain may
exist, as an implementation of a protocol and never a toll booth; if it goes
down everything is still fine). Three pieces of shared infrastructure exist —
the gateway origin, the relay, the discovery DNS — and each obeys four rules:
it holds no state that members' nodes cannot reconstruct; it sees no
plaintext and, for the gateway, not even which ring (everything identifying a
ring rides the URL fragment, which a browser never sends — matrix.to's
pattern); it is named in the link or the config, so it is swapped without
coordination; and its claim to be non-load-bearing is a gate, so it is proven
by turning it off (ARCH principle 5).

The link is the protocol: `<any-gateway>/#ring=<key>&via=<member keys>&relay=<url>`,
with a `ring:` scheme form for installed apps and for print beside a QR. The
gateway is a reproducible static bundle with a published hash; anyone hosts
one, a node already serves one on its LAN door, and the poster generator
prints whichever the host chose. The project's domain is only the default.

What survives the default gateway going down: every installed node; every LAN
room; every phone that loaded once, because the service worker answers the
navigation; every link opened at another gateway. What does not: a stranger
with a stock camera scanning a poster that names the dead gateway. A printed
URL's first contact depends on its name resolving, and nothing on a stock
phone avoids that — so the name does no work and holds nothing, and a
community that cares prints its own. Two limits stated plainly: a web origin
cannot be defended from its own server, so whoever controls a gateway's name
can serve its browser-only users new code (installing removes that trust);
and switching gateways is a new browser key, joined to the same person by an
ordinary `Admit`.

**Apps arrive through the ring, not over HTTP.** The bundle (§14) comes from
a member's node over iroh, is checked against its hash, and runs in an
`<iframe sandbox="allow-scripts">` with no `allow-same-origin`. The browser
gives that frame an opaque origin: no storage, no cookies, no reach into the
runtime, and with a frame CSP no network. The runtime in the top frame holds
the key, the endpoint and the journal, and talks to the app by `postMessage`.
This meets both webxdc MUSTs with no DNS and no per-ring origin, and it
replaces Compartments as the isolation boundary in §13; the deterministic
`Date` and `Math.random` are injected by the frame's bootstrap.

**Availability is the members'.** No keeper. A ring is as available as its
most available member. Two phones in browsers sync through a relay while
both tabs are open; the first member to run a node makes the ring durable. A
ring of one in a browser lives in that browser's storage and is as durable as
that storage, and the page says so.

**The LAN door stays** as the no-internet path: plain HTTP, guests only, the
host signs. It is built.

### Superseded: the HTTPS door (first draft)

`crypto.subtle` exists only in a secure context, and a phone off the WiFi
cannot reach a LAN bind at all, so both identity and reach need one public
HTTPS name that tunnels to the host over `GUEST_ALPN` — the bridge
`mesh_guest.rs` already runs client-side. It is stateless and holds no
journal. With client-side signing it cannot forge an act. It can read
traffic and it serves the JavaScript, so a guest trusts it as they would any
website, and installing removes that trust. It is infrastructure, not a
member: the trust class of the iroh relay already depended on. Anyone may run
one; the link names which.

For a stranger arriving from an ad there is no friend's node. They found their
own ring from the browser key, a keeper node is admitted as an ordinary
member to hold the journal, and after installing they `Remove` it. Same ring,
same identity, exit in one act. The keeper reads plaintext; the amendment
deferred encryption "for a real third-party relay", and this is that relay.

Operator decision: whether to run one.

## 18. Prior art — verdicts in the shelf's vocabulary

`quality/campaigns/ring-apps-shelf.md` holds the dependency verdicts per
primitive. These are the ones it has no row for.

| Source | Verdict | What is taken |
|---|---|---|
| webxdc (spec and catalog read 2026-09-20) | **avoid** as the native contract; **spike** as an import path | It has no order, no void, no completeness, no signed authorship: native apps written to `sendUpdate` would discard what the rail guarantees. Taken regardless: the two MUSTs in §13, limits as values, a zip as the bundle. Spike bar: a shim under 200 lines, zero changes to `window.ring` or the rail, at least 4 of 5 chosen collaborative apps converge on two nodes. The catalog is 217 entries, a handful collaborative, none with licence metadata. Off the critical path. |
| Hardened JavaScript (SES) | adopt, behind §13's bar | Compartments. Unverified. |
| Temporal | adapt | deterministic replacements; replay tests, which are `diffFold`; recorded results, which are the fulfilment act |
| Syncthing | adapt the model | folder = ring, device list = roster, one address book per node |
| iroh pkarr | adopt | already wired |
| Delta Chat SecureJoin | adapt the shape | §16 |
| RFC 8785 (JCS) | adapt | a dev-only oracle in a property test of the rail's canonicaliser |
| Keyhive / BeeKEM | **adopt the rules, not the code** | re-add must causally succeed the removal; revocation names a delegation by hash; the ephemeral group root key (a key that no longer exists cannot backdate). Pre-alpha, self-labelled not for production, no audit, and its seniority rule has no definition in its own design docs. BeeKEM's key-agreement half now has proofs (eprint 2026/1434); the access-control half does not. |
| MLS (RFC 9420) | defer, and the trigger is sharper | The Delivery Service is NOT required as a server (RFC 9750 §2.2, §5.2.2) but IS required as a function: "the group must agree on a single Commit that ends each epoch". That is a total order on membership ops. Take instead §12.3's remove-absorbs-by-type-priority — Updates and Removes apply "in any order", Removes before Adds — which is a commutative merge rule with no sort in it. |
| **p2panda-auth** | **adopt — read before writing the resolver** | The reference implementation of this design, shipped (Rust, v0.7.x). Causal-Length Set membership; pluggable resolver; strong-removal rules; no seniority, no timestamp, no hash tie-break. Their own caveat: mutual removal in a two-manager group freezes it permanently. |
| Causal-Length Set (Yu & Rostad, PaPoC '20) | **adopt as the membership primitive** | One natural number per element merged by `max`; odd present, even absent. Unlimited add/remove/re-add, permutation-invariant by construction, no tombstone permanence. |
| OpenSSH KRL serial ranges | **cite as prior art for the cut** | `PROTOCOL.krl` §2.2: a revocation range scoped to one CA key, merged by set union, so two cuts compose by minimum with no shared order. `Remove{key, through_seq}` is this shape. Shipped 2013. The commutativity ARGUMENT is still unclaimed and is ours. |
| PKISN (arXiv 1601.03874) §3.3 | **adopt the rule** | A revoked issuer may not issue further revocations, "otherwise an adversary could cause collateral damage." Published structural block on the retaliatory-backdating attack. A cut party cannot cut back. |
| Radicle / radicle-link | **adopt the shape** for the ring id | RID = hash of the INITIAL identity document, so "the document is able to change while the RID remains the same." Ring id = hash of the genesis seed: stable, nobody's key, derivable from the log, survives the founder leaving, renders as a hostname label. Keep their quorum-of-the-PREVIOUS-revision rule, which is what stops a captured majority rewriting history. |
| CT witness cosigning / SUNDR / PeerReview / MINGLE | **adopt the mechanism CT abandoned** | Google's stated reasons for dropping client gossip — "it isn't clear how clients would find each other" and "client-to-client communication isn't scalable" — are facts about the web's topology, not the mechanism. At 25 named mutually-reachable members: 300 pairs, ~112-byte checkpoints, a full all-pairs round under 500 KB. PeerReview puts O(N²) as affordable to hundreds. SUNDR: fork consistency is "the strongest notion of integrity possible without on-line trusted parties." |
| Matrix state resolution v2 / v2.1 | **reject** | Under flat authority Matrix cannot remove anyone: kick, ban and demote each require the target's power to be strictly LESS than the sender's, so the act is unissuable, not merely badly resolved. Room v12's answer to the same pressure was an un-demotable creator with infinite power. Take only PDU check 4 — an act is authorized against the acts it cites, a pure order-free predicate — and the 2.0→2.1 lesson that an agreed set is a final overwrite, never a replay base. |
| SSB group exclusion | **cite the failure** | "Removing a peer is impossible under the assumptions we operate with"; the spec calls its own scheme an "illusion of group member removal", because membership there is possession of a shared symmetric secret. The rule: if you want revocation, authority must never be a shared secret. Our per-actor keys already satisfy it. |
| Tahoe-LAFS | **cite the limit, in our own docs** | Their revocation section: deep-copy-and-re-delegate is "the strongest form of revocation that can be accomplished", and anyone still inside can proxy for the excluded party. A void can withdraw standing; it cannot withdraw knowledge. Say so rather than implying otherwise. |
| `git replace` / `git notes` | **cite the failure** | A genuine void-without-erase primitive — original addressable, override is itself data, escape hatch to the raw truth — whose flaw is fatal here: `refs/replace/` is excluded from pack transfer, so the void does not replicate. A void outside the replicated set is not a void. Our `Correct` being a journal act is right for a reason git demonstrates the hard way. Note also that git's own argument for revert-over-rewrite is coordination cost, never auditability; that argument is ours to make. |
| Datomic's transactor | **cite as why coordinator-free is sound here** | Halloway, on the record: "if we removed the code that manages HA, you could have N transactors. Semantics would be fine but perf would be terrible", and the storage CAS "is still the gatekeeper". The transactor exists for cross-entity invariants on the write path — uniqueness checks, transaction functions, CAS. A ring has none, which is why we need none. The day an app wants one, it wants a serializer, and §11 question 1 already says to refuse it. |
| ERA (PaPoC '26) + CALM / Jacob & Hartenstein | **read — this is the impossibility** | ERA §3.2: genuine mutual removal and retaliatory backdating "are structurally identical. No peer can distinguish these cases from the DAG alone… external information is required." Safety P3: no user may influence a conflict they manufactured; add-wins and remove-wins both violate it. CALM gives the spine — revocation is what makes an authorized fold non-monotone, hence not order-invariant. The escape every shipped system takes is to stop asking "was the remover authorized at that moment": remove-wins set arithmetic commutes because it never asks. |

## 19. Order of work

**Amended 2026-09-20 by the precedent review in §18. Two findings reorder it.**

**A new step 0, and it is a prerequisite rather than an enhancement.** An act
commits to nothing but itself: `ring_op_message` signs
`(namespace, ts_unix, actor, seq, body_json)` and there is no `prev` and no
heads (`rail-core/src/sig.rs:72-87`). (Partly overtaken by 2026-10-02: the
signed body now carries the author's view digest, per-actor chain heads,
`admit.rs:661-687`; nothing reads it beyond verification yet — `RING_SPEC.md`
U3.) Two consequences compound. Equivocation —
one actor signing two different acts at one `seq` and showing each to half the
ring — produces identical per-actor counters on both sides and is caught only
where some node happens to hold both; nothing forces that. And Jacob &
Hartenstein (PaPoC '24) show why that is not merely an integrity gap:
"a Byzantine replica can equivocate by skipping the inflation of logical time
to assign the same logical timestamp to different events", so a cut stated as
`Remove{key, through_seq}` is **sound only on a chain known to be
unequivocal**. Fork detection is a precondition for the removal semantics, not
an adjacent feature. Make every act commit to its author's view — the ring's
DAG heads, under 100 bytes regardless of journal size (Kleppmann, PaPoC '22
§3.2) — before writing step 3.

One thing already holds and should not be lost: `RingJournal::ingest` dedupes on
`op.id`, which hashes the whole line including the signature, so two acts at one
`seq` both land; `ops_missing_from` is author-blind and republishes what the node
holds; and `admit` excludes both (`admit.rs:392`). That is Blocklace's rule —
store it, exclude it, gossip it — already satisfied, and it must survive any
change to ingest. Dropping the duplicate on arrival would make a node that saw
only one act unable ever to reach the state that excludes both, and the
divergence would be permanent.

**Drop "the ring always has an admin" as an invariant.** Minimum-cardinality
invariants are the canonical shape that cannot be held coordination-free, and
p2panda is honest that a mutual removal in a two-manager group freezes it
permanently. CoCoA (ASIACRYPT '22 §3.5) argues the destructiveness is the point:
both removals taking effect is desirable "so users could not avoid being removed
by issuing removals of other parties." State it as a consequence rather than
discover it.

1. CSP from `serve_file`; views as structure.
2. The dial-by-key measurement; sync by roster; `member_check`.
3. `Admit` and `Remove` — after step 0 above, and against `p2panda-auth` rather
   than from scratch. The open question is not convergence, which a
   Causal-Length Set gives for free; it is that ERA proves no DAG-computable
   rule can stop a manufactured conflict, so the choice is seniority (permanent
   concentration), an arbiter (not decentralised), or accepting mutual
   destruction with PKISN's block — a cut party may not cut back — and
   reporting the fork rather than resolving it.
4. wasm signing against golden vectors; client-signed append; the one invite
   with a use count; `ring invite` and the desktop button.
5. Compartments and the deterministic clock.
6. The HTTPS door.
7. Reads gated by roster, then encryption.

The library extraction from `ring-doc` (Part 1 section 10) runs beside 1 to 3;
it touches no file they touch. §20 to §22 carry their own order and reorder
nothing above; all lean on steps 0, 2 and 3.

## 20. Held, lent, carried

Added 2026-10-02, from two operator directions: treat a server like any other
node, and let distributed inference and shared knowledge land in the ring world
rather than beside it. Three kinds of thing reach a ring, and each answers
"what happens when the member who brought it leaves" differently. Most
disagreements about servers, models and corpora are disagreements about which
kind something is.

| | What it is | Examples | Lives | When its member leaves |
|---|---|---|---|---|
| Held | a fold over the journal | ledger, roster, decisions, rota, the index of the journal | every member's node | stays |
| Lent | something a member's node offers to a ring | a media library, an app, a model or a pooled one, a corpus built from their own shelf, batch compute | the lender's node | goes with them |
| Carried | a pure artefact addressed by hash | app bundles, recipes, profiles, public corpus snapshots, seeds, distilled articles | wherever it was installed | everyone who has it keeps it |

Part 1 is the library for held things. This section is the other two. §22
restates all three as a town and adds the case this section lacks: a lent
shop protected without becoming held.

**A server is a member's machine.** Core sentence 2 holds as written. A homelab
box or a rented VPS holding a member's key is one of that member's machines — a
second key under the same person, which the roster already models (§16). It is
not the keeper §17 withdrew: the keeper was a party outside the ring admitted
to hold the journal, and the line between the two is whose key it is, not the
hardware. Nothing a ring holds may live only on a server; that is what makes it
a member and not a hub.

**The node owns what it runs; the ring owns who is in** (ARCH principle 12).
Declarative operators get the first half whole. A NixOS module — unbuilt; there
is no `.nix` file in this repository — would declare the node's key from a
secrets store, each service, and which rings each is lent to. It never declares
membership; that is an `Admit` a person writes. Publication follows the unit's
life: wrapping a unit's command in `svrn run --as <name> --port <p> --` holds a
claim that is renewed while the child lives and retaken after a daemon restart
(`sovereign-cli-mesh/src/run_cmd.rs:222-236`). A stopped unit stops being lent
within its TTL, and the durable tier `docs/PUBLISH_AN_APP.md` warns only
accumulates is not needed.

**One question decides reach: is the caller on the roster of a ring this was
lent to?** Today seven things answer it separately:

- `[iroh] app_allow`, `media_allow` and `offer_allow`: names or node prefixes,
  empty meaning every member of the active mesh (`docs/PUBLISH_AN_APP.md`, "Who
  may reach it");
- the knowledge serving route, which admits any member the iroh acceptor
  verified and reads no per-corpus sharing flag
  (`sovereign-daemon/src/routes_internal/knowledge.rs`; no hit for `sharing`,
  2026-10-02);
- `allowed_peers` on peer-assisted ingest;
- the rail's default roster: every ring nobody narrowed admits everyone in the
  mesh, the `inference` namespace included
  (`commonwealth-rails/src/rail.rs:120`);
- the work plane's hand-written `roster.json` (`sovereign/deploy/mesh/WORK_PLANE.md`,
  "One namespace").

`OriginKind`'s separate trust classes are right — lending the chore app is not
lending the film library (`oicp-types/src/origin.rs:29`) — so the change is not
one list. Each lend names the rings it goes to, and the membership function
`admit` already calls says who is in them (ARCH principle 8). That depends on
§15: a server lending to a house, a band and a friend group needs every ring
live at once and `member_check` widened to any roster the node holds.

**Legacy services keep their users; the roster decides them.** Cheapest first:

1. *Native.* The app reads `X-Mesh-Member`. Built (`docs/PUBLISH_AN_APP.md`).
2. *Injected.* The lending node attaches the service's own credential to
   forwarded requests, so no member ever holds it (`svrn mesh media declare`,
   `commonwealth-transport/src/iroh_identity_forward.rs`). Built, as one shared
   read-only viewer (`sovereign-cli-mesh/src/mesh_media/viewer.rs`). Many
   self-hosted apps accept a trusted proxy header as the user; mapping
   `X-Mesh-Member` onto it gives per-person identity with no provisioner. From
   memory — verify per app.
3. *Provisioned.* An account per member, created on `Admit` and disabled on
   `Remove` (`RING_ENTRY.md`, decision 4). This is §5's `pending` one layer down
   — desired accounts from the roster, minus the accounts the service reports —
   the reconciliation shape NixOS activation and a Kubernetes controller already
   use. The performer is the node that runs the service, because that node owns
   the user table; when it goes, the accounts go with it, so nothing needs
   reassigning. A provisioner is a tool brick (`LEGO_KIT.md`), not a seventh
   kind. One on a node other than the roster's host waits on §19 steps 0 and 3.

**Duties go to an awake member.** The chat bridge, invite minting, the Elder's
engine and a relay each run on some member's node. `RING_APPLICATIONS.md`
already says "a club mints on its always-on member" and "a relay is a member
whose machine stays on." Choosing among awake members is a candidate for
`kernel_types::partition::rendezvous_owner` (`kernel-types/src/partition.rs:65`)
over the awake set; unmeasured. A server wins by staying awake and is
replaceable in each.

**Inference is lent.** A node's OICP manifest is what it lends.
`LAZY_INFERENCE_ON_THE_RAIL.md` moves it onto the journal under
`INFERENCE_APP_ID`, written on change; the step here is to scope that namespace
by ring rather than by mesh. Two members pooling a model neither holds is a
joint lend. The decode turn stays request and response on the data plane,
routed by the existing `rank()` — "a decode turn is not a job and must never
become one" (`WORK_PLANE.md`, "The axiom"). The journal carries what is lent,
never the turns.

**Knowledge is all three.**

- *The ring's memory is held.* A corpus built from the journal is a cache of a
  fold — a function of the journal, a recipe and an embedding model — so any
  member with a model can rebuild it and nobody owns it. It needs the journal
  acquirer `STRATEGY_RINGS.md` names; nothing under `corpus-engine/src` reads a
  `RingJournal` (checked 2026-10-01). The acquirer indexes what the fold shows:
  a corrected act appears corrected, and the struck-through original is reached
  only when someone asks what happened. Acts are typed records already (§4), so
  only their free text needs a model. The ring's recipe pins the embedding
  model, or two members' indexes of one journal disagree; the refusal that
  should catch a mismatch, `EmbedModelMismatch`, has no constructor
  (`WORK_PLANE.md`, audit row 4).
- *A member's shelf is lent.* A query reaches it and passages come back with
  citations; the files stay. Passages are copies of parts, and the lend says so.
- *Public knowledge is carried.* A snapshot published with a sha256 manifest is
  installed, or asked of a member who lends it.

**Recipes and profiles are carried.** Each is code addressed by hash that turns
a source into derived state — to a corpus what a bundle is to a ring's state.
Core sentence 5 extends to them, and `LEGO_KIT.md` step 3 (one distribution
mechanism, signed by the author's ring key) is how; today there are three
(`meshapp publish|install`, snapshot manifests, the recipe registry).
`FIVE_PROGRAMS.md` §11.3 already draws the line: "Drafted packages, never
content, go back to the bank."

**An app reaches a model through `pending`.** A fold cannot call a model: it
runs on every node and again on every replay. `pending` returns an `ask`; the
member lending inference performs it; the fulfilment act carries the
`kernel_types::Answer` — claims, citations, verdict — and the model's identity.
Every node folds that one answer instead of asking its own model and getting
another. This is §18's Temporal row ("recorded results, which are the
fulfilment act") and the first demand on §12's performer. Only answers the ring
keeps travel this way; a chat turn does not. A recorded answer is evidence,
labelled as written by a model, and never a decision (§21).

**The mesh becomes a view.** When membership lives only in rings, the mesh is
not a set anyone joins. It is everything lent to the rings a key belongs to —
§15's "a mesh shrinks toward what it owns — lending", finished.

**Bars, proposed before any of it is built.**

- *Revocation.* One `Remove` ends a member's reach to every kind of lend — app,
  media, corpus, inference, work — within sixty seconds. Watched failing first
  with any one decider left on its own list.
- *Switch-off.* Turn off a lender. The ring keeps everything it holds; the Elder
  answers from the journal through another member's model; anything that needed
  the lent thing comes back as an asleep row, never as a smaller answer that
  does not say so. Watched failing with the held index planted only on the
  server.
- *Seam.* A NixOS module lends a service to a ring with no change to the daemon.

**Order.** (1) The journal acquirer. (2) Lends name a ring — apps, media and
offers first, then the knowledge route, inference, work. (3)
`LAZY_INFERENCE_ON_THE_RAIL.md` as written, then its write-on-change row for
every kind of lend, replacing the manifest pull, the config tier and claims as
three ways to advertise. (4) One fan-out: `commonwealth_media::fanout` and the
knowledge fan-out are two implementations of "ask every lender, one row each".
(5) The performer, with `ask` first. (6) One distribution path for carried
artefacts. Multi-node rings wait on §19 steps 0 and 3; at M0 a lend from the
host works against the host's roster.

**Naming, owed first.** "Offer" already means two things (`OriginKind::Offer`,
`WorkAct::Offer`) and an inference capability would make a third
(`LAZY_INFERENCE_ON_THE_RAIL.md`, "Naming"). If "lend" is the word, settle it
once with `sovereign code converge noun` before it spreads.

## 21. Gestures and gifts — two gates

Added 2026-10-02 with `STRATEGY_RINGS.md` "Commons, gift, inheritance", which
holds the argument. Two of its rules are guarantees code can hold, so they are
gates here (ARCH principle 10).

**No surface returns a value about a member.** Stance 7, owed as a gate since
the strategy was drafted. Its first subject is built: `LedgerEventKind`
(`oicp-types/src/contributions.rs:38`) records inference served,
knowledge queries served, bytes moved and work units completed, and `aggregate`
(`commonwealth-core/src/contributions.rs:55`) sums them per node — per person, on a mesh where a node is a person.
Between strangers pooling compute that is bookkeeping and may stay for routing.
Inside a ring it renders nowhere. The gate: a planted surface returning a
per-member series fails the build.

**A gesture needs a person.** App act kinds that are gestures — thanks, vouch,
introduction, congratulation — say so in their one declaration (§4), and an act
of a gesture kind carries a person's confirmation in its provenance. An act a
model proposed and no person confirmed is refused, not flagged, beside
`GovernanceIssue::UnattendedAct`
(`corpus-engine-atlas-reader/src/governance_view.rs:611`). Watched failing first
with a planted auto-confirmed vouch. Guest grade passes, because the bridge
signs what a person tapped (`RING_ENTRY.md`, decision 2). `Admit` is not an app
kind and is not covered here: §9's one rule already makes it a member's act,
and a bridged `Admit` rests on the human `Admit` of the bridge.

**What the library owes both.** The `ledger` reducer folds splits — money the
members agreed to share. A gift is not a split with a flag: it is a `thanks`
act, signed by the person who received it and pointing at what it thanks, and
the library ships no reducer that sums thanks per person. And the maker is
shown wherever an app runs, which costs nothing once bundles are signed by
their author's key (`LEGO_KIT.md` step 3; today a bundle carries a hash, not an
author, §14).

## 22. The town — register, shops, deeds

*Naming and the manifest are superseded by `RING_SPEC.md` §3.3–§4: the shop's
runner is its **host**, `Keeper` is `ring-apps` rung `ra-7`'s copy-holding role
(which an insurer is), and there is one `[app]` manifest for pages and shops.
The text below keeps its original words.*

Added 2026-10-02, from two operator directions: a ring is what is offered on
it — a digital town, open when its members are and closed when they are not —
and members must be able to protect the shops they do not want to lose. This
is the frame §20 and Part 1 sit inside. §20's *held* are the town's civic
books, its *lent* are the shops, and insurance is the case §20 lacked. Whoever
writes a shop and whoever uses one specifies all of it without knowing the
rail.

**The model.** A ring is a town: a register of who is in, and the shops its
members open for each other — a film library, a tool-lending list, a Minecraft
server, the house model, a corpus. Each shop has a keeper and runs on the
keeper's machine. The town is open while keepers are awake and closed when
nobody is (Part 2, "Always there": "this is the design"). Members arrive from
the chat they already use (`RING_ENTRY.md`) and walk into any shop with their
identity.

**Two kinds of state, and every shop declares one.**

- `writer = "ring"` — a *civic book*. State is a fold on the ring's log
  (Part 1). Every member who syncs holds it, any member's node can serve it, and
  writes from anyone converge. The register, the treasury
  (`docs/HOUSE_EXPENSES.md`) and the deeds below are civic books. Insurance is
  automatic.
- `writer = "keeper"` — a *shop*. State lives on the keeper's disk and has one
  writer at a time. Any server that reads `X-Mesh-Member` is one
  (`docs/PUBLISH_AN_APP.md`). It leaves with its keeper unless it is insured.

The rule for choosing: anything that must keep taking writes while its keeper
sleeps is a civic book. Everything else may be a shop.

**The register is built at the rail.** `Admit` and `Remove` are rail acts
(`commonwealth-rail-core/src/lib.rs:346`, `:364`), and a ring nobody narrowed
admits everyone in the mesh (`commonwealth-rails/src/rail.rs:120`). Not built:
the register belonging to a ring rather than the mesh, the bridge that feeds
it from a chat (`RING_ENTRY.md`), and reach decided by it (§20).

**Four parties, each declaring what it owns** (ARCH principle 12).

| Party | Declares | Where it lives |
|---|---|---|
| Author | what the shop is: its code by hash, its writer, its state, how to copy and restore that state, whether it can be served read-only | the shop manifest, carried with the code |
| Keeper | that they keep it and whether it may be insured; handing it over; releasing it | deed acts on the ring's log |
| Member | insuring a shop or withdrawing; taking a shop in first refusal, or passing | deed acts |
| Ring | how long a shop may be unreachable before anyone may open first refusal, and how long each insurer's turn lasts | a civic setting, written as an act |

**The shop manifest** is the author's: a TOML file beside the code, the shape
recipes already use (`LEGO_KIT.md`, "Assemblies are TOML manifests").

```toml
[shop]
name   = "tools"
code   = "git+<url>#<rev>"      # or an image digest, a flake ref, a bundle hash
run    = ["python", "app.py"]   # handed PORT, as `svrn run` does
writer = "keeper"               # or "ring"

[shop.state]                    # writer = "keeper" only
path      = "data/"
snapshot  = ["sqlite3", "data/tools.db", ".backup $OUT/tools.db"]   # optional
restore   = ["cp", "$IN/tools.db", "data/tools.db"]                   # optional
read_only = "methods"           # or "none": cannot be served read-only
```

`code` is what makes a shop insurable: the insurer runs exactly what the keeper
ran. A pinned git rev is the precedent the work plane already uses
(`process:v1`, `sovereign/deploy/mesh/WORK_PLANE.md`). Without `snapshot`, the
state is copied only at a handover, when the shop is stopped; a copy of a
running database is torn, so continuous insurance needs the command.
`read_only = "methods"` lets an insurer's door serve `GET` and `HEAD` and
refuse every other method, with no change to the app. A shop that writes on a
`GET` says `"none"`, and is closed rather than read-only while its keeper
sleeps.

**The deeds** are a civic book, folded with Part 1's library and judged by its
five laws.

| Act | By | Effect |
|---|---|---|
| `Open{shop, manifest, insurable}` | keeper | opens or updates a shop under the keeper's key; the latest per keeper wins, as the work plane's `Offer` does |
| `Insure{shop}`, `Withdraw{shop}` | member | opts in or out, valid only while the shop is insurable; the rail's order of `Insure` acts is the order of first refusal |
| `Handover{shop, to, snapshot}` | keeper | names an insurer as the next keeper, with the hash of the final copy |
| `Release{shop}` | keeper | gives the shop up without naming anyone; opens first refusal |
| `Refusal{shop}` | any member | opens first refusal on a shop unreachable longer than the ring's period; any later act by the keeper on that shop closes it |
| `Take{shop, snapshot}`, `Pass{shop}` | insurer | accepts the shop in its turn, or passes it on early |

A keeper's `Remove` from the register opens first refusal on every shop they
keep. Turns are computed in the fold from the `op.ts_unix` of the act that
opened first refusal — insurer *i*'s turn starts after *i* windows unless those
before passed — so no clock but the rail's is read (§2). Whether a shop is
awake is not in the fold. Liveness comes from the keeper's claim, the TTL that
`svrn run` holds, never from the log, which carries no heartbeats
(`LAZY_INFERENCE_ON_THE_RAIL.md`, gotchas 1 and 2).

**What the town does.**

- *Keeper awake.* The keeper serves and takes writes; copies ship to insurers
  on change, addressed by hash.
- *Keeper asleep.* An insurer's node serves the last copy read-only, labelled
  with its age — "Dave's tool library, from Mia's copy, as of 9:14" — and
  refuses writes, naming the keeper.
- *Handover.* The keeper stops the shop, takes the final copy and writes
  `Handover` with its hash; the new keeper restores and opens. No writes are
  lost.
- *Release, removal or refusal.* Insurers are offered the shop in `Insure`
  order, one window each. The first `Take` naming the latest copy's hash
  becomes keeper. With no taker the shop goes dormant: every insurer keeps the
  last copy, and the town lists the shop as closed. It is never silently
  deleted.
- *A keeper returns after a `Take`.* Their node folds the deeds before serving,
  finds it no longer keeps the shop, refuses writes, and offers to insure the
  new keeper.

**One writer.** A node accepts writes for a shop only while the deeds name its
key as keeper. The right does not lapse on a timer: the work plane's lease
(`oicp-types/src/work/mod.rs:42`) is not reused, because renewing it would put
heartbeats on the log. It changes only by `Handover`, `Release`, `Remove` or
`Take`. The residual case is a keeper cut off from the ring yet still serving
someone on its own network while a `Take` happens elsewhere. Writes in that
window land on a copy the town has left; they are reported when the node
rejoins, never merged.

**What can be lost, said plainly.** An insured shop whose keeper's disk dies
without a handover loses the writes since its last copy. A civic book loses
nothing any member synced. A media library's files insure by the same
mechanism, and their size is shown before anyone opts in.

**The clerk's part, and the gift's.** When a keeper's leaving is in view — a
`Release`, a `Remove` proposed, "moving out" said in the chat — the clerk lists
their uninsured shops to the house and asks who will keep them. Noticing is the
chore; `Insure` and `Take` are a person's tap. The shop shows "insured by Mia":
recognised, never tallied (§21).

**Bars, set before any of it is built.**

- *Keeper off.* An insured shop answers a `GET` from the insurer, labelled with
  the copy's age, and refuses a `POST` naming the keeper. Watched failing with
  insurance off, and again with `read_only` ignored.
- *Handover.* Zero writes lost across a handover under a steady write load.
- *One writer.* No node the deeds do not name accepts a write. Watched failing
  with the check disabled.
- *First refusal.* A keeper is removed; insurers are offered the shop in
  `Insure` order; with no taker, the last copy survives on every insurer.
- *Generality.* Three shops — a one-file Flask app on sqlite, a fold app, and a
  stock self-hosted server with a declared state directory — insured by
  manifest alone, with no change to any of them.

**Order.** (1) The manifest, read by the runner `svrn run` is today. (2) The
deeds fold, the library's first customer beyond expenses. (3) Copy shipping
and the insurer's read-only door. (4) Handover. (5) First refusal. (6) The
clerk's notice. Reach decided by the ring's register (§20, order 2) comes first
for any of it to work across rings.

**Names.** Shop, Deed, Keeper and Insure are defined nowhere in the workspace
(`sovereign code converge noun`, 2026-10-02). They are free; mint each once.
