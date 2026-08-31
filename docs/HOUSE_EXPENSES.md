# Split the house expenses

Four people share a house. Somebody buys propane, somebody else buys the
groceries, and at the end of the month one person is owed money by the other
three. The usual answers are a spreadsheet nobody maintains or an account on a
service that owns the record.

This walks the third answer: a small web page, backed by an append-only log
that every housemate keeps a full copy of. Each entry is signed by the person
who wrote it. Every machine applies the entries in the same order, so two
laptops cannot disagree about who owes what. Nothing leaves the machines the
four of you already own, and a housemate who moves out does not take a quarter
of the ledger with them.

The first person scaffolds the app and starts the roster. Everyone else copies
a folder, adds the others' keys, and opens a page.

## Before you start

A daemon, running:

```sh
svrn daemon start
```

If more than one machine is involved, they need to be on a mesh together — one
person runs `svrn mesh create` and reads out the key, everyone else runs `svrn
mesh join <key>`. [Join a mesh](./JOIN_A_MESH.md) covers it. A single machine
works fine for trying this out; you just won't see anything sync.

You'll also want `node` on the path if you plan to change the money rules,
because the rules ship with tests.

## 1. Scaffold the app

```sh
svrn ring new ./house-expenses --name "House Expenses"
```

Four files, and it is worth knowing which is which:

| File | What it is |
|---|---|
| `index.html` | the page — a balance list, an add form, a history, a gap panel |
| `app.js` | the wiring: read the log, fold it, render it. Holds no money rules of its own |
| `expenses.js` | **the money rules.** The split, the penny remainder, the settlement key, the refusals |
| `expenses.test.mjs` | 20 tests over `expenses.js`, run with `node --test` |

That split is the shape of every ring app. The rail underneath hands you signed
entries in one agreed order; `expenses.js` is the layer that decides what an
entry *means*. Build a lending board instead and you replace `expenses.js` and
the form in `index.html`, and `app.js` barely changes.

Re-running `svrn ring new` on an existing directory never overwrites — it says
what is already there and writes nothing.

## 2. Say who is in the ring

An entry signed by a key nobody claims is not an entry, it is a **gap** — the
rail shows it and refuses to count it. So the roster comes first.

On each machine, that person adds themselves:

```sh
svrn ring roster add alex --self --ring house-expenses
# alex → 3f9c1a…de04        (the 64-hex node public key, abbreviated here)
#   ring: house-expenses
#   file: ~/.svrnmesh/rings/house-expenses/roster.json
```

Then everyone reads out the key that printed, and **each person adds the other
three on their own machine**:

```sh
svrn ring roster add sam   --key 7b2e… --ring house-expenses
svrn ring roster add robin --key c40a… --ring house-expenses
svrn ring roster add kit   --key 91df… --ring house-expenses
```

Yes, that is four commands each, times four people. The roster is a local file
and it is deliberately not gossiped: who is in the ring is a decision people
make, and there is no rail route that can change it — so a deployed app cannot
add a key to the ring, including its own. The cost is that you type the keys
once. Check yourself with `svrn ring roster list --ring house-expenses`, which
reports what the *running daemon* loaded rather than what you think you wrote.

There is nothing else to provision. The namespace `house-expenses` comes into
existence on its first write.

## 3. Open it

```sh
svrn ring dev house-expenses --dir ./house-expenses
# ring `house-expenses` is live.
#   bundle : ./house-expenses
#   rail   : http://127.0.0.1:9743
#   open   : http://127.0.0.1:4318/   (Ctrl-C to stop)
```

The dev server holds a grant that reaches this one namespace and nothing else
on the daemon — not `/internal/*`, not the mesh routes, not another ring. The
grant lives in the server process and dies with it; the browser tab never sees
it. If the daemon isn't running, `ring dev` refuses to bind rather than serving
a page that would silently fail to reach its journal.

Add an expense in the form: who paid, how much, what for, and who it splits
between. The balance list fills in — positive means *is owed*, negative means
*owes*.

## 4. What happened underneath

The page called `window.ring.record({...})` with a plain JSON object. The
daemon assigned the sequence number, the timestamp and the id, signed the act
with this node's key, and appended it. **The app cannot choose any of those** —
an app that could pick its own sequence number or actor could write as somebody
else, and the grant would be decorative.

