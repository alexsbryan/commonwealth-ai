# Next-edit: the symbol lane — spec

**Status: SPEC, nothing built. M0 is a measurement, not a feature.**
Written 2026-08-28. Sibling of [`NEXT_EDIT.md`](../NEXT_EDIT.md), which
is the as-built design; this proposes a THIRD induction source beside
the rule lane and the model lane.

## The gap this addresses

Measured on the golden set 2026-08-28 (`gym/next-edit/golden/`, 711
positives, rule lane isolated, production route):

| shape | n | useful-fire |
|---|---|---|
| `signature_fanout` | 90 | **3.3%** |
| `delete_propagation` | 90 | **4.4%** |
| `import_addition` | 90 | **0.0%** |

270 cases — 38% of all positives — at 2.6% combined, against 35.0%
overall. These are not tuning misses. **The rule lane cannot reach them
by construction**: its trigger is textual repetition and it requires
support ≥ 2 of the *same* induced rule. In a signature fanout the
trigger (you edit the declaration) and the consequence (call sites gain
an argument) are different text, so support never reaches 2 and the
lane is correctly silent.

A symbol graph is a different induction source: **semantic consequence,
actionable on the FIRST edit.**

## The hypothesis, stated so it can fail

> When a developer edits a function's signature, the SCIP graph built
> from the last SAVE names the call sites the author is about to edit,
> with recall high enough to be worth offering and precision high enough
> not to bury them.

**Why the staleness objection does not apply here**, which is the
non-obvious part. `next_edit_syntax.rs` declines the SCIP graph because
next-edit fires on an unsaved buffer, so an index of the last save is
stale exactly where it is needed. That argument holds for renames. It
does **not** hold here: the function *existed before* the developer
touched its signature, so the saved index knows the symbol and knows
its callers, and the call sites themselves are unedited. This is the
one shape where a last-save index is precisely right.

**What would kill it**, named now: call-site precision. If `callers()`
routinely names sites the author did not touch (an overload, a
same-named method on another type, a call already passing the new
argument), the lane over-offers and a wrong edit is the expensive
failure (`NEXT_EDIT.md` §1).

## M0 — the minimal realization: a probe, no production code

**M0 writes no daemon code and changes no behaviour.** It answers only
the question above, offline, and it is allowed to kill the idea.

Bank: `gym/next-edit/aligned/` — the index-aligned bank
(`README.md` there). It exists for exactly this: every case's file is
byte-identical to HEAD, so the SCIP graph validly describes it. This is
the index-dependent experiment that bank was kept for, and neither of
the other two banks can host it (the 120-case bank replays historical
states; the golden set is 41 unindexed repos).

The two graph queries are already verified against
`~/.svrnmesh/indexes/commonwealth-ai/scip_graph.db` (2026-08-28):

```sql
-- 1. the edited line -> its enclosing function
SELECT name, qualified_name FROM symbols
 WHERE file_path=? AND line_start<=? AND line_end>=?
 ORDER BY (line_end-line_start) ASC LIMIT 1;

-- 2. that function -> every call site, compiler-resolved
SELECT file_path, line, start_col, end_col FROM refs
 WHERE callee_qualified=?;
```

Verified on `next_edit.rs:489` → `predict_filtered` (lines 484-567) →
2 call sites, extracted tokens matching the symbol name.

M0 mines signature-change episodes from the aligned pool (a commit hunk
landing inside a function's declaration line range) and reports:

| metric | question |
|---|---|
| **site recall** | of the call sites the author edited, how many did `callers()` name? |
| **site precision** | of the sites `callers()` named, how many did the author edit? |
| **trigger yield** | how many aligned episodes are signature changes at all? |

### Pre-registered bars for M0

Set before the probe runs. A miss is a finding, not a bar to move.

| bar | value | why that number |
|---|---|---|
| site recall | **≥ 80%** | below this the lane misses the sites the user came for, and a partial fanout is worse than silence — they will not trust the rest |
| site precision | **≥ 60%** | the rule lane ships at 38.6% hunk-precision; a semantic lane that cannot beat the text engine has no argument |
| trigger yield | **≥ 25 episodes** | fewer and neither rate above has a CI worth reading; report the CI and say so rather than ranking a number |

If recall clears and precision does not, the finding is "the graph
names the sites but over-offers" and the next question is a filter on
*which* callers, not more induction.

## M1 — the edit content (only if M0 clears)

The sites are half the problem; what to write at each is the other. The
proposal: derive it from the developer's own declaration diff (the
`HistoryUnit` already carries `before`/`after`), not from a model —
`, host.as_ref()` added to a parameter list implies `, None` or the
same expression at each call site, and getting that wrong is a wrong
edit rather than a missed one.

Bar: **exact-match on the author's call-site text ≥ 70%**, measured on
the M0 episodes. Below that the lane emits sites without content and
the honest product is a *navigation* affordance ("3 call sites need
updating") rather than an edit proposal — which is still useful and
much safer.

## M2 — the lane, default OFF (only if M1 clears)

Shape follows `next_edit_syntax.rs` exactly, and that precedent is the
reason this is cheap:

- `next_edit.rs` STAYS PURE. It has no editor knowledge and no index by
  contract, and a symbol graph is both. It already takes a `SiteOracle`
  closure; this adds a second seam of the same kind for *candidates*
  rather than for filtering.
- A new `next_edit_symbols.rs` beside the syntax oracle owns the two
  queries and the trigger detection. The route composes it, exactly as
  it composes `SyntaxOracle::parse`.
- **Crate-graph decision, flagged not assumed** (ARCH §8):
  `commonwealth-api` currently depends on `corpus-engine` for GRAMMARS
  only. Reading `scip_graph.db` is a new capability and needs an
  explicit owner — reuse the existing store rather than opening the
  SQLite file by hand in a second place.
- Default OFF behind an env flag, declared in `quality/env-flags.toml`,
  with a `DEFAULTS_LEDGER.md` row naming the measurement owed. It flips
  on only when the aligned bank says so.

## Explicitly out of scope

- `import_addition`. The shape label names the episode's TRIGGER, not
  the held-out edit — sampled truths include a plain identifier rename.
  A mapping from "import addition" to `symbols(name)` was proposed on
  2026-08-28 and **withdrawn on inspection**; recorded so it is not
  re-proposed from the shape name alone.
- `delete_propagation` until `signature_fanout` clears. Same query
  shape, 84% in indexable languages, but one lane at a time.
- Anything for a repo with no index. The lane must be absent, not
  degraded — an unindexed workspace gets today's behaviour exactly.

## Sizing, as a bound rather than a promise

45 of 90 `signature_fanout` cases have a held-out truth that is
literally a call site gaining an argument (measured 2026-08-28), and
89% are in indexable languages, against 3 cases served today. If the
lane recovered those, that is **+42 cases on 711 positives ≈ +6 points
of useful-fire** — against the +2.6 points of hunk-precision the
TypeScript work bought. It is a bound on one shape, not a forecast, and
M0 exists to find out whether the first half of it is real.
