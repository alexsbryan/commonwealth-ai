# Next-edit: the symbol lane — spec

**Status: SHIPPED as NAVIGATION, 2026-08-28.** M0 and M1a are
measurements; the lane that landed proposes call SITES and no edit
text, because recall is the verdict that passed and precision is a
could-not-judge. See "M3 — what shipped" at the end, `NEXT_EDIT.md`
§9e, and the `DEFAULTS_LEDGER.md` row.
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

## M1 — the site filter (the blocking question after M0)

M0 renumbered this plan. Deriving edit content was M1; it is now M2,
because there is no point deriving content for sites that should not be
offered. **M1 is a filter on WHICH callers**, which is the branch M0
pre-registered for exactly this outcome.

The population is fixed by M0 and is not re-derived: on the cross-file
slice the graph named **1,156** sites, the author edited **694**, the
overlap is **593**. So **563 sites are over-offered** and 101 are
missed. M1a diagnoses those 563; M1b builds a filter only if M1a names
one worth building.

### M1a — pre-registered measure (written before the classification ran)

For every candidate filter `F` — a predicate over a predicted site or
over its episode — report four numbers on the cross-file population,
against the same ruler M0 used:

| quantity | definition |
|---|---|
| junk removed | over-offered sites `F` drops (of 563) |
| good lost | author-edited sites `F` drops (of 593) |
| precision after | kept-TP / kept-total |
| recall after | kept-TP / **694** — the denominator does not move |

`F` is a **candidate for M1b** iff it lifts cross-file precision to
**≥ 60%** (M0's bar) while holding recall **≥ 80%** (M0's bar). A filter
that clears precision by destroying recall is reported and not
recommended. If no single-axis filter clears both, the bounds and the
best joint filter are reported and nothing is ranked.

### The four hypotheses, and what each implies

Named before measurement so that a null result is legible:

1. **Arity.** A signature that gained a parameter leaves old-arity calls
   needing work and new-arity calls not. If this dominates, the filter is
   nearly free and nearly exact.
2. **Test / bench / example paths and `#[cfg(test)]` blocks** — plausibly
   updated in a separate commit, or not at all.
3. **Crate distance** from the edited declaration.
4. **Additive-compatible changes**, where only some callers ever needed
   updating. If this dominates, **51.3% is not an over-offer** — it is
   the truth set being narrower than reality, and the right response is
   to fix the ruler, not to filter.
5. **The site is not a call at all.** Added before the classification ran
   and after reading the table's schema, not after seeing an outcome:
   `refs` is an OCCURRENCE table, not a call table — `ref_kind` is
   uniformly `'direct'` across all 1,356,363 rows, and sampled rows are
   `use` statements. A function mentioned in an import, passed as a
   value, or named in a trait bound is a site the graph offers and a
   signature change never obliges the author to touch. M0's author-truth
   requires `NAME\s*\(` on the line, so every true positive is
   call-shaped by construction while a predicted site need not be. The
   `start_col`/`end_col` columns test this directly and cheaply: read the
   character that follows the occurrence.

### The instrument check that runs first (ARCH §18.4)

Every file in this bank is byte-identical to HEAD, and HEAD compiles.
So every predicted call site is already consistent with the **new**
signature. Two consequences, and the first is a test of the instrument
rather than of the graph:

- If a large share of over-offered sites parse as **old** arity, the
  arity extractor is wrong and no arity number may be published.
- If they parse as **new** arity, they genuinely did not need editing,
  which supports hypothesis 4 over hypothesis 1 and makes the
  **episode-level** question — *did this signature change break callers
  at all?* — the one worth filtering on.

# M1a RESULT (2026-08-28) — the population was not the shape, and M0's cross-file verdicts do not survive their own ruler

Run: `python3 gym/next-edit/aligned/classify_overoffer.py [--all-sites]`
from the repo root. Transcripts and the 1,156-row per-site table are
committed beside it (`m1a_crossfile.txt`, `m1a_allsites.txt`,
`m1a_sites.tsv`). The classifier **imports** `derive_episodes` from the
M0 probe rather than re-deriving the population, so the two cannot
disagree about what was measured; the probe was refactored to expose it
and reproduces both M0 headlines to the decimal.

## 1. The 563 over-offers are mostly not signature fanout

| episode kind | episodes | sites |
|---|---|---|
| function did not exist at `commit^` | 109 | 592 |
| declaration text **unchanged** (the file moved) | 101 | 439 |
| return type / generics only | 11 | 77 |
| **parameter list changed** | **10** | **41** |
| parameter renamed only | 1 | 7 |

**1,031 of 1,156 cross-file sites (89.2%) come from episodes where no
pre-existing function's signature changed.** M0's trigger is "the commit
touched a line inside the declaration span", which a brand-new function
and a bulk file move both satisfy for every function they contain. The
23 commits in the population include squash-merged PRs — `Noun
convergence (#47)` alone supplies 45% of the sites — and a squash merge
moves files wholesale.

