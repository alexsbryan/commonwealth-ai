# The ring app library — a design from first principles

> **DRAFT — not in force (2026-09-20).** A design record for the library a
> ring app is written against. It supersedes nothing. What a ring is *for*
> lives in `docs/internal/RING_APPLICATIONS.md` (per-host, untracked); the
> primitive inventory lives in `quality/campaigns/ring-apps.toml`.

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
Under the 2026-09-18 amendment (`docs/internal/RING_APPLICATIONS.md`)
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
than only testing it; origin isolation between rings.

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

Part 1's purity rule stops here. Everything below does I/O. It is drawn as
four layers, each owning one thing about itself (ARCH principle 12), and it
is scoped to what the first ring apps and the spot in
`docs/internal/RING_SPOT.md` demand — nothing is here because it might be
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
- **The test entry.** `node --test`, the five laws, `diffFold` against
  `svrn ring log <ns> --json`.

Out, by demand: the effect performer (no app asks for one — ring-room reaches
the house AI through `chat ask` in its demo script), so `pending` is rendered
as wanted and unperformed, never dropped; and the debounced writer, which has
one caller and stays in the `doc` adapter.

## 13. The sandbox — two layers, neither remembered

Checked 2026-09-20: `templates/app.js:42,57,89` and `ring-doc/app.js:275`
write peer-authored strings through `innerHTML`, and no response on the ring
page path carries a Content-Security-Policy. Any roster member can run script
in every other member's page, where the guest bearer sits in `location.hash`
(note `1f0b0fce`). An `escape()` helper in the template would be a remembered
rule the next generated app forgets.

**The browser's layer.** `serve_file` — the one function both the door and
`ring dev` serve through — sends `default-src 'self'` with no inline script.

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
a subdomain per ring can on an HTTPS door. Undecided.

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

## 17. The HTTPS door

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
| Keyhive, Byzantine eventual consistency | read | behind the one membership function |
| MLS (RFC 9420) | defer | trigger: the keeper node in §17 |

## 19. Order of work

1. CSP from `serve_file`; views as structure.
2. The dial-by-key measurement; sync by roster; `member_check`.
3. `Admit` and `Remove`.
4. wasm signing against golden vectors; client-signed append; the one invite
   with a use count; `ring invite` and the desktop button.
5. Compartments and the deterministic clock.
6. The HTTPS door.
7. Reads gated by roster, then encryption.

The library extraction from `ring-doc` (Part 1 section 10) runs beside 1 to 3;
it touches no file they touch.