Then the page called `window.ring.log()` and got back every act this node can
account for, already in the order every node applies them, with corrections
already resolved. It folded them with `expenses.reducer`.

Use the fold. Do not iterate `log.ops` yourself:

```js
const book = window.ring.fold(log, expenses.reducer, expenses.initial(log.roster.members));
```

The guarantee is in the traversal, not in the array. `fold` skips the acts a
correction voided; a hand-rolled `ops.filter().sort()` double-counts every
corrected entry, and it looks right until the first correction.

Note `log.roster.members`, not `log.roster`. The rail ships the whole roster
value and the reducer wants the person→keys map inside it. Passing the wrapper
makes `members` look like the ring's only member and every real name look like
a stranger — correct balances beside a page of spurious "not in the roster"
gaps, which reads like a roster problem and is not one. `initial()` throws
rather than accept the wrapper.

One rule about payloads that will bite you otherwise: **whole numbers only.**
`1e2`, `100.0` and `100` are one value with three spellings, two nodes have to
derive identical bytes from an act to verify one signature, and JSON does not
promise that for fractions. Pick a unit and use an integer — the reference app
uses cents. The rail refuses a fraction at the door, names the offending
number, and tells you what to write instead. A payload is also capped at 64 KB
— a journal line is an act, not a file, and every peer keeps a copy forever.

## 5. The second machine

