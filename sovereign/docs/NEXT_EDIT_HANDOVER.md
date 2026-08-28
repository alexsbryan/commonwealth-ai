# Next-edit: handover brief

Written 2026-08-28 for the team picking this up. It is a map and a
ranked backlog, not a spec — the spec is
[`NEXT_EDIT.md`](./NEXT_EDIT.md) (as-built) and the user's view is
[`../../docs/NEXT_EDIT_IN_YOUR_EDITOR.md`](../../docs/NEXT_EDIT_IN_YOUR_EDITOR.md).

## What you are inheriting

After you make the same edit twice, the editor offers the remaining
sites as a queue you walk with Tab. It is two engines behind one route
and one key:

| | rule lane | model lane |
|---|---|---|
| where | `commonwealth-api/src/next_edit.rs` | `next_edit_model.rs` |
| needs a model | no | yes, any competent chat model |
| latency | p95 24 ms | p95 ~1.8 s |
| can invent | structurally no — string search only | yes, so it is fenced hard |

Both answer `POST /v1/edit_predictions`
(`routes_edit_predictions.rs`). All policy is daemon-side by design,
so an editor client is a thin capture-and-render shell — that is what
makes the JetBrains port cheap, and it is worth not eroding.

The expensive failure is a *wrong* edit, because the user might accept
it. Every gate in here prefers silence, and several deliberate trades
below only make sense read in that light.

## Run it in five minutes

```sh
svrn setup --fim                       # writes [models.edit]; leaves your chat primary alone
python3 scripts/next_edit_eval.py      # rule bank, 120 cases, NO model needed
python3 scripts/next_edit_gen_eval.py  # model bank, 60 cases, needs the edit slot
```

Both exit non-zero on a gate miss. The rule bank runs against any
daemon build carrying the route, whatever `[models]` says.

## What is measured, and when

| bank | n | last verdict |
|---|---|---|
| rule (`gym/next-edit/`) | 120 | **2026-08-28, five gates green** — 95 scored, 25 declined and re-verified, p95 24 ms |
| model (`gym/next-edit/gen/`) | 60 | 2026-07-30, five gates green — 30/30 correct, 0 wrong, p95 1.8 s |
| golden (`gym/next-edit/golden/`) | 1,098 + 383 | the sweep surface for precision work; not a gate |

**The model bank was not re-run for this handover.** The box this was
prepared on has no `[models.edit]`, so that row is the last recorded
verdict, not a fresh one — re-run it before you trust it.

### The declined population — read this before you touch the rule bank

25 of the rule bank's 120 cases are declined by a deliberate precision
trade rather than scored, and `gym/next-edit/README.md` carries the
full account. The short version: the syntax oracle (2026-08-06) and
the `MIN_RULE_CHARS` raise (2026-08-07) both landed *after* those
fixtures were cut. Scored against the original bar they could only
ever fail, so G2 sat permanently red and a reader could not separate
an inherited red from a regression they had just caused.

Each one now names its mechanism in `expect.declined_by`, and **that
annotation is a check, not a waiver** — G5 re-derives it every run and
goes red both when a mechanism stops holding and when a case starts
passing outright. If you change site selection or the threshold, the
bank will tell you exactly which of the 25 moved and in which
direction. That is the point of them.

## Open work, ranked

1. **The syntax oracle for TypeScript, JavaScript and Python.**
   `PROVEN_LANGUAGES` in `next_edit_syntax.rs` is Rust and Go only,
   because on TypeScript the filter measured *worse* — useful-fire
   52.0% → 41.2%, wrong-fire 6.2% → 9.7%. This is the widest user
   surface still blocked, and `cases.react-ts.jsonl.gz` (383 cases) is
   already cut for it. The known residue is same-kind-different-
   referent matches, which needs name binding a parse tree does not
   carry. Adding a language to that list without a measurement is the
   exact move the list exists to prevent.

2. **The casing-variant rule sub-lane.** Renaming `getUserData` while
   `get_user_data` survives elsewhere is detected today and declined
   by name (`casing_deferred`). The consult gate already computes the
   variant find/replace, so this is a deterministic, byte-precise
   sub-lane rather than a model problem — the model measured actively
   destructive here (4 of 9 casing fires wrong, including applying a
   rename backwards twice). The bank's `deferred_casing` cases
   re-activate when it lands. Small, well-specified, and it closes a
   category users hit constantly.

3. **The 25 declined cases as a recall backlog.** Each is a named,
   reproducible instance of a precision trade costing a real edit. The
   14 `syntax_oracle` ones are the `KIND_DEPTH` and name-binding
   question above; the 8 `min_rule_chars` ones are the threshold
   frontier (the sweep table in `should_fire` is where that argument
   lives); the 3 `pair_fallback` ones are arguably already correct —
   the routed edit is text-equivalent to what the fixture wanted, at a
   wider anchor — and could be re-cut once someone decides the
   anchored form is the intended answer.

4. **Cross-file sites.** Named deferred. Note the oracle's own
   reasoning applies: next-edit fires on an unsaved, often unindexed
   buffer, so a SCIP index is stale exactly where it is needed.

5. **The JetBrains port.** Named deferred, and cheap by construction —
   all policy is daemon-side, so the port is a capture-and-render
   client. The fastest way to double the addressable editors.

6. **Marketplace publish.** Named deferred.

## Traps that will cost you a day

- **No `[models.edit]` means the model lane is silently inert** —
  `dropped: "unavailable"`, not an error. The rule lane keeps working,
  which is exactly what makes it easy to miss.
- **`next_edit_format` cannot be probed** — it is a fact about the
  fine-tune, and an unset value defaults to `region_instruct`. A
  specialist served the wrong dialect does not fail; it returns
  confident, well-formed, wrong edits. Instinct scored 0/30 that way.
- **`commonwealth-api` compiles into the daemon binary.** Rebuilding
  and not restarting means you are testing the old code. Confirm
  through `/status`, not a file mtime.
- **Re-mining the bank** (`gym/next-edit/harvest.py`) carries the 25
  annotations forward by case id and reports orphans on stderr. Read
  that line — an orphan means a case stopped being mined, and its
  annotation went with it.
- **Thinking must be suppressed on a chat model, and it takes both
  transports** — `chat_template_kwargs.enable_thinking=false` *and*
  `think_budget=0`. With reasoning on, a 35B chat primary scored 0/30:
  reasoning ate the entire generation budget before the first answer
  byte.
