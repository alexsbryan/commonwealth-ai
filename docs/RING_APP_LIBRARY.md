# The ring app library — a design from first principles

> **DRAFT — not in force (2026-09-20).** A design record for the library and
> SDK layer a ring app is written against. It supersedes nothing. What a ring
> is *for* lives in `docs/internal/RING_APPLICATIONS.md` (per-host, untracked);
> the primitive inventory lives in `quality/campaigns/ring-apps.toml`. This
> document owns one question those two do not: what shape the code an app
> author writes should have, and why that shape is forced rather than chosen.

The method is derivation. Start from what the rail already guarantees and
cannot take back, and ask what each guarantee forces on the layer above it.
Where the derivation lands on the Elm architecture, that is a result, not a
starting preference; where it departs from Elm, the departure is the
interesting part and is marked.

## 1. What is given

Three facts about the substrate, none of them negotiable.

**G1. The log is the only shared thing.** A ring is a namespace; its state is
an append-only journal of signed acts. The rail computes one total order and
one void set, server-side, and every node that holds the same acts derives the
same order. It may hold fewer: `log()` answers with `complete`, and a journal
with gaps is a subset, reported as one.

**G2. Every node computes alone.** There is no coordinator, no primary, no
server the app can ask what the state is. Each node folds its own copy.

**G3. The author is increasingly a model, and the reader is a person.** Code
is written by an agent from a sentence, verified by a machine, and used by
someone who will never read it.

As built, the whole client surface is `window.ring`: `namespace`, `log`,
`record`, `correct`, `live.send`, `live.drain`, `fold`
(`sovereign/crates/sovereign-daemon/src/guest_door.rs:253`). `fold` only
traverses — it skips voided acts and replacement-less corrections and walks
the rest in the rail's order (`:323`). Ordering and voiding are decided once,
in `commonwealth-rail-core`'s `admit`, and the page never re-derives them.
That is one decider, and this design keeps it that way.

## 2. What the givens force

**State is a fold.** G1 and G2 together leave one way for N nodes to agree
without talking: each computes `state = fold(reduce, initial, log)` with the
same `reduce`. Any state that is not a function of the log is state two
housemates can disagree about. The reducer is not an architectural taste
borrowed from Redux; it is the only shape that converges.

**The reducer sees three things and nothing else.** Convergence requires
`reduce(acc, payload, op)` to be a function of its arguments. The clock,
randomness, the locale, floating point, iteration order of an unordered
container, the network, and — less obviously — the roster are all inputs that
differ between nodes. `roster.json` is a per-node file; two members hold
different ones. So the roster may shape what a person *sees* and never what
the state *is*. `op.ts_unix` is the only clock.

**Acts are forever, so the reducer is a function of every version.** Nothing
leaves an append-only log. An app's second release folds its first release's
acts. Schema evolution is therefore part of the reducer's type, not a
migration run once.

**State is always state-as-of-a-possibly-partial-log.** Incompleteness is a
normal condition, so it travels with the state rather than beside it. Absence
is reported, never defaulted (ARCH principle 6).

**Effects cannot live in the fold.** The fold runs on every node and again on
every reload. An effect inside it fires N times, and then again tomorrow.
Effects must be outside, and section 5 derives where.

## 3. The program

An app is a record of pure functions. The runtime owns the loop.

```
app = {
  initial : State
  reduce  : (State, Payload, Op) -> State        // the fold; converged
  pending : State -> [Effect]                    // what the state still wants
  view    : (State, Ctx) -> View                 // local; never converged
}

Ctx = { me, members, complete, gaps, live }
ring.run(app)
```

`ring.run` loads the log, folds, renders, performs the pending effects this
node owns, and repeats on every new act. The app never calls `fetch`, never
calls `log()`, never schedules anything. Today the template does that loop by
hand in 135 lines of `app.js`; under `run` that file is the `view`.

The split between `reduce` and `view` is the split between what converges and
what does not. `me` and `members` reach `view` through `Ctx` and are
structurally absent from `reduce`. The template's header warns in prose that a
split must never read "everyone in the ring"; here there is nothing to read.

Against Elm: `reduce` is `update`, `view` is `view`, the live lane and the log
tail are `subscriptions`. `pending` stands where `Cmd` stands, and it is not
the same thing — section 5.

## 4. The library is an algebra

SICP's test for a language: primitives, a means of combination, a means of
abstraction, and closure — combining things yields a thing of the same kind.
The library plane contains exactly two kinds of value.

**Reducers.** `ledger`, `doc`, `poll`, `presence`, `rota`. Each is a complete,
tested `reduce` with its `initial`.