Get the folder to your housemate however you'd get them any folder — a git
remote, a shared drive, `scp`. There is no publish verb yet (see [what M0 does
not do](#what-m0-does-not-do-yet)). They run:

```sh
svrn ring dev house-expenses --dir ./house-expenses
```

Their daemon already has the journal, or gets it within a minute: every node
republishes what it holds to every online peer once a minute, and runs one
round immediately on daemon start. So a housemate whose laptop was shut for a
week opens the page and the missing entries arrive.

Sixty seconds, not ten. Money does not need fast convergence and the bandwidth
belongs to the mesh's other traffic.

## 6. Fix a mistake

Somebody typed $472.00 instead of $47.20. You do not edit the entry — the log
is append-only, and an editable ledger is one where the version you're reading
depends on who you ask.

```js
// `id` comes off the entry itself — every op in `log.ops` carries one.
await window.ring.correct(op.id, correctedPayload);   // omit the second argument to simply withdraw it
```

The void is **permanent**. Correcting a correction cancels its replacement and
leaves the original gone; to bring something back, write it again. That rule
lives in the rail rather than in your app, because "this act was wrong and it
never comes back" is generic and is the rule most easily got wrong — the void
set has to be built from every correction at once, never by walking the chain
for liveness.

The voided entry stays visible in the history, struck through. Showing the
correction without the thing it corrected leaves a reader unable to check the
change.

## 7. Settle up

At the end of the month, Sam pays Alex $80. That is an act too:

```js
import { SETTLE, settleKey, today } from "./expenses.js";

const day = today();
await window.ring.record({
  kind: SETTLE,
  from: "sam",
  to: "alex",
  amount_cents: 8000,
  key: settleKey("sam", "alex", 8000, day),
});
```

Both parties usually record the same payment — Sam enters it, and Alex enters
it again an hour later without checking. `settleKey` is derived from the same
four facts on both sides, so the second copy collapses into the first instead
of paying the debt twice. The accepted cost: two genuinely separate identical
payments between the same pair on the same day also collapse. Give the second
one a distinct key if that ever happens.

The starter page has no settle form — the reducer handles settlements, the UI
doesn't offer them yet. Adding one is a form and one `record` call, and it is a
good first change to make.

## 8. Read it from the terminal

```sh
svrn ring log house-expenses
```

```
house-expenses — 3 act(s) admitted from 3 line(s) held

  2026-08-31 18:04  alex         {"amount_cents":4720,"description":"propane","kind":"expense","participants":["alex","kit","robin","sam"],"payer":"alex"}
  2026-08-31 19:20  sam          [voided] {"amount_cents":6215,"description":"groceries","kind":"expense","participants":["alex","kit","robin","sam"],"payer":"sam"}
  2026-08-31 19:41  robin        corrects 4f2a91c0e8b3… → {"amount_cents":6125,"description":"groceries","kind":"expense","participants":["alex","kit","robin","sam"],"payer":"sam"}

  complete — every op this node holds is accounted for.
```

Robin corrected Sam's entry — anyone the roster claims may correct any act,
and the original stays in the history marked `[voided]` rather than
disappearing. The payload keys print sorted because that is how they were
signed. And note what this command does *not* print: a balance. The rail has no idea what a payload means,
and a terminal that guessed at one for whichever app happened to be in front of
it would be the money rules living in a second place. Balances are the app's;
open the page.

`--json` gives you the same thing machine-readably.

## Gaps — the part that makes this trustworthy

Two kinds, and the page shows both:

**The rail's gaps** (`log.gaps`) — an op that hasn't reached this node yet, a
signature that doesn't verify, a signer no roster claims, a line written by a
newer build. They mean the totals above cover a subset.

**Your app's gaps** (`book.gaps`) — an amount that isn't money, a name the
roster doesn't know, an expense split between nobody. They mean an act arrived
intact and could not be read as an expense.

A gap does not make the entries above it wrong. It makes them **partial**, and
that distinction is the whole point: a total shown without saying it may be
partial is worse than the spreadsheet it replaces. The scaffold renders the gap
panel for you and keeping it is not optional. Sequence holes usually close on
their own within a minute.

An empty roster flags every name, on purpose. Before anyone has been added,
"we know nobody" is the honest answer — and it is what tells a new housemate to
run `roster add` rather than leaving them with a page that looks fine and
converges to nothing but unknown-signer gaps on every other machine.

## Changing the money rules

`expenses.js` is yours. Two rules in it are worth reading before you touch
anything, and both have tests named after them:

**The remainder goes to whoever sorts first.** $10.00 three ways is
334/333/333 — never 333/333/333 with a penny evaporating, and never 3.333…
rounded per node. The rule is arbitrary; what matters is that it is computable
from the act alone, so every node lands on the same cents with no coordination.

**`participants` is an explicit list, and the roster is never consulted for
it.** The moment a split means "everyone in the ring", the same acts divide
differently on a node whose roster has one more person on it — and adding a
housemate silently re-divides the entire history.

Run the tests after any change:

```sh
cd house-expenses && node --test
```

`validate()` is deliberately called by both the page's submit handler and the
reducer, so the app never writes an act it would later report as a gap. Keep
that shape; two copies of "is this act writable" is two answers.

## Building something else on the same rail

The rail never reads inside a payload, so a tool-lending board is the same
substrate with different rules. Scaffold it, replace `expenses.js` with a
reducer over `{kind: "borrow", item, borrower}` and `{kind: "return", item}`,
replace the form, and keep `app.js` almost as-is. The signing, the ordering,
the deduplication, the corrections and the gap accounting are already done and
you do not re-derive any of them.

If your app needs a change to the rail itself, that is a bug report worth
filing — the rail is meant to be finished.

## What M0 does not do yet

**No publish verb.** `svrn ring deploy` does not exist. `window.ring` is
injected by `ring dev`'s own shim rather than shipped in the mesh-app SDK, so a
bundle put through `svrn meshapp publish` comes up without it. Each member runs
`svrn ring dev` against their own daemon, from their own copy of the folder.

**The roster travels by hand.** It is a per-node file, not gossiped state.

**The rail is loopback-only.** It is mounted on the operator and rail listeners
and on neither the peer nor the guest surface, so nothing off the machine
reaches it directly. Journal ops sync between nodes over the mesh's internal
path; the rail routes themselves stay local.

None of these are permanent, and none of them stop four housemates from
settling a real month.

## Where things live

| Thing | Path |
|---|---|
| The journal and the roster | `~/.svrnmesh/rings/<namespace>/` |
| The rail routes | `POST /v1/rail/append`, `GET /v1/rail/log` on `:9743` |
| The app | wherever you scaffolded it |

Reference: [MESHAPP_AUTHORING.md](../sovereign/docs/MESHAPP_AUTHORING.md) for
the authoring contract, [CLI_REFERENCE.md](../sovereign/docs/CLI_REFERENCE.md)
for every `svrn ring` flag, and
[INTEGRATION_SURFACES.md](./INTEGRATION_SURFACES.md) for what is a contract and
what is not.