Hypothesis 4 is therefore answered, but not as posed: the over-offer is
not additive-compatible change with an incomplete truth set. It is a
trigger that fires on a different event.

## 2. The verdicts do not survive a cluster-aware ruler

`last_touching()` keeps, per file, the commit that touched it LAST, and
alignment then requires that file to be untouched since — so the
population is whatever recent commits touched the most files, and
episodes within a commit share an author, an intent, and a bulk-move
status. Resampling **commits** rather than sites:

| slice | precision | recall |
|---|---|---|
| all sites | 86.6% **[79.4, 91.1]** | 94.4% **[85.9, 97.7]** |
| cross-file | 51.3% **[27.2, 65.8]** | 85.4% **[53.6, 94.6]** |

The all-sites headline holds: both intervals clear their bars. **The
cross-file headline does not.** The 60% precision bar and the 80% recall
bar both lie *inside* their intervals, so M0's cross-file "precision
FAIL / recall PASS" is a **could-not-judge** on both counts (ARCH §18.1),
not the split verdict recorded. That correction is owed to the M0
commit message and note, which quote the point estimates as verdicts.

## 3. On the shape the hypothesis actually names

Restricting to episodes where an existing function's **parameter list**
changed — the only shape that obliges a call site to change:

| slice | episodes | commits | precision | recall |
|---|---|---|---|---|
| all sites | 34 (bar ≥ 25) | 13 | 69.7% [34.4, 91.5] — **could-not-judge** | 95.8% [87.0, 100.0] — **PASS** |
| cross-file | **10** | 4 | yield bar not met — no rate published | — |

**The core hypothesis holds on recall and only on recall.** A graph built
from the last save names the sites the author then edits: 95.8%, interval
entirely above the bar. Precision cannot be judged at 13 clusters.

M0 met its ≥ 25-episode yield bar by counting episodes of every kind; on
the shape the hypothesis names, the cross-file yield is 10.

## 4. Candidate filters — none clears the pre-registered ruler

Best of ten, on the cross-file population:

| filter | junk removed | good lost | precision | recall |
|---|---|---|---|---|
| drop non-call occurrences | 105 | **0** | 56.4% | 85.4% |
| drop `cfg`-gated sites | 282 | 164 | 60.4% | 61.8% |
| call **and** a real signature change | 526 | 515 | 67.8% | 11.2% |

Hypothesis 5 is confirmed and is free: all 105 non-call sites are `use`
imports and `pub use` re-export lists, and **none** is a true positive.
It is also not enough. Everything that reaches 60% precision does so by
discarding most of the recall — the case the pre-registration said to
report and not recommend. **No filter is recommended, and M1b as
chartered should not be built:** a filter cannot fix a population whose
96.5% majority is a different event.

## 5. Two instrument bugs found in M1a itself

Recorded because the first M1a run was, like M0's, confident and
well-formed and wrong:

- `git show commit^:decl_path` is empty for a file the commit **moved**,
  which reads as "new function". On a 40-episode sample, **20 of 40** so
  classified existed in the parent tree under another path. Resolution is
  now by symbol across the tree, not by path.
- No clustering was accounted for anywhere. It is the difference between
  a 51.3% FAIL and a could-not-judge.

## 6. What replaces M1b

Not a filter. The blocking question is **bank construction**: 13
independent commits cannot settle precision, and the byte-identical
alignment constraint caps the reachable pool — only 78 commits can
contribute at all, and 731 predicted sites are already dropped as
unaligned. Priced options, in order:

1. **Widen the harvest** by mapping call-site lines through the
   intervening diffs instead of requiring byte-identity. Recovers the 731
   dropped sites and admits commits that are not a file's last touch.
   Offline, no new index.
2. **Ship on recall.** Recall is the verdict that passes, on every slice
   and on the target shape. A navigation affordance — "N call sites
   reference this" with nothing proposed and nothing to accept wrongly —
   has a recall bar, not a precision bar. This is `NEXT_EDIT.md`'s own
   fallback and it needs neither the filter nor the content derivation.

## M3 — WHAT SHIPPED (2026-08-28)

The lane, as navigation. `next_edit_symbols.rs` beside the syntax
oracle; `next_edit.rs` untouched and still pure. M2 (edit content) is
NOT built and its bar is unchanged.

**The crate-graph decision M3 flagged, resolved.** `commonwealth-api`
takes a DIRECT dep on `corpus-engine-scip`. That is the carve-out's own
rule, not an exception to it: `corpus-engine/src/lib.rs` states
"external consumers import directly from `corpus_engine_scip::*`. No
re-export shim from this crate", precisely so two crates cannot land on
different versions of one logical type (ARCH §8.3). It is not a
`sovereign-*` edge, so the layer gate's `commonwealth-api` forbid rule
does not reach it, and no `ARCH_LAYERS.toml` entry was needed.

