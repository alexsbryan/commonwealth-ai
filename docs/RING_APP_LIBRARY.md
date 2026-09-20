# The ring app library — a design from first principles

> **DRAFT — not in force (2026-09-20).** A design record for the library a
> ring app is written against. It supersedes nothing. What a ring is *for*
> lives in `docs/internal/RING_APPLICATIONS.md` (per-host, untracked); the
> primitive inventory lives in `quality/campaigns/ring-apps.toml`.

**Scope: the library and nothing above it.** Every export is a pure function,
a plain value, or a property test. The library performs no I/O, holds no
grant, knows no roster, and runs under `node --test` with no daemon present.
Who may write an act, who may perform an effect, which version of an app a
ring runs, how code reaches a machine, and how a page is isolated are
authorization, governance and distribution. They are layers above, they are
not designed here, and section 9 names the two seams they attach to.

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
is looking, the local roster, completeness, the live lane. The library fixes
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

## 9. The two seams

The fold sits in the middle. Everything above attaches at one of two places
and the library is unaware of both.

**Acts in.** The library folds whatever sequence it is handed. Deciding which
acts count — beyond the rail's own admission — is a filter applied before the
fold.

**Effects out.** The library returns descriptions. Deciding which are
performed, by whom, and under what authority is a policy applied after
`pending`.

Deferred to those layers, recorded so they are not lost: who may perform or
fulfil an effect; reassigning an effect whose performer never returns; who
may deploy; that two nodes on different reducers diverge, so the choice of
reducer must itself be ordered; pinning library code by hash; enforcing
determinism at run time rather than only testing it; origin isolation between
rings.

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