**Functions from reducers to reducers.**

| Wrapper | What it removes from app code |
|---|---|
| `validated(schema)` | an invalid act becomes a gap, never a throw |
| `once(keyFn)` | two acts with one idempotency key collapse to the first |
| `upcast(migrations)` | old versions of an act are lifted before `reduce` sees them |
| `byKind({...})` | dispatch on `payload.kind`; unknown kinds are counted, not fatal |
| `combine({a, b})` | two apps are one app |

Every result is a reducer, so every result can be wrapped, combined, tested
and deployed the same way. `ring-doc` is a document plus an expense book; it
got there by copying `expenses.js` and its test verbatim, 491 lines, because
there was no `combine` and no way to import. With closure that is one import
and one call.

**The envelope is fixed.** Wrappers need somewhere to report, and the Express
lesson is that a mutable grab-bag passed down a chain becomes the framework's
worst property. The accumulator is `{ state, gaps }` and nothing else;
wrappers append to `gaps` and never add fields.

**Laws, each a property test the SDK ships.**

1. *Determinism.* Folding one log twice gives deep-equal state.
2. *Environment independence.* The same fold under a different clock, locale,
   random seed and roster gives deep-equal state.
3. *Non-interference.* `combine({a, b})` projected to `a` equals `a` folded
   alone. `combine` refuses, at construction, two children declaring one kind.
4. *Idempotence.* Under `once`, appending a duplicate-keyed act changes
   nothing.
5. *Totality.* Under `validated`, no payload makes `reduce` throw.

Determinism is closed under composition — a deterministic function of a
totally ordered sequence, composed with another, is one — so an app assembled
from library parts passes laws 1 and 2 by construction, and the suite confirms
it rather than hoping. An author gets convergence testing without writing any.

**One declaration per act kind.** Redux's cost was saying "expense" in five
places. Here a kind is declared once —

```
kind("expense", schema, (state, act) => ...)
```

— and the creator, the validator, the reducer branch and the form description
are derived from it. This is porcelain over the bare `reduce`; the plumbing
stays usable without it.

## 5. Effects — where this is not Elm

Elm's `update` returns `(Model, Cmd)`. That works because there is one runtime
and each message is processed once. Here `reduce` runs on every node and on
every replay, so a command emitted *by a transition* is emitted N times, and
again on reload. Edge-triggered effects cannot be made safe by discipline.

Make them level-triggered instead. An effect is not something a transition
fires; it is something the *state* still wants.

```
pending : State -> [Effect]
Effect  = { id, by, run: Ask | Infer | Work | Blob | Notify, ... }
```

`pending` is a pure function of state. An effect's `id` is derived from the
act that called for it. The runtime performs an effect and records the result
as an act keyed by that `id`, under `once`. The next fold sees the fulfilment,
and `pending` stops returning it. "Has this been done?" is answered by the log
itself, so the mechanism survives replay, crash and restart with no separate
bookkeeping — desired minus observed, the reconciler shape.

**Who performs it.** `by` names a key. The default is the author of the act
that called for it, so one node acts and the rest wait. If that node is
offline the effect stays pending, and `view` can say so. Work any member may
do goes through a lease on the work plane that already exists
(`commonwealth-work`: `submit`, `lease`, `complete`). Two nodes racing still
converge: `once` keeps the first fulfilment in the rail's order.

**This is how a model enters a ring app.** `Ask` and `Infer` are effects. The
daemon performs them under the ring's grants; the answer returns as an act
signed by the node that did the work. The model is an actor in the log —
attributed, ordered, and voidable with `correct` — never a side effect hidden
in a reducer. An answer carrying a verdict and citations is just a payload.

The live lane is the one exception and stays one: it is delivery, not record,
reaches `view` through `Ctx.live`, and never enters `State`.

## 6. Determinism is enforced, not requested

A rule that needs a linter is not structural; React's hooks rules are the
cautionary case. `reduce` and `pending` run in a worker whose global scope
has had `Date`, `Math.random`, `fetch`, `Intl` and `crypto.getRandomValues`
removed before app code loads. Law 2 is the second guard and catches what a
frozen scope cannot, such as ordering by an unordered container.

The rail already constrains payloads to JSON objects of whole numbers and
strings, because two nodes must derive identical bytes and JSON makes no
promise about fractions. The library keeps that: `money` is integer cents with
a deterministic remainder rule, and there is no float helper.

## 7. Evolution — the journal is the regression suite