**One new method on the graph's owner**, rather than SQLite opened by
hand in a second place: `ScipGraph::find_callers_qualified`. The
existing `find_callers` resolves a plain NAME and matches
`refs.callee_symbol`, which is also a short name — `new` maps to 631
distinct symbols on this graph, so it answers "every `new` in the
workspace". The new method keys on the SCIP descriptor. It names both
columns because `callee_qualified` has no index: measured on the live
graph, identical rows at **0.03 ms** against **105 ms** for the bare
qualified scan, worst case **9.4 ms** (`poll`, 21,420 refs). The 6.8%
of rows with an empty `callee_symbol` are all module references, never
functions, so the pairing cannot hide a call site.

**The trigger was made falsifiable.** "Cursor in a parameter list and
the user just edited" is a gate that cannot fail — next-edit fires on
edit-settle with the cursor AT the edit (ARCH §18.1). Shipped instead:
the buffer's parameter list must DIFFER from the last save,
whitespace-normalised so a rustfmt rewrap is not a contract change.
This is also M1a's shape implemented rather than approximated, and it
excludes the two classes that dominated M0's population —
`symbol_not_indexed` (a function being typed for the first time) and
`signature_unchanged` (a file that merely moved).

**M1a's free filter ships unconditional.** Dropping occurrences with no
call paren after them removed 105 junk sites and zero true ones.

**Verification.** `commonwealth-api/tests/next_edit_symbol_lane_e2e.rs`
— six tests against a REAL `ScipGraph` and real files on disk, plus 12
unit tests in the module. Both measured guards were watched to FAIL:
neutering `is_call_site` reds two e2e tests, neutering the signature
comparison reds one. Gates on the landing commit: lint `--full` 0
errors, 11,037 tests 0 fail, extension 50/50 with tsc clean.

**What is NOT covered**, unchanged from M1a: Rust only, because it is
the only language the graph indexes; and the precision number that
would justify proposing edit text still does not exist — the path to it
is a wider harvest, not a better filter.

## M2 — the edit content (only if M1 clears)

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

## M3 — the lane, default OFF (only if M2 clears)

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

---

# M0 RESULT (2026-08-28) — the hypothesis holds; precision splits

Run: `gym/next-edit/aligned/probe_symbol_lane.py`, rust, index-aligned,
925 episodes (bar was ≥25).

| | all call sites | cross-file only |
|---|---|---|
| site recall | **94.4%** (2597/2752) PASS | **85.4%** (593/694) PASS |
| site precision | **86.6%** (2597/2999) PASS | **51.3%** (593/1156) FAIL |

**The core hypothesis is confirmed.** A SCIP graph built from the last
SAVE names the call sites the author then edited, at 94.4% recall. The
staleness argument that keeps `next_edit_syntax.rs` off the graph does
not apply to this shape, as predicted — the function existed before its
signature was touched.

**Precision is the open problem and it is concentrated across files.**
Same-file the graph is nearly exact; cross-file it names 1,156 sites
where the author edited 594. More than half of cross-file callers did
not need the change. The spec pre-registered this branch: *"if recall
clears and precision does not, the finding is 'the graph names the
sites but over-offers' and the next question is a filter on WHICH
callers, not more induction."* That is now the M1 question.

**Read the 51.3% against the incumbent before judging it.** The bar was
60%, reasoned from "a semantic lane that cannot beat the text engine
has no argument" — and the shipping rule lane is at **38.6%**
hunk-precision. Cross-file at 51.3% misses the bar and still beats what
users have today. Whether that ships is a judgement, not a measurement,
and it is the operator's.

## What this measurement does NOT cover, stated plainly

- **Rust only.** It is the sole language with a SCIP export here
  (`scip_meta.languages_with_scip`).
- **Unambiguous names only.** 1,755 of 4,377 episodes (40%) were
  EXCLUDED because the repo defines the same name on more than one
  symbol, and a textual ground truth cannot tell `Foo::new(` from
  `Bar::new(`. That exclusion is not random: those are exactly the
  cases where a graph's disambiguation is worth most, so the measured
  numbers are if anything conservative about the graph's edge — but
  they are unmeasured, not assumed.
- **Sites, not content.** Whether the right text can be derived for
  each site is M1 and is untouched here.

## Two instrument bugs found and fixed before publishing

Recorded because the first run produced a confident, well-formed, wrong
answer (12.2% recall) and only looked wrong on inspection:

1. **Signature detection by span proximity alone** counted body edits
   near the top of any short function — 15,096 "signature edits" across
   2,074 files. Fixed by requiring the line to match `fn <name>`.
2. **`"name(" in line` as ground truth** matched `Bar::new(` for symbol
   `Foo::new`, inflating the author set with lines that were not call
   sites of that symbol and depressing recall for a reason unrelated to
   the graph. Fixed with a word-boundary match plus the ambiguous-name
   exclusion above.

Also: `refs` has no index on `callee_qualified` (only on the empty
`callee_symbol` column), so a per-symbol lookup is a full scan of 1.36M
rows. The probe does ONE scan for all callees. Anything built on this
table needs to know that.
