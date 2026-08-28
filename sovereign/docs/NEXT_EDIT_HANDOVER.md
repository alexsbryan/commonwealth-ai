# Next-edit: handover brief

Written 2026-08-28 for the team picking this up. It is a map and a
ranked backlog, not a spec — the spec is
[`NEXT_EDIT.md`](./NEXT_EDIT.md) (as-built) and the user's view is
[`../../docs/NEXT_EDIT_IN_YOUR_EDITOR.md`](../../docs/NEXT_EDIT_IN_YOUR_EDITOR.md).

## What you are inheriting

After you make the same edit twice, the editor offers the remaining
sites as a queue you walk with Tab. THREE engines behind one route:

| | rule lane | model lane | symbol lane |
|---|---|---|---|
| where | `next_edit.rs` | `next_edit_model.rs` | `next_edit_symbols.rs` |
| needs a model | no | yes, any competent chat model | no |
| needs an INDEX | no | no | **yes — see the traps** |
| latency | p95 24 ms | p95 ~1.8 s | p95 < 10 ms (one indexed SQL lookup) |
| can invent | structurally no — string search only | yes, so it is fenced hard | structurally no — it proposes NO text |
| gesture | Tab | Tab | status bar → QuickPick, Enter |

The symbol lane (2026-08-28) is the odd one and the difference is not
cosmetic: it proposes call SITES and never edit text. That is the
measurement, not a staging choice — on the shape it fires on, site
recall is 95.8% (CI [87.0, 100.0], clear of the 80% bar) while site
precision is 69.7% (CI [34.4, 91.5]) with the 60% bar INSIDE the
interval, i.e. a could-not-judge. A jump list's bar is recall.

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

1. **The syntax oracle for JavaScript and Python.** TypeScript LANDED
   2026-08-28 and this entry is narrowed, not closed. The TS blocker
   was re-tested under current code and reproduced to the decimal — it
   was never stale — and the unlock was not a better filter but
   `DECLINE_WHEN_EMPTIED`: emptying the site set hands the case to the
   pair fallback, whose rule can be wrong, so declining instead
   recovered useful-fire to 51.0% at hunk-precision 38.9% → 41.5%
   (88 junk hunks removed for 2 good, 44:1). Whether that guard helps
   is a property of the LANGUAGE — on Rust/Go it COSTS 3.0 pts, so
   they keep the fallthrough. `PROVEN_LANGUAGES` is now
   `["rust", "go", "typescript"]`. JS and Python parse fine and stay
   out for want of a measurement; adding one without it is the exact
   move the list exists to prevent. The known residue is
   same-kind-different-referent matches, which needs name binding a
   parse tree does not carry — and which the symbol lane now has.

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

4. **Edit CONTENT for the symbol lane** (cross-file SITES shipped
   2026-08-28). This entry used to say cross-file was deferred because
   "a SCIP index is stale exactly where it is needed" — **that was
   measured and refuted.** The staleness objection holds for a RENAME,
   where the symbol being created is by definition not indexed. It does
   not hold for a signature edit: the function existed before its
   parameter list was touched, so the last-save graph knows it and its
   callers, and the call sites are themselves unedited. Site recall on
   that shape is 95.8%.

   What is still deferred is the TEXT to write at each site, and the
   blocker is **bank construction, not a filter**. M1a measured ten
   candidate filters and none reached 60% precision while holding 80%
   recall; the precision CI is wide because the population is only 13
   independent commits. The named path is widening the harvest by
   mapping call-site lines through intervening diffs instead of
   requiring byte-identity with HEAD — which also recovers the 731
   sites currently dropped as unaligned. Do NOT reach for a filter
   first; M1a is the record of why.

5. **The JetBrains port.** Named deferred, and cheap by construction —
   all policy is daemon-side, so the port is a capture-and-render
   client. The fastest way to double the addressable editors.

6. **Marketplace publish.** Named deferred.

## Traps that will cost you a day

- **No indexed corpus means the SYMBOL lane is inert — but it now says
  so in three places.** `svrn setup --fim` does NOT build a code index
  (indexing takes minutes and setup is a command people expect to
  finish), so a fresh install has FIM and next-edit working and the
  jump list absent. That was silent until 2026-08-28; it is now
  announced, and the three surfaces are deliberate rather than
  redundant — a developer meets whichever one they reach first:

  1. **`svrn setup --fim`** closes with an OFFER — the command, and why
     the lane is worth having — suppressed once any corpus holds a
     populated graph.
  2. **`svrn doctor`** — the `scip_indexed` check names the jump list
     among its consumers and repairs with `svrn init`. It
     asks the graph for its symbol count, so an empty schema from a
     failed export reads as failed, not present.
  3. **The editor** — invoking the command with no sites reports the
     real reason instead of "edit a parameter list", and offers to open
     a terminal with `svrn init` typed but NOT run.

  The daemon also logs `WARN` once per process, and only for
  `graph_unavailable`: `symbol_not_indexed` fires for every new
  function and `no_path` for every scratch file, so warning on those
  would fire on ordinary typing and train the reader to ignore the log.
  Corpus id defaults to the workspace folder's name; override with
  `sovereign-fim.nextEdit.corpusId`. **Still the first thing to check
  when someone reports "I never see call sites."**
- **The symbol lane is bounded at 250 ms and degrades to `timed_out`.**
  The measured lookup is 0.03 ms (9.4 ms worst-cased), but the
  reindexer can hold SQLite's lock and this runs on the typing path. A
  jump list is never worth a stalled keystroke.
- **The symbol lane is Rust-only** (`TRIGGER_LANGUAGES`), because Rust
  is the only language the SCIP exporter indexes on this host. It is
  not a policy whitelist like `PROVEN_LANGUAGES` — adding an id without
  an index behind it builds a trigger that can only ever find zero
  sites.
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