Every act carries `kind` and `v`. `upcast` lifts old versions before `reduce`.
The newest reducer folds the whole log; there is no per-era reducer.

The gate: `ring_test` folds the ring's real journal under the deployed reducer
and under the candidate, and diffs the two states. Every difference must be
declared in the change or the deploy is refused. An agent asked to "add
categories" cannot silently re-divide a year of expenses.

The promise, borrowed from SQLite's file format: a journal written today folds
in 2040. It is kept by golden journals checked into the SDK's own tests.

## 8. Code is part of consensus

Two nodes running different reducers compute different states. That cannot be
prevented, so it is ordered. A deploy is an act: it names the app bundle by
hash and the SDK by hash. Every node runs the version named by the latest
deploy act in the rail's order, so the whole ring switches at one point in the
log. Rollback is `correct` on that act.

It follows that the SDK cannot be fetched by name. The daemon serves it as ES
modules at a hash-pinned, same-origin path; no bundler, no package manager, no
vendoring. `ring-doc`'s 1.17 MB vendored bundle becomes a served module. The
shim, today a string constant in a `.rs` file (`guest_door.rs:253`), becomes
the first of those modules.

## 9. The contract is the wire

The kernel is four rail operations and the act envelope. The JavaScript SDK
is one porcelain over it; a Python agent or a Rust service that speaks the
four operations is as much a ring app as a page is. The kernel is small
enough to freeze, and it is frozen: additions go to the library.

## 10. What the library does not contain

No router, no component framework, no state container other than the fold, no
ORM, no auth, no package manager, and no network access for app code. Views
are userland. The SDK ships a handful of elements bound to fold state — a
schema-driven form, a list, a member picker, a completeness banner — and the
escape hatch is the DOM, since it is only a page.

## 11. Authoring by agent

The shape is deliberately isomorphic to Elm and Redux in every place the
semantics match: `reduce`, `combine`, higher-order wrappers. Models have read
millions of reducers and no ring apps; each borrowed name is authoring skill
that costs nothing. `act` stays `act`, because it is signed and correctable
and an `action` is neither.

The judge is deterministic — the app's tests plus the five laws — which bounds
the search enough for a small local model under the existing `solve` loop. The
authoring surface is one skill and four tools: `ring_new`, `ring_test`,
`ring_deploy`, and `ring_explain(act_id)`, which names the branch that
consumed an act or the gap it became.

## 12. Bars, written before any of this is built

Measured 2026-09-20: the scaffold template is 674 lines (domain 239, tests
252, glue 135, HTML 48). `ring-doc` is 1,732 first-party lines, 491 of them a
verbatim copy of the template's domain and tests, 423 an adapter the shelf
priced at about 50.

- `ring-doc` re-expressed on the extracted library is under 500 first-party
  lines, with its adapter tests passing from inside the SDK.
- Over a fixed bank of ten one-sentence ideas on a named model: median app
  under 400 lines including tests, and at least 7 of 10 pass the five laws on
  first deploy. Below 5 of 10, agent authoring is not the on-ramp.
- The third app needs fewer than 200 lines that are neither domain nor
  library. More, and the plane is cut in the wrong place.

Order of work: extract from `ring-doc` first. It is behaviour-preserving, it
is the inventory speaking (ARCH principle 11), and it yields the first real
measurement. `run`, `pending` and the deploy act follow; the elements last.

## 13. Open questions

1. Who may deploy. A deploy act changes what every member's node executes;
   the roster has no role that says who may write one.
2. Origin isolation. Peer-authored code runs against a member's own daemon.
   The grant is namespace-scoped; whether each ring needs its own origin so
   apps cannot read each other's storage is undecided.
3. Roster-relative gaps. The template passes the roster into the fold
   (`templates/app.js:36`, `expenses.js:194`) and uses it for one thing: an
   `unknown_person` gap, marked `fatal: false` (`expenses.js:93-98`). So
   balances converge and the gap list does not — the author kept the money
   right by hand. Under section 3 that warning is `view`'s, computed from
   `Ctx.members`, and `gaps` in the envelope holds only what the act alone
   decides. Whether any app has a legitimate need for a roster-relative
   *fatal* gap is open; if one does, it cannot converge, and the design should
   refuse it rather than accommodate it.
4. Fold cost. Every load folds the whole journal. A memo keyed on bundle hash
   and log digest is a cost-only optimisation; `seal` and `compact` already
   bound the journal. Not measured at ring scale.
5. An effect whose `by` node never returns. Reassignment is a lease, and a
   lease needs a clock the members agree on; `op.ts_unix` is the candidate.
