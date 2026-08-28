# Defaults Ledger — capabilities shipped dark, and what flips them

**The failure mode this file exists to stop:** an initiative proves a
capability works ("zero delta on both banks!"), ships it behind a flag
or non-default mode "until X", and then X never happens — the work
withers, provably good code sits dark forever, and six months later
nobody remembers the flip condition or whether it was ever met.
(Poster child: the cluster-score blend shipped 2026-05-22 at
`cluster_weight=0.0` "pending bench plan" and sat dark for ten weeks
before this ledger existed.)

**The contract:**

1. Any push that ships a capability default-off or dark **adds a row
   in the same commit** — with a falsifiable flip condition, which
   plan item or run settles it, and a review-by date.
2. When the condition is met (or refuted), the row **moves** to
   Graduated or Rejected — it never silently disappears.
3. A row past its review-by date is not noise: it is the signal. Any
   session touching that area raises it to the operator — flip it,
   kill it, or re-date it with a reason. "Still waiting" without a
   named blocker is not a valid state.

Cross-references: env flag defaults live in `quality/env-flags.toml`
(this ledger records *why* a default is what it is and what changes
it, not the mechanics); decisions with full context live in the notes
store (ids cited per row).

---

## DARK — proven or plausible, awaiting a named condition

### `Mesh::gossip_authorized` legacy compat arm — shipped ON (2026-08-26)

**What is dark.** Not a capability held back: a *fallback* held open. The
credential split gives `Mesh` a `mesh_secret` that authorizes gossip and never
rotates, separate from the `invite_key_hash` that admits joiners. A peer on a
pre-split build sends a zeroed `mesh_secret`, so `gossip_authorized` falls back
to comparing `invite_key_hash` — the old predicate — and logs
`gossip: legacy auth` at `warn` naming the mesh.

**Why it is not just removed.** Every node upgrades on its own schedule. Without
the arm, the first upgraded node rejects every peer that has not upgraded yet
and is rejected by them, which is precisely the symmetric partition the split
exists to eliminate. The arm is the only thing that makes the upgrade rolling
rather than flag-day.

**What it costs while open.** Invite rotation is still unsafe for un-upgraded
peers, because they authorize on the very hash rotation changes.
`EmbeddedDaemon::rotate_invite` therefore refuses with `PreSplitPeersOnline`
(HTTP 409) unless every online member is CONFIRMED post-split, naming the ones
that are not; `--force` overrides. So the cost is visible and gated, not silent.

The confirmation is our own observation, never a peer's claim: a merged gossip
payload carrying a `mesh_secret` proves the sender is post-split, and
`MergeReport::peer_pre_split` is the only place that fact surfaces before it is
discarded. `AppState::peer_post_split` retains it. Unknown counts as unsafe, so
a fresh daemon refuses until it has gossiped once with each online peer.

This guard shipped INERT on 2026-08-26 and was fixed 2026-08-27: it filtered on
`mesh.mesh_secret` — our OWN credential, non-zero on every migrated node — so
the predicate was always false and the refusal never fired. No test covered it
(`tests/rotate_pre_split_guard.rs` now does, five cases). Worth recording because
the failure was invisible in exactly the way this ledger exists to catch: a
guard that reads as protection, passes every gate, and protects nothing.

**Flip condition (falsifiable).** No `gossip: legacy auth` warn observed on any
fleet node for one full week. At that point every peer carries a real secret and
the arm is unreachable.

That condition was NOT falsifiable as first shipped, and it is worth recording
why. `DAEMON_TRACING_FILTER` listed `commonwealth_discovery` and
`commonwealth_api` but not `commonwealth_core`, where the warn is emitted, and
the filter's default directive is ERROR — so the warn was dropped before it
reached the log. The flip condition would have been satisfied by construction,
on a fleet still actively running the arm. Measured live 2026-08-27: a peer
sending a zeroed `mesh_secret` produced 5 gossip rounds and zero log lines.
Fixed by adding `commonwealth_core=info` to the filter and pinning it in
`tests::daemon_filter_lists_grounding_targets`. Verified after the fix: one
`gossip: legacy auth` warn per round, `self_has_secret=true
peer_has_secret=false`.

Before flipping, confirm the reader is actually wired: the absence of this warn
means nothing unless `commonwealth_core` is in the daemon's filter.

**Two things narrow the arm while it stays open (2026-08-27).** The fallback is
peer-SELECTED — `gossip_authorized` takes it whenever EITHER side's secret is
zeroed, so a caller omits the field and gets the weaker predicate, and its only
entry ticket is an `invite_key_hash` that rides every gossip payload and every
join snapshot. That is exactly what a departed member holds, and the mesh has no
eviction (todo `23f0b547`).

1. The gossip reply now REDACTS `mesh_secret` whenever the merge authorized on
   the legacy arm. Without it, a holder of any current-or-stale invite hash
   could read the real secret out of the response and hold permanent gossip
   auth — `mesh_secret` never rotates and `rotate_invite_key` structurally
   cannot change it, so there is no revocation path afterwards. That would be
   strictly worse than the pre-split model, where rotating DID revoke. Costs a
   genuine pre-split peer nothing: its build has no such field.
2. `SOVEREIGN_MESH_STRICT_AUTH=1` refuses the arm outright, for operators who
   know their fleet is upgraded and want the surface closed before the
   fleet-wide flip. Off by default. It only acts on a node whose own secret is
   set, so a not-yet-migrated node cannot self-partition with it.

**What settles it.** Delete the fallback branch in
`commonwealth-core/src/mesh.rs::gossip_authorized`, the `UNSET` zero-checks
around it, and the `PreSplitPeersOnline` refusal in
`EmbeddedDaemon::rotate_invite` — rotation becomes unconditionally safe once
there is no peer that reads `invite_key_hash` for auth.

**Review by:** 2026-10-01.

### `[models].fast_context_size` — per-slot KV windows, shipped UNSET (2026-08-25)

**What is dark.** The mechanism, not the value. Until now `[models].context_size`
was one global applied to every `LlamaContext` this daemon builds — the fast
slot, the primary, the primary sibling pool and the fast/primary alias — with a
doc comment that said so outright ("Applies to all loaded slots … per-slot
override would need a richer schema"). KV cache is linear in `n_ctx`, so a 4B
fast model carried a 27B primary's window and paid a 27B primary's cache for it.
`SlotWindows` makes the window a per-slot value; `fast_context_size` is the key
that uses it. It ships unset, which resolves to the primary's window and is
byte-identical to the old behaviour (pinned by
`an_unset_fast_window_is_the_primary_window`).

**Why it is not just set.** The fast slot is the OVERFLOW path — `pick_slot`
routes any prompt too large for FastShort's per-sequence budget here — so its
window must cover the largest prompt that actually lands on it, not the typical
one. Nobody has measured that. Shipping a guessed value would trade a known
memory cost for an unknown `NoKvCacheSlot` failure at decode time, which is the
worse trade.

**FLIP CONDITION (falsifiable).** With the `kv budget: slot context built`
trace lines now emitted per context at load, and a distribution of prompt sizes
that reach the `fast` slot (not FastShort) over a real soak:
set `fast_context_size` to the observed p100 fast-slot prompt + the output cap,
rounded up to a multiple of 256 (llama.cpp pads there anyway — see
`ctx_n_batch`). The flip is settled when a soak at that value shows **zero**
`NoKvCacheSlot` errors on the fast slot and the summed `kv_ceiling_mib` across contexts
drops. Both halves are required: a smaller number that starts failing decodes is
a regression, not a win.

**Settled by.** The same daemon restart that takes the per-slot KV
measurement — the trace lines exist precisely so this does not need its own run.

**Review by 2026-09-25.** If unmeasured by then the honest options are to take
the measurement or to delete the key; a per-slot knob nobody sizes is the
withering this ledger exists to stop.



### `SOVEREIGN_TENSOR_BUFT_OVERRIDE` — declaring where one tensor lives, shipped UNSET (2026-08-27)

**What is dark.** The ability to say, in configuration, which buffer type a
tensor must land in — llama.cpp's own `<regex>=<buffer-type>` syntax, with `CPU`
resolving to `ggml_backend_cpu_buffer_type()`: plain *pageable* CPU, deliberately
not the device HOST buffer, which is pinned and is the opposite of what a large
evictable table wants.

**Why it ships unset: it is currently a NO-OP, and that is the whole problem.**
Qwen3.8-Flash-Next's 26.8 GiB `per_layer_token_embd` engram already lands on
plain CPU without it — but only because IQ4_NL is unsupported by `Vulkan_Host`
and *falls back*. The load says so in as many words: `tensor
'per_layer_token_embd.weight' (iq4_nl) … cannot be used with preferred buffer
type Vulkan_Host, using CPU instead`. Measured flat either way (22.42 vs 22.45
tok/s; `research/engram/RESULTS.md` §6, and §6.1 measures the override itself as
a no-op on this model). Shipping it default-on today would be complexity that
moves no metric.

**Why it exists anyway.** That placement is **load-bearing and accidental at the
same time** — the two properties that should never coexist. Flash-Next is 103.7
GiB and fits a 128 GB box *only* because 26.8 GiB of it stays out of GTT
(measured: 81.9 GiB peak GTT + 27.0 GiB RSS, `research/engram/spike-flashnext.log`).
Nothing declares that; a quant-support gap in one backend is holding up the whole
serving story (`RESULTS.md` §6.3). This flag is the declaration that would make
it structural (ARCH §7, §10).

**Flip condition (falsifiable, and it is a detector, not a date).** Flip to
default-on for `qwen4exp` the first time a load is observed in which
`per_layer_token_embd` does **not** land on plain CPU — concretely, when the
`using CPU instead` line for that tensor disappears from the load log, or when
`projected_overheads`' `model_host_bytes` for Flash-Next drops materially below
~26 GiB. Either means the accident has stopped holding and the model is about to
stop fitting.

**The event that could trigger it is known:** the next move of the vendored
llama.cpp pin (`vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT`). Check the detector
above as part of that move — that is cheaper and more reliable than a calendar.

**Review by 2026-10-27**, or the next vendor pin move, whichever comes first.
Owner: whoever moves the pin. Notes `dccee8ed`, `b588ba8b`;
`research/engram/RESULTS.md` §6.1–6.3.

---

### `SOVEREIGN_TENSOR_NO_HOST` — llama.cpp `--no-host`, shipped OFF and MEASURED HARMFUL (2026-08-27)

**What is dark.** Bypassing the device HOST buffer type on a local load — and,
through patch `0008-no-prefetch-when-host-buffer-bypassed.patch`, the load-time
prefetch along with it.

**This row is not "promising, awaiting evidence". The evidence is in and it is
negative for the case the flag was written for.** On Flash-Next, turning it on
converted the mmap-resident engram into an *anonymous* copy — RssAnon 8.85 →
21.47 GiB, RssFile 0.29 → 0.06 GiB — and the load OOM'd. Anonymous pages cannot
be reclaimed; file-backed ones can. Default OFF is the measured answer, not a
holding position.

**Why it is kept rather than deleted.** On a backend whose host buffer *does*
accept the tensor's quant, CPU-placed weights really do land in pinned memory and
this is the lever that moves them. The flag is right for a machine we do not have;
it is wrong for this one. Measure the RssAnon/RssFile split before turning it on.

**THE REAL DEBT THIS ROW RECORDS — a useful capability is welded to a harmful
one.** Patch 0008 gates `ml.init_mappings(!params.no_host, …)`. So "do not
MAP_POPULATE 26.8 GiB of engram before a single row has been gathered" is
reachable *only* through the flag above, which OOMs here. As shipped, the good
half is unreachable in the configuration we actually run. That is one flag
deciding two independent things (ARCH §10.6).

**What this row does NOT gate, contrary to an earlier reading of the code.** The
local-fit gate's discount of `model_host_bytes` is **unconditional** — it does not
require `no_host`, and `wants_no_host` returns false on a default load. A stale
comment paragraph in `rpc_distribution.rs` claimed the opposite directly above the
correction that reversed it; it is deleted, and the log line that said "no_host
load" is corrected. Flash-Next therefore passes the gate on a stock load. If it
did not, this row would be a shipping blocker rather than a debt.

**Flip condition — and it is NOT "turn this on".** This row retires when prefetch
is **decoupled** from `no_host` (its own gate, or keyed to the buft override), and
a paired load with `no_host` OFF measures prefetch-off as neutral-or-better on:
major faults during load, the RssAnon/RssFile split, peak `MemAvailable`, and
load wall-clock. `research/engram/spike-run.sh` is the harness — it already
samples avail/GTT/RSS every 2s across a real load, so the A/B is two runs and no
new instrument. If prefetch-off measures worse or neutral, say so and close the
row REJECTED; an unmeasured "should help" is what this ledger exists to stop.

**Review by 2026-10-27.** Notes `dccee8ed`, `401428eb`, `b588ba8b`;
`research/engram/RESULTS.md` §6.4, §7.

---

### `sec-filings-company` install-by-ticker — `catalog_status = "featured"` since 2026-08-18 (was **preview**)

**FLIPPED 2026-08-18 BY OPERATOR DECISION, over an unmet condition.** The
seat recommended holding and was overruled; that is the operator's call
and it is recorded as a decision, not as a bar that was met. Three of the
four flip conditions below are MET — (1) render pre-ingest, (2) a figure
answered from a ticker-installed corpus with basis and accession, now
proven on BOTH the CLI (ring 0, n=3) and the DESKTOP (ring 2, run 6), and
(3) the install form. **(4), the F3 hand-read at its registered bar, is
NOT met**, and the ring 2 run that would speak to it is RED on a
transport invariant (one trailing newline, `invariants.ts:111`), with the
KO filer and two-filer assertion SKIPPED rather than passed.

**The three known gaps a user can hit, stated so nobody has to rediscover
them** (also carried in `sovereign-recipes/registry.toml` above the entry,
and in notes `9be87107` / `45b04cf5`):

1. **Segment questions with no structural word answer CONSOLIDATED, and
   this is a MEASURED REGRESSION against a pre-registered item, not an
   estimate.** `SecRefusal::ScopeNotInSource` catches "Mac **segment**
   revenue" but not "**Mac** revenue" / "**Services** revenue" — it keys
   on structural vocabulary, never on a filer's product names, which
   companyfacts does not carry.
   - Frozen-set item `segment-services`
     (`sovereign-recipes/sec-filings-company/prereg/aapl-fabrication-set.toml:62`,
     `question = "What was Apple's Services revenue in fiscal 2025?"`,
     `expect = "refusal"`) **passed on 2026-08-16** (`aapl-fabrication-
     n3run1_20260816.jsonl`, 9/9) and **does not now**: the planner sends
     `concept="revenue"` and the turn serves consolidated
     `$416,161 million` for a Services question. True Services revenue is
     `109,158` — listed in that item's own `forbidden_values`.
   - **Cause is the enum, by mechanism.** Pre-enum an out-of-vocabulary
     ask drifted to something unmapped and refused honestly; post-enum the
     planner is reliably pushed into the closed set, picks the nearest
     LEGAL member, and the tool answers it. The enum is a real improvement
     on in-vocabulary asks and a real regression on out-of-vocabulary ones.
   - **SCORED, whole set, 2026-08-18: `8/9 passed` — HONESTY 1, bar
     ZERO.** `segment-services` FAILED with "unattributable numeral(s):
     $416,161 million". Every other item passed, including
     `prose-explanation-mac`, `period-calendar-trap`, `period-beyond-asof`
     and `arithmetic-yoy-revenue` (11 figures, all attributable).
     Artefacts committed alongside the prior runs:
     `sovereign/bench/sec-filings/results/aapl-fabrication-postenum-scope_20260818.jsonl`
     (+ `-records_`). Compare `aapl-fabrication-n3run1_20260816.jsonl` = 9/9.
   - BOTH instruments validated before the result: `run_frozen_set.py
     --self-test` (6/6 controls, watched reading fired and not-fired) and
     `scripts/check-sec-answer-path.py --self-test` (watched FAILING on 5
     tampered controls — 4 honesty, 1 competence — and passing on 4 clean).
   - This is the highest-severity live gap: a real, cited,
     wrong-granularity figure rather than a refusal, and it crosses a
     registered bar whose threshold is zero.
2. **The e2e has never gone green end to end** — nine attempts. Run 6 got
   the figure on the desktop and then failed a byte-equality invariant.
   Unattributed; the newline is not chased.
3. **F2 segment honesty is unsettled and this flip does not settle it**
   (`e8b9319b`) — companyfacts is consolidated-only by construction.

**Review-by 2026-08-30 stands.** If gap 1 is still open then, this row is
the place to argue for reverting to `preview`.

---

_Historical record below — the reasoning while this row was `preview`._

### `sec-filings-company` install-by-ticker — `catalog_status = "preview"` (not **featured**)
- **Shipped dark:** 2026-08-16, order `financial-corpora` slice 2
  (worker `sec-recipe-install`). The recipe installs one company's 10-K
  by ticker with no repo script: `sec_edgar` acquirer + registration on
  all four engines that can ingest, and `[parameters.ticker]` as the
  user's sole input. `preview` keeps it out of the desktop catalog's
  featured surface — a user cannot arrive at it by browsing.
- **Why dark rather than shipped (2026-08-16, superseded — see below):**
  the acquirer deliberately left `companyfacts.json` untouched under
  `raw/`, because rendering figures there would be a SECOND
  implementation of the `sec_facts_render::render` decider (ARCH §10.6).
  Until that renderer was called pre-ingest, an installed corpus carried
  PROSE ONLY and the `sec_facts` tool could not claim it.
- **Why still dark, 2026-08-17 (order `sec-filings-last-mile`, M4):** the
  reason above is GONE — the acquirer now calls the decider at step 6 and
  `place_rendered` writes `docs/facts/*.txt` pre-ingest and stages the
  sidecar where `install_fact_sidecar` already places it synchronously.
  What is missing now is not code, it is a RUN. The journey has never
  been executed end to end on this host: three attempts, none of which
  reached the assertions — a harness livelock in `global-setup`'s own
  fixture ingest, then a 20.5-minute install that was TOTALLY SILENT
  because the daemon's tracing allowlist did not carry `sec_edgar`, then
  a workspace build broken by an unrelated in-flight change. So the
  install is UNPROVEN, not disproven, and an unproven button stays off.
  Arming it now would ship exactly the failure F3 exists to prevent, on
  a guess: a user picks a ticker, waits, and finds out.
- **Evidence so far, cited:** live against real SEC, not a fixture —
  `svrn recipe test … --params ticker=AAPL --recapture` VERDICT GREEN
  on Apple 10-K accession 0000320193-25-000079. ACQUIRE 5 docs from
  `custom:sec_edgar`, 0 empty; CHUNK 0 over the 3000 limit
  (2587..2621); FTS rare-token probe returns its source chunk. 22
  `sec_edgar` tests, instrument validated (a deliberately broken
  assertion took the run to exit 100 naming the right test).
- **Flip condition (falsifiable):** `catalog_status` moves to
  `featured` when ALL of (1) `sec_facts_render::render` is called
  pre-ingest, writing `fact_files` into `docs/facts/` and staging its
  sidecar where `install_fact_sidecar` places it; (2) `sec_facts`
  answers a figure from a corpus installed BY TICKER — not from a
  script-built one — with fiscal-period basis and accession citation;
  (3) the desktop install form exists and passes `ticker`; (4) the F3
  hand-read passes at its registered bar. Any one unmet keeps it
  `preview`.
- **Condition audit, 2026-08-17 — two met, two `never-ran`:**
  (1) **MET** — `sec_edgar::acquire` step 6 + `place_rendered`; 4 tests,
  instrument validated (redirecting the sidecar write reddened exactly
  one test, two controls in the same file stayed green).
  (3) **MET** — `RecipeParameterForm.svelte`, rendered from
  `ParameterSpec.kind` with nothing ticker-specific; 8 tests, one of
  which renders a DIFFERENT recipe's `int`/`date`/`list` parameters with
  no new code.
  (2) and (4) are **`never-ran`, NOT failed** — the e2e that would decide
  them (`tests/e2e/real/sec-filings-by-ticker.real.spec.ts`) has never
  reached its assertions. Recording them as failures would put a wrong
  verdict against work that is probably correct; recording them as met
  would be the F2 mistake in another costume. They are unmeasured.
- **Condition audit, 2026-08-18 — (2) moves `never-ran` → FAILED, for a
  cause that is not the corpus.** Order `sec-filings-close`, ring 2
  (`runs/sec-filings-close-e2e/`, `DONE 15:35:15Z`). The e2e reached its
  assertions for the first time in seven attempts.
  - **Arming is PROVEN on the desktop.** `sec_facts: discovery complete
    declared=1` → `coarse=Some("AUTHORITY_CLAIM")`, neither ever logged
    on this surface before. Registering `SecFactsTool` (`655c6ab5`) is
    the whole of the change; run 4 logged `not armed — no evidence
    corpus declares authority handler="kq_stream"` at the same point.
  - **(2) FAILED — no figure was answered.** The planner called the tool
    with `concept="capital_expenditures|acquisitions|property_plant_equipment"`,
    a pipe-alternation hedge. `resolve_concept` is a DECLARED two-step
    resolver that never similarity-guesses (§18.3), so it normalized that
    to a single id containing pipes, matched nothing, and refused
    (`outcome="unmapped"`, `store_concepts=20`). The store DOES hold
    `capital_expenditures` — the first alternative would have resolved
    at step 1.
  - **Two structural defects behind it, neither in scope for that order.**
    (a) `concept` is a CLOSED set — the store's 20 ids, known at call
    time — passed to the planner as free text with six examples and no
    `enum`, while `mode` in the same schema IS an enum (ARCH §2 /
    principle 9; §7.6 on asking a model to guarantee what a schema can
    enforce). (b) `executor: step done success=true` for a step whose
    tool REFUSED — an `Err` collapsed into a success-shaped value (§18.3,
    smell table). The empty basis is why the provenance audit then
    flagged all four numerals: with no tool datum in the basis,
    "untraceable" is correct by construction, so the guard is right about
    a symptom.
- **Condition audit, 2026-08-18 (later) — (2) moves FAILED → MET.** Order
  `sec-facts-concept-enum` + the planner change it was blocked on
  (`format_param_hint` now renders a declared `enum` into the plan
  prompt). Ring 0, `svrn chat ask` against `sec-cik0000320193`, n=3:
  - The planner sent `concept="capital_expenditures"` — a bare canonical
    id — **3/3**. No hedge, no label, no pipe alternation. The prior
    failing inputs were a label (`"Payments to acquire property, plant
    and equipment"`, deterministic 3/3) and the pipe hedge; both are gone.
  - **A figure was answered, with basis and citation, 3/3 identical:**
    `capital_expenditures = $12,715,000,000.00`, us-gaap
    `PaymentsToAcquirePropertyPlantAndEquipment`, period
    `2024-09-29..2025-09-27`, Form 10-K accession `0000320193-25-000079`,
    plus a reproduce path to the companyfacts URL. Zero `refusal emitted`.
  - **Still `preview`, because (4) is unmeasured.** (1) (2) (3) are now
    MET; the F3 hand-read has never run. Any one unmet keeps it `preview`,
    so this does not flip the row — it removes the blocker that made the
    initiative close NOT-SHIPPABLE.
  - Ring 2 (desktop e2e) NOT re-run: ring 0 proves the planner→tool→basis
    path, and the desktop's own arming was already proven at `655c6ab5`.
  - **NEW BLOCKER, same session — the REFUSAL half now fails, 2/2.** This
    registry entry's flip condition wants an answered figure AND a refusal
    naming what IS available; only the first is met. Asked for Apple's
    **Mac** and then **iPhone** segment revenue, the planner sent
    `concept="revenue"` both times and got consolidated $416,161M, which
    the model narrated as "Apple Inc.'s Mac segment revenue in FY2025 was
    $416,161 million". The enum closed the in-vocabulary hole and
    SHARPENED this one: the schema tells the planner to "send the closest
    single id and let the tool refuse", but that premise is false when the
    closest id is in the store — so no refusal ever fires (§18.3, moved
    from the resolver to the planner). The provenance guard withheld both
    answers only INCIDENTALLY — on a rounding restatement (`$416.2
    billion`) and on an accession fragment (`0000320`) — neither catch
    about granularity. Detail + fix shape (compare the ask against the
    resolved concept at the `ToolContext` seam, as M1b already does for
    period): note `45b04cf5`. **Do not flip until this refuses.**
  - **CLOSED, same session.** `SecRefusal::ScopeNotInSource` +
    `scope_qualifier_in_question`, wired at the same seam as
    `PeriodNotAsAsked` and gated on `store.coverage.consolidated_only`.
    Re-run of the exact failing probe: the planner still sends
    `concept="revenue"` (the enum correctly pushes it into the closed
    set), and the tool now REFUSES — "this question asks for a
    'segment'-level figure … the consolidated 'revenue' figure is NOT
    that number and is not offered as a substitute", followed by all 20
    typed concepts. Consolidated control unchanged: capex still answers
    `$12,715,000,000.00`, zero refusals. So both halves of this entry's
    flip condition — an answered figure AND a refusal naming what IS
    available — now hold at ring 0.
  - **RESIDUE, disclosed not hidden (§18.3).** The guard keys on
    STRUCTURAL segment vocabulary (`segment`, `division`, `business unit`,
    `product line`, `by region`, `geographic`, …), deliberately not on one
    filer's product names, which would not transfer to the next company.
    So "What was **Mac** revenue in FY2025?" — a product name with no
    structural word — is NOT caught and would still answer consolidated.
    Closing that needs the filer's own segment names, which companyfacts
    does not carry. Scoped to the clear case exactly as the calendar check
    was, by the same operator direction.
- **Ring 2, run 6 (`runs/sec-filings-scope-e2e/`) — the figure LANDED on
  the desktop, and the run is still RED.** Both are true and neither
  cancels the other.
  - **The substantive win, first time in nine attempts.** Installed BY
    TICKER through the catalog, in `sovereign-desktop`, the turn returned
    `capital_expenditures = $12,715,000,000.00 — us-gaap
    PaymentsToAcquirePropertyPlantAndEquipment, period
    2024-09-29..2025-09-27, Form 10-K accession 0000320193-25-000079`.
    That is condition (2) on the surface that ships — the inference M2.5
    existed to stop us making, now measured instead of inferred.
  - **Why it is still red, and it is NOT the figure.** The spec aborted in
    `assertTurnInvariants` (`invariants.ts:111`) on a TRANSPORT invariant:
    `concat(message-chunk)` differs from `message-complete.full_text` by
    exactly one trailing newline. It failed BEFORE reaching the figure
    assertion at `:475`; the figure is visible only in the failure diff.
    The describe block is serial, so **KO and the two-filer assertion
    never ran** (skipped, not passed).
  - **UNATTRIBUTED.** Neither order touches the answer-rendering or
    streaming path — the enum changes a prompt, the scope guard is inert
    here (the spec has no segment vocabulary, grepped). Leading hypothesis
    is REVEALED-not-caused: this assertion had never executed on an
    answered figure in eight prior attempts, so a pre-existing off-by-one
    newline would surface exactly now. **That is a hypothesis, not a
    finding** — proving it needs a control run on a stashed tree, which
    was not spent.
  - **Condition (4) therefore does NOT pass, and the row stays
    `preview`.** A red e2e is not an F3 hand-read at its registered bar.
- **Instrument gap this run exposed:** the answer TEXT is preserved
  nowhere in `evidence/` — only the audit's violation list — so whether
  `2023`/`2024` were prose years or claimed data CANNOT be decided from
  this run's record. That question is left open rather than guessed.
- **Known gap this does NOT cover:** segment figures. The acquirer
  refuses segment concepts because companyfacts is consolidated-only;
  the F2 honesty violation at `e8b9319b` is the same gap seen from the
  answer end (prose-served Mac segment numbers). Flipping this row
  does not settle F2.
- **Review-by:** 2026-08-30. If slice 3 closes without the flip, this
  row moves to Rejected naming which of the four conditions failed.

### Batched claim verify (one prefill, N verdicts) — `SOVEREIGN_GATE_BATCH_VERIFY` (default **OFF**)
- **Shipped dark:** 2026-08-14, order `audit-economy` D2 (approval
  directive 086f6682; worker directive 233d3558). The family-joined
  batched register (D1, fc58319d) plus the asymmetric-trust wiring:
  batch "supported" clears without a per-claim call; "unsupported" and
  parse gaps fall to the calibrated per-claim judge, so released flags
  stay calibrated by construction.
- **Evidence so far, cited:** replay recalibration on the pinned v2
  set — catch 0.950 / clear 1.000 (vs the calibrated register's
  0.900/0.750 on the same labels), ZERO (c)-class loss, bit-stable;
  population sweep 3/407 flips, all hand-read (a)-class
  (`audit_economy_d1_batched_recalibration_20260814.md`). Measured
  batch support rate 53.7% => predicted per-claim term ~6.2s vs 11.1s
  baseline.
- **Flip condition (falsifiable):** (1) live smoke
  (`runs/audit-economy-d2-smoke/`) batch+judges call-sum <=6.5s median
  — the POST-DATA AMENDED bar per directive 6686251c (registered bar
  was 5.5s; amended after D1 measured the 53.7% support rate; recorded
  as amended everywhere it appears); (2) frozen-3 live arm 3/3;
  (3) dropped-catch read zero unexplained (c)-class; (4) paired chaos
  CONFAB-LEAK NEW<=OLD; (5) composed after-arm audit#1 median <=16.8s
  with p90 <=90s re-judged. Promotion to default-on is OPERATOR-HELD.
- **Settling plan item:** order `audit-economy` steps 4-6 (directive
  233d3558) — the D2 smoke, the full live discipline, the composed
  after-arm vs 688f8eba.
- **Review-by:** 2026-08-28. If the order closes without the flip,
  this row moves to Rejected with the curve that said no.

### TOMBSTONE 1/2 — the longform REWRITE pass — `SOVEREIGN_GATE_LONGFORM_REPAIR` (default **OFF**)
- **Shipped tombstoned:** 2026-08-14, order `gate-tombstone-ladder`
  (Phase 4 of `sovereign/docs/specs/NATIVE_GROUNDING_ECONOMY.md`),
  operator directive `c256c16f`. **This is a tombstone, not a
  delete** (§9.0): the code stays, the path stops executing on the
  default configuration, and the switch that re-runs it is this row.
- **What stopped executing:** the repair pass on the longform path —
  surgical span-edits on the fast slot, and its full-re-synthesis
  fallback. A draft whose audit found failures is now released with
  those claims marked instead of re-written.
- **Why the grounding function is undiminished:** §3.3 G2 — marking
  discharges G2 completely; the rewrite discharged a *presentation*
  preference, at wall cost and at honesty cost (a rewritten answer no
  longer shows where it was thin). The mark is a `failed_once` holding
  in a `mixed`-verdict epistemic ledger, which ships and renders on the
  desktop today (D0 inventory, note `e1e9e7a3`: verified live on 9 of
  17 captured desktop turns, zero exceptions).
- **Measured expectation, cited not promised:** 5.4s/turn — the
  mechanism's price after the Phase 2 cap fix (§7.3.1), *not* the 43.2s
  the plan's first draft booked.
- **Flip condition (re-arm):** `SOVEREIGN_GATE_LONGFORM_REPAIR=1`
  re-arms this and Tombstone 2/2 together. Re-arm if the pre-registered
  kill **K2** fires — the chaos gate regresses hallucination beyond lane
  tolerance, in particular the 2026-07-17 CONFAB-LEAK probe — or if the
  operator judges the marked answer unacceptable to read
  (`E-operator-holdout` is terminal). Under tombstone-then-delete that
  retreat is a flag flip, not a revert, which is why the ratchet was
  retargeted.
- **Settling plan item:** Phase 5, the single deletion pass, triggered
  when these tombstones have held across the window below, `E-wall-time`
  and `E-variance` have readings, and the operator says the new stack is
  right.
- **Review-by: 2026-09-13** (30 days). Per **K8**: if this path is still
  tombstoned-but-undeleted past that date with no dated Phase 5 trigger,
  tombstone-then-delete has collapsed into "nothing is deleted until H0
  graduates" wearing a new hat, `E-tombstone-ledger` fails, and the
  deletion pass is scheduled by the seat rather than waited for.

### TOMBSTONE 2/2 — AUDIT #2, the re-audit — `SOVEREIGN_GATE_LONGFORM_REPAIR` (default **OFF**)
- **Shipped tombstoned:** 2026-08-14, same order, same commit, same
  knob as Tombstone 1/2. It has **its own row** because it is its own
  path with its own cost, and a ledger that folded it into the rewrite's
  row would hide the larger of the two numbers.
- **What stopped executing:** the full re-audit of repaired text — claim
  re-extraction and the per-claim judge fan-out over prose the rewrite
  had just produced.
- **Why it needs no separate flip condition:** audit #2's only input is
  the rewrite's output (`StageCause::RewriteProducedNewProse`). With the
  rewrite tombstoned there is no new prose to audit, so this path has
  nothing to run on. It is tombstoned *by consequence*, and it re-arms
  in lockstep.
- **Why there is deliberately NOT a second knob** — the one design
  decision in this pair worth reading twice. A separate re-audit flag
  would make **"rewrite ON, re-audit OFF"** reachable. That is precisely
  the configuration attempted on 2026-07-17, which shipped unaudited
  regenerated prose and leaked a GK-caveated fabrication (CONFAB-LEAKED
  0→1); it was reverted and §7.4 forbids re-proposing it. One switch
  keeps the unsafe combination unreachable **by construction** rather
  than by anyone remembering (ARCH §7, §10.6). A knob whose only safe
  value is one value is not a knob; it is a trap.
- **Measured expectation, cited not promised:** 50.9s on the operator's
  turn — the larger half of this phase, and the reason the pair is worth
  the two rows.
- **Settling plan item / Review-by:** as Tombstone 1/2 — Phase 5,
  **2026-09-13**.

### ~~H1 tau overrides — `SOVEREIGN_NG_TAU_ABSTAIN` / `SOVEREIGN_NG_TAU_ANSWER`~~ — RETIRED 2026-08-10
- **Retired the same day they shipped, by their own written clause.**
  The row's flip condition said these "do not outlive Step 3's
  conclusion" and named branch (b) — per-corpus thresholding recorded
  failed — as one of the two settled end states. D5 returned exactly
  that: 0.65/0.65 against a 0.71 bar, with the margins interleaved
  (present m=1.19 *below* absent m=1.31), so no operating point on this
  signal separates the two classes (`step3/d5_verdict.json`, note
  `d6911acb`).
- **Executed:** order `native-grounding-p1-desktop`, per the parity
  plan's P1 Deletes ledger (`NATIVE_GROUNDING_PARITY_PLAN.md` §7).
  Deleted: both env reads, `apply_tau_overrides` and its `TauSource`
  enum, the override test, and both `quality/env-flags.toml` rows.
  `effective_thresholds()` now returns the committed calibration and
  cannot return anything else — one ruler, structurally, and a test
  asserts the two env names appear nowhere in the module.
- **The finding survives the knobs**, which is the point of retiring
  them here rather than silently: per-corpus thresholding on the
  reranker margin is closed by measurement, not by opinion. Re-opening
  it needs a new signal, not a new threshold.
- **Nothing to review by:** there is no flag left to review.

### Local journals — `SOVEREIGN_JOURNAL` / `SOVEREIGN_NEXT_EDIT_JOURNAL` (default **ON**)
- **Shipped:** 2026-08-07, default-on, with the developer handover.
- **Why it is in this ledger at all** — it is not dark, it is the
  opposite: a default-ON **local write** the user did not ask for. The
  ledger's job here is to hold the boundary rather than the flip. The
  boundary: recording locally is on, **sending is never** — there is no
  network path out of `types::next_edit_journal`, and `svrn journal
  bundle` writes a file plus a manifest of every field in it so the
  developer decides what leaves the machine after reading what is in
  it. If a future push adds a submit path, this row is the review that
  has to happen first.
- **Scope of this row:** the JOURNAL LAYER, not just next-edit.
  `sovereign-contracts/src/types/journal.rs` is feature-agnostic and
  next-edit is its first stream; a second stream inherits this row's
  boundary rather than minting its own, and `SOVEREIGN_JOURNAL=off`
  covers every stream including ones added later.
- **Second stream (2026-08-07): `grounding`** — one decision line per
  gated answer (verdict, score, tau, action, `(corpus, chunk-id)`
  evidence handles; never claim/answer/chunk text — canary
  `no_content_bearing_field_can_reach_a_line`). It is the VERIFIER_V0.md
  §6.1 phase-0 collector: the training/calibration substrate for the
  deferred second-judge slot, gathered from the incumbent-only gate that
  16 GB nodes actually run. Its own settle condition: after ~2 weeks,
  `svrn journal grounding stats` shows an evidence-handle coverage high
  enough (≥80% of chunks resolvable) that a mining pass can re-judge
  what the gate judged — below that, the chunk-target plumbing on the
  non-corpus surfaces gets fixed or the field is documented as
  corpus-lanes-only, rather than left silently bounding every future
  mining pass. Off switch: `SOVEREIGN_GROUNDING_JOURNAL` /
  `svrn journal grounding off`.
- **What it does:** one metadata-only record per `POST
  /v1/edit_predictions` at `~/.svrnmesh/journal/next-edit-<date>.jsonl`
  (14-day retention, 8 MiB/day cap), plus one line per outcome the
  editor reports (`accepted` | `dismissed` | `diverged` |
  `superseded`). Metadata-only is structural, not a convention:
  `NextEditEpisode` has no free-form or `serde_json::Value` field, so
  there is no channel a document, a region, a needle, a rewrite or a
  file path could travel through. Two tests hold it —
  `no_code_bearing_field_can_reach_a_line` (contracts) and
  `debug_extraction_carries_no_code` (commonwealth-api) — each feeding
  a canary through every field a caller might smuggle it in.
- **Why default-on:** the terminal milestone is a small group of Go and
  TS/React developers using this and their experience returning as
  evidence. A journal nobody switched on returns nothing, and the
  alternative to a local record is asking people what they remember.
- **What settles it (falsifiable):** after the first cohort week, `svrn
  journal stats` on at least 3 machines reports ≥20 judged episodes
  each. Then either (a) the acceptance rate is actionable and the
  journal has earned its default, or (b) coverage is so low the outcome
  reporting is not working, and the extension half gets fixed or
  removed rather than left recording into a number nobody can use.
- **Cost of on:** one ~600-byte append per prediction, off-thread, and
  a directory the user did not create. Unquantified: nobody has
  measured the append against the p50 (1.2 s) — expected to be
  invisible, and it cannot fail a request by construction (`record`
  drops its join handle).
- **Review by:** 2026-09-05. If no cohort data exists by then, the
  honest move is to say the handover did not happen, not to extend the
  date.
- **Decision:** note `09599af1` (outcome telemetry: four-way and
  invisible; reverses an earlier daemon-side-journal-only call in the
  same session).



### Corpus relevance prefilter — `SOVEREIGN_CORPUS_PREFILTER_TOPK` (unset)
- **Shipped:** dark, pre-2026-08; row added 2026-08-05 on first real
  measurement.
- **What it does:** on an UNSCOPED turn, prunes the eligible corpus set
  to the top-K by query↔centroid cosine before the fan-out.
- **Proof so far:** measured at `K=5` on
  `bench sep/summarize --prod-pipeline` (14 questions, 420 chunks,
  deterministic): off-topic evidence 11.0% → **10.2%**, no change to
  fact recall (0.7500 / 0.7833). The centroid ranking is genuinely
  discriminating — sep 0.59 and wikipedia 0.59 against
  conversations-anthropic 0.39 — so the mechanism works.
- **Why it under-delivers, and this is the actionable part:** the trace
  shows `kept=9` at `top_k=5` — five corpora earned a slot on relevance
  and **four more were admitted by the "always keep `personal_scope`
  regardless of score" carve-out**. Those four are the entire residual
  (31 of 43 off-topic chunks). The prefilter cannot fix what it is
  required to exempt.
- **Flip condition:** a run on a bank that contains BOTH reference-corpus
  and personal-corpus questions shows top-K pruning holding personal
  recall flat while cutting off-topic share. Flipping it on today's
  evidence would be tuning against a bank that can only see one side.
- **Settled by:** the personal-corpus bench bank (see
  `docs/RETRIEVAL_AUDIT_2026-08-04.md` §D1-residual) — unowned. If no
  tranche claims that bank by the review date, kill this row rather than
  re-dating it a third time.
- **Review by:** 2026-09-05.
- **Notes:** `8758759a`, `c9aa59c6`.
- **2026-08-13 (order `mesh-scale-t1-retrieval`) — a SECOND reason it was
  not flippable, now removed.** The mesh-scale red baseline measured the
  prefilter running **once per fan-out, 4× per turn**, each pass linear in
  corpus count (~0.73 s per 100 corpora per pass). At n=1000 turning it ON
  made the turn 35% SLOWER — 22.1-22.4 s → 29.7-30.6 s — so on a large
  install it was a net latency REGRESSION regardless of its recall effect
  (`MESH_SCALE_100_USERS_1000_CORPORA.md` §8.3.4). `SOVEREIGN_EXPANSION_SCOPE`
  collapses it to one pass per turn structurally (a scoped fan-out skips the
  prefilter), which removes the multiplier. The flip condition above is
  unchanged and still governs — this note only records that the cost
  objection is no longer one of the blockers. The var is now declared in
  `quality/env-flags.toml` and off the env-gate waiver baseline.

### Expansion fan-out scope — `SOVEREIGN_EXPANSION_SCOPE` → **GRADUATED 2026-08-13, default ON**
- **Shipped:** dark 2026-08-13, order `mesh-scale-t1-retrieval`; flipped to
  default ON the same day on verdict 94f01eb2. Both events in this row.
- **What it does:** scopes every expansion fan-out — entity boost, query
  decomp, title expand, demand-plan fan-out, graph-neighbor, and the two
  lanes SPAWNED at `ppr_struct_spawn` (PPR structural + entity obligations) —
  to the top `SOVEREIGN_EXPANSION_SCOPE_CORPORA` (=8) CORPORA of the MAIN
  fan-out, ranked by each corpus's best chunk under
  `reweight_by_query_relevance`. Decided once in `step_main_retrieval_mesh`
  and threaded through the single accessor
  `PipelineState::expansion_corpora()`. Bounded above by 8 corpora however
  many are installed. Side effect, structural and free: a scoped fan-out skips
  the corpus prefilter, so that runs once per turn instead of once per
  fan-out.
- **The red it attacks:** per-turn retrieval wall LINEAR in corpus count at
  **2.19 s per 100 corpora** (5-point sweep, intercept 0.38 s, within 5% at
  every point), with a fixed 4 fan-outs/turn — 1 KnowledgeQuery + 3
  EntityBoost, the latter ~62% of the fan-out wall at n=1000
  (`MESH_SCALE_100_USERS_1000_CORPORA.md` §8.3.3).
- **Proof:** `MESH_SCALE_100_USERS_1000_CORPORA.md` §8.4. Slope **2.183 →
  0.849 s per 100 corpora, a 2.57× cut**, on the red's own 5-point harness;
  the three EntityBoost passes fall 13,346 → 106 ms at n=1000. The flag-OFF
  arm was re-measured on every rig and binary revision and reproduced the red
  six times (2.176-2.193 vs 2.19) — the instrument was validated before any
  green number was read. Quality: SEP-at-rig anchor **42/66 + 137/158,
  byte-identical**, at 190.1 s vs 321.0 s (41% wall cut); sep and cross-corpus
  banks identical; wikipedia sources identical, −1 fact (reproduced).
- **Also carries the scale-vs-recall dial** `SOVEREIGN_EXPANSION_SCOPE_CORPORA`
  (default 8). Two earlier producers were measured WRONG and are recorded in
  §8.4.1 — "corpora that produced hits" selected 50 of 50 (a no-op), and
  raw-score ranking on a chunk budget scoped 14 of 20 wikipedia questions to
  `sf-assessor-roll` alone. Ranking on `reweight_by_query_relevance` is what
  works.
- **Known limit, named not silent:** the sweep rig's 1000 corpora are `cp -r`
  clones of ONE index, so all of them score identically and the top-8
  selection is tie-arbitrary there. The rig can prove the BOUND (≤8 corpora
  searched per expansion regardless of n) but not selection QUALITY. Quality is
  carried by the SEP-at-rig anchor (one real corpus among stubs — a real
  relevance gradient) and the bank battery on real corpora. A heterogeneous rig
  is banked as a Tier-2 improvement.
- **Flip condition (as written when the row was dark) — MET, clause by clause,
  2026-08-13:**
  1. *Slope ≤ ~0.55 s per 100 corpora* — **met against the RE-CUT bar of
     ≤~0.9, not the original 0.55.** Measured **0.849**. The original 0.55 was
     unreachable by arithmetic, not by effort: it was the AVERAGE over four
     unequal fan-outs, and the one fan-out this order does not scope (the main
     KnowledgeQuery pass) costs **0.84 s/100 on its own**. A turn cannot come
     in under the cost of the fan-out it must always run. The re-cut is
     recorded in §8.4 with that derivation, and it is a SUBSTITUTED bar — named
     here rather than quietly satisfied.
  2. *SEP-at-rig anchor holds 42/66 + 137/158* — **met exactly, byte-identical**,
     at 190.1 s vs 321.0 s (41% wall cut).
  3. *SEP 21-q, wikipedia, cross-corpus banks inside their noise bands* —
     **met**: sep and cross-corpus identical, wikipedia sources identical with
     −1 fact of 130 (reproduced, so real but immaterial).
  4. *`sovereign-ci-bench.sh --quick` green on `retrieval-prod`* — **met on the
     lane, SUBSTITUTED at the suite level.** Both `retrieval-prod` lanes PASSED
     (HARD, feature ON, against flag-OFF baselines), as did the other three
     HARD retrieval/enrichment lanes. The suite-level aggregate VERDICT was
     **never produced** — the run died during the advisory `chaos-monkey` lane
     when its harness wrapper was reaped. Per-lane evidence was accepted in
     lieu of the aggregate by verdict 94f01eb2. One unrelated HARD lane,
     `routing`, failed with 3 regressions; it runs `bench all --routing-only`,
     which drives ONLY the intent classifier (no retrieval, no synthesis), so
     this feature's code path is never entered. That failure predates and is
     independent of this row and still wants an owner.
- **Settled by:** order `mesh-scale-t1-retrieval`; landing verdict **94f01eb2**,
  operator direction "approve with the flip", 2026-08-13.
- **Status: GRADUATED to default ON, 2026-08-13.** The flag survives as the
  OFF-switch (`=0/false/off/no`); `SOVEREIGN_EXPANSION_SCOPE_CORPORA` stays at
  8. The corpus prefilter (`SOVEREIGN_CORPUS_PREFILTER_TOPK`) stays UNSET on
  the recommendation in §8.4.3 — even hoisted to one pass per turn it is a net
  loss (1.828 s/100 with it against 0.849 without), because its own probe is
  O(n).
- **Review by:** n/a — graduated. Row retained here rather than moved, because
  its flip-condition audit above is the evidence for the default and belongs
  next to it.

### Multi-quote citation contract (`SOVEREIGN_CITATION_MULTIQUOTE`) → **GRADUATED 2026-08-05, default ON**
- **Moved to GRADUATED the same day it shipped dark.** The row stays
  here rather than in the Graduated section only because its whole
  argument is the DARK row below it; read the two together. Flip
  landed in `grounding/config.rs::citation_multiquote_enabled`.
- **What settled it** (matched control — same HEAD, same day, same
  local topology, `saltgrass_compound` n=7, **0 extraction failures in
  both arms**, `=1` vs `=0`):

  | metric | `=0` (legacy) | `=1` (shipped) |
  |---|---|---|
  | citation releases | 0 | **3** |
  | competence-when-present | 0.14 (1/7 correct) | **0.43 (3/7)** |
  | misses attributed to gate | 4 | **2** |
  | blatant-confab-rate | 0.00 | **0.00** |

  Both halves of the flip condition are met: releases > 0, and confab
  did not regress. **The known risk did not materialise** — it did not
  trade a full correct legacy answer for a partial one. It RECOVERED
  two turns the legacy ladder was abstaining away
  (`compound-sentence-then-inn`, `compound-constable-then-finder`:
  Abstained → `citation_grounded`), which is the "kills 3-4 correct
  drafts per run" cost the dark row predicted, now measured at 4 → 2.
- **Caveat carried forward, not hidden:** n=7 on one bank. The
  competence delta is 1/7 → 3/7 — direction is unambiguous and the
  mechanism is understood, but this is not a CI-separated result and
  the n≈20 compound bank should re-confirm it.
- **Shipped:** 2026-08-05, dark.
- **Proof so far:** the defect is measured and deterministic, the cure
  is not yet. Quote-first citation grounding releases on **0 of 14**
  compound probes (chaos-monkey `saltgrass_compound`, n=7 × 2
  independent runs, 2026-08-04): every probe ends `ANSWER: NONE`
  because the prompt demands the ONE sentence answering the whole
  question, and a two-part question has none. In
  `compound-inn-and-innkeeper` the model copied the correct sentence
  for part one and still answered NONE. Consequence: `cites_a_source`
  is 0/7 structurally, and all 14 fall through to the legacy ladder,
  which then kills 3–4 correct drafts per run.
- **Flip condition:** an arm-C chaos + situated run vs the arm-A
  baseline shows citation releases > 0 on the compound bank AND no
  regression in blatant-confab-rate (currently 0.00) — i.e. the
  partial releases are grounded, not padded. Overlapping CIs on the
  situated dimensions do NOT settle it either way at n=7; the bank
  grows first (see below).
- **Settled by:** P4 arm C (this initiative's A/B), then the n≈20
  compound bank.
- **Known risk this must clear:** unlike `SOVEREIGN_CITATION_BROAD`
  this is *not* purely additive — it converts a legacy-ladder turn
  into a partial citation release, so it could in principle replace a
  full correct legacy answer with a grounded-half-plus-named-gap. The
  arm measures exactly that trade.
- **Review by:** 2026-09-05.


### Next-edit consult gates `fanout_insert` + `param_insert` (detected, declined)
- **Shipped:** 2026-08-06, dark — `next_edit_model::should_consult`
  returns `Consult::No { skipped: "fanout_insert_deferred" }` /
  `"param_insert_deferred"`. Detection is unchanged, so both stay
  visible in the admission table; only the consult is withheld. Joins
  `casing_deferred`, deferred the same way in v1.
- **Why:** scored **per admitting gate** rather than per bank shape on
  the golden set (`gym/next-edit/golden/`, 1,098 cases, note
  `2c22ec10`), the three consult reasons are three different bets:
  `multiline_fanout` 17 useful / 1 wrong (94.4%), `fanout_insert` 2/17
  (10.5%), `param_insert` 2/6 (25.0%). `fanout_insert` was also the
  path by which 7 `neg_literal_trap` wrong fires reached the model.
- **Cost of off:** 4 useful edits, measured — paired, deterministic
  pipeline, same 1,098 cases. Bought 23 fewer wrong fires; all 27
  changed cases moved one way, none regressed. System goes 36.0%
  useful / 21.0% wrong-fire → 35.4% / 15.2%, which is a LOWER
  wrong-fire than disabling the model lane entirely (33.1% / 15.8%).
  Model-lane p95 1748ms → 9ms.
- **Flip condition:** a candidate model scores ≥60% useful on the
  `fanout_insert`-admitted slice (n=41) and `param_insert`-admitted
  slice (n=19) of the golden set, with ≤1 wrong fire on the negatives
  each gate admits. Re-measure with `--force-consult` + `compare_runs.py`;
  the admission counts those gates still log are the denominator.
- **Settled by:** the next bakeoff arm scored on the golden set —
  zeta-2 and instinct have never been run against it. Until one is,
  this is a property of sweep-1.5b only.
- **Review by:** 2026-09-06.

### Next-edit syntax site filter — dark for TypeScript, JavaScript, Python
- **Shipped:** 2026-08-06 (`5a962765`), ON for Go and Rust only.
  `next_edit_syntax::PROVEN_LANGUAGES = ["rust", "go"]`. The grammars
  for typescript / tsx / javascript / python are compiled in and the
  parse works — the filter is withheld, not unavailable.
- **What it does:** parses the live buffer and keeps only candidate
  sites whose node-kind chain matches a site the user ALREADY edited
  (the occurrences of the rule's `replace`). Targets the largest
  measured defect in the feature: only ~34% of proposed hunks were
  edits the author actually made.
- **Why dark for TS:** it measured WORSE there. On the React/TS bank
  (`gym/next-edit/golden/cases.react-ts.jsonl.gz`) useful-fire
  52.0% → **41.2%** and wrong-fire 6.2% → **9.7%**, with `.ts` wrong
  fires going 2 → 4. Mechanism understood, not mysterious: emptying the
  literal lane's site set hands the case to the pair fallback
  (`next_edit::predict_filtered`), whose rule can be wrong. Per-hunk the
  trade is also worst on TS — 6.75 junk removed per good hunk lost,
  against 11.5:1 on Go and 9.8:1 on Rust.
- **Value of on, where it is on:** main bank hunk-precision
  33.9% → 38.6%, wrong-fire 12.8% → 12.6%; 441 junk hunks removed per
  45 good (9.8:1). The React/TS bank is bit-for-bit unchanged, which is
  the whitelist doing its job.
- **Flip condition (per language, not as a set):** on a bank of ≥150
  positives in that language, adding the id must (a) raise
  `hunk-precision` by ≥5 points, (b) not raise `wrong-fire`, and (c)
  not cost more than 2 points of `useful-fire`. TypeScript today fails
  (b) and (c) outright.
- **Settled by:** the pair-fallback interaction is the thing to fix
  first — if a filtered-empty site set stopped falling through to the
  pair kinds, the TS wrong-fire rise likely disappears and TS becomes
  re-measurable. That is a code change, not a threshold sweep.
- **Note:** `e8ecaef7` (frontier + per-language trade), `de3003cc`
  (first-user languages), `e0d16d45` (what syntax cannot fix).
- **Review by:** 2026-09-06. **First users are Go + React TS**, so a
  capability that is dark on half their codebase is not a quiet row —
  raise it.

### Next-edit fallback onto the resident chat model — `SOVEREIGN_NEXT_EDIT_FALLBACK` (off)
- **Shipped:** 2026-08-07, dark —
  `EmbeddedLlamaCpp::install_fallback_next_edit_slot`, armed from
  `daemon_cmd/build/inference.rs` only when the env var is `1`/`true`
  AND no `[models.edit]` is configured. An explicit `[models.edit]`
  always wins; the fallback never overwrites an existing arrangement.
- **What it does:** serves the next-edit lane
  (`POST /v1/edit_predictions`) off the already-resident fast slot
  (`ModelsSection::fast_path()` — explicit `[models].fast` when set,
  primary otherwise) for users who configured no editing model at all.
  Marks the slot `degraded: true`, which drives the one-sentence
  `advice` nudge on `/status.inference.edit`. Zero extra GB, zero
  download, and no editing keystroke can trigger a model load because
  those weights are resident either way.
- **Why it is plausible:** the two-lane split (`EditSlotInfo`) made it
  *possible* — next-edit needs only a prompt dialect, not FIM marker
  tokens. Measured 2026-08-07 with the consult gate forced open: the
  35B-A3B chat primary on `region_instruct` with thinking off scored
  **21/30 useful, 0 wrong edits, p95 2576 ms**, against the 1.5B
  next-edit specialist's **19/30, 0 wrong, p95 828 ms**. A 2-case
  spread at n=30 is inside the noise, so on a *primary-class* model
  the specialist's real win is latency, not correctness. For the user
  with no edit model the alternative is not a worse suggestion — it is
  no feature.
- **The number that actually governs this flag, and it did NOT hold
  (2026-08-07).** The fallback serves off `fast_path()`, so on any box
  with an explicit `[models].fast` the answering model is the FAST
  slot, not the primary. Run end to end through the **production
  daemon** (not a standalone llama-server, unlike the arms above) with
  the flag armed, `[models].fast = Qwopus3.5-4B-v3-MTP-Q8_0`, same
  60-case bank, same forced gate:

  | gate | fast slot (4B) | 35B primary | sweep-1.5b |
  |---|---|---|---|
  | GM4 usefulness | **FAIL 14/30** | PASS 21/30 | PASS 19/30 |
  | GM3 wrong-edit | PASS 0/17 fires | PASS 0/25 | PASS 0/26 |
  | GM5 p95 | PASS 2194 ms | 2576 ms | 828 ms |
  | GM1 malformed | PASS 0 | PASS 0 | PASS 0 |

  `next_edit_gen_eval.py`'s own verdict line: **`stay opt-in`**. So the
  21/30 does **not** transfer down a model class, and the fallback is
  not meaningfully faster either (2194 vs 2576 ms — a 4B on this lane
  is no cheaper than a 35B-A3B MoE, because ~3B active is the same
  decode cost). It stays safe (0 wrong edits), which is why the honest
  posture is opt-in rather than removed.
- **Cost of off:** every user without a `[models.edit]` section gets
  no next-edit model lane. Unquantified: nobody has counted how many
  installs that is. The rule lane is unaffected either way.
- **Why not default-on already:** the measurement above is **one run
  of one model on one bank**, and it is the wrong bank for this
  question — `gym/next-edit/gen/` is 60 hand-curated generalization
  cases with the gate forced open, not the 1,098-case golden set the
  shipped model lane is actually gated on (ARCH §18.4/§18.5). Turning
  this on also silently changes which model answers on machines whose
  primary is arbitrary; the p95 of 2576 ms is already 1.4x the
  shipped lane's 1748 ms, and a slower or thinking-locked primary
  would be worse.
- **Flip condition:** the fast-slot fallback scores, on
  `gym/next-edit/golden/` (1,098 cases) via `examples/next_edit_score`
  against the operator's resident primary, (a) wrong-fire **no higher**
  than the shipped model lane's 15.2%, and (b) p95 **≤6 s** (the GM5
  bar). Quality parity is NOT required — "better than nothing" is the
  claim, and the useful-fire number only has to beat rule-lane-only.
- **Settled by:** a golden-set arm in the next-edit bakeoff
  (`sovereign/bench/next-edit-bakeoff/arms.toml`, the Phase 1
  `chat-primary-moe-*` arms) — those arms exist and have run on the
  gen bank; the golden set is what is missing.
- **Known risk, now CLEARED for the daemon path (2026-08-07).**
  Thinking suppression is load-bearing, not a tuning knob: the same
  model with reasoning ON scored **0/30**, emitting ~1044 tokens of
  `reasoning_content` before its first answer byte against this lane's
  64–1024 grant, so every case truncated. That risk was that the
  daemon's transport might not actually suppress. It does: the 60-case
  run above produced **zero `truncated` drops** (17 noop, 6 invalid, 20
  inconsistent, 17 fired), and a direct probe on the same slot returned
  `content='READY'` in 526 ms suppressed versus reasoning prose leaking
  into `content` at 1114 ms unsuppressed. `ConsultPlan::suppress_thinking`
  → `chat_template_kwargs.enable_thinking=false` + `think_budget=0` is
  exercised end to end, not assumed.

  What remains unproven is the same claim on a model whose template
  ignores both transports — the fallback targets whatever the user has.
- **Blocking issue for the flip:** quality on the model the flag
  actually routes to. 14/30 is below the shipped rule lane's bar for a
  default-on claim; the flip needs either a better fallback target
  (e.g. prefer a coder-class fast slot) or acceptance that "better than
  nothing" is worth 47% usefulness. That is a product call, not a
  measurement gap — the measurement now exists.
- **Review by:** 2026-09-07.

### EvidenceCheck frame + evidence-shape early-decline
- **Shipped:** 2026-07-21, dark.
- **Proof so far:** top_cosine established as TOPIC signal, not
  answer-containment (~0.75 in-topic-but-thin) — the floor needs
  calibration before the frame can gate anything.
- **Flip condition:** floor calibration soak separates
  "in-topic-thin" from "answerable" without raising false declines.
- **Settled by:** unowned — no current T1 item covers it. If no
  tranche claims it by review date, kill or re-scope.
- **Review by:** 2026-08-14.

### Cross-encoder reranker slot
- **Shipped:** dark (note `10a1b08d`). **Wired into the daemon-server
  and desktop Runtimes 2026-08-03** (T1 A2) — until then the `svrn
  chat` CLI was the only surface that installed one, so both shipping
  surfaces ran baseline fusion and `SOVEREIGN_PPR_EXPAND` logged "lane
  dark" for want of the same `rerank_fn`. Still opt-in via
  `SOVEREIGN_RERANK_MODEL_PATH`; the row stays DARK until the A/B.
- **Cost of on:** the ~500MB / ~1.7s-per-query figures below are
  SUPERSEDED and were measured on the broken jina GGUF. SP4
  (2026-07-31, note `d43fb03b`) adopted the official
  `Qwen3-Reranker-0.6B-Q8_0` GGUF: 639MB, **22.7ms/pair batched**
  (~470ms for top-20), 2.57ms/pair on short titles. The
  `jina-reranker-v3-Q8_0.gguf` finding that read as "rerankers are
  unusable" was a conversion defect in that one artifact — it dropped
  the scoring head — not a property of the capability.
- **Superseded cost figures:** ~500MB resident, ~1.7s/query at k=50,
  OICP wire work for peer routing.
- **Flip condition:** residual contribution (+1 SEP source, +5 wiki
  sources, +12 wiki facts) survives after cap-N chunks-per-article +
  vector-distance dedup are measured *combined* — the cheap fixes
  must fail to close the gap before the expensive slot earns it.
- **SETTLED 2026-08-04 — the flip condition PASSED on quality and the
  slot was REJECTED ANYWAY, on latency.** See the REJECTED section
  below; this row is kept here only so the flip condition and its
  answer sit together. Notes `6a957b47`, `f4150097`.
- **Review by:** closed.

### Hardened `sovereign-server` — `dev-routes` + `net-tools` (both default **ON**)
- **Shipped:** `dev-routes` 2026-08-02, `net-tools` 2026-08-03. Both
  default ON. The *hardened* build is the opt-in one:
  `cargo build -p sovereign-server --no-default-features`.
- **What is dark:** not a capability — a *posture*. Default-on keeps
  every existing build (desktop, mobile host, dev workstation)
  byte-identical, so the row records why the safe configuration is the
  one you have to ask for.
- **`dev-routes` gates PRIVILEGE:** `POST /v1/solve` + `/v1/cycle/bdd`
  (client-supplied `test_command` reaches `sh -c` inside the
  *authenticated* router — any tenant key is a shell);
  `POST /v1/documents/upload` + `/v1/corpora/upload` (ingest an
  absolute server-side path — any tenant can read any file the process
  can, including the config holding every other tenant's key);
  `/mcp`, `/mcp/message`, `/mcp/stats` (outside the auth layer, gated
  only by `ip.is_loopback()`, which a same-host reverse proxy
  satisfies for every remote caller); `ShellTool`.
- **`net-tools` gates EGRESS**, and it exists because an audit found
  three agent tools reaching the open internet on **ordinary chat
  turns** with no config key, no env var, and no removal by
  `--no-default-features`: the `search` tool's web fallback
  (DuckDuckGo → Google → DuckDuckGo Lite, fired whenever the top LOCAL
  retrieval score is thin), `web_fetch` (any URL the model emits,
  scheme-only validation, 5 redirects), and `wikipedia_fetch`. They sit
  three lines below `ShellTool`, which *is* gated. `Permission::Network`
  is not a control: it is consulted at exactly one call site, in the
  plan executor, and the chat path calls `tool.execute()` directly.
  Under `--no-default-features`, `search` survives built local-only.
- **Why they are two flags, not one:** privilege and egress are
  unrelated decisions. One flag for both would make neither name true
  (§10.6, one decider one name).
- **Proof so far:** both configurations compile clean; under
  `--no-default-features` the dead-code count drops 47 → 2, confirming
  the modules are excluded from the binary rather than merely
  unreachable. `acceptance.sh` check 0c enumerates `GET /v1/tools` on
  the running box and fails if either egress tool is present *or* if
  `search` went missing with them.
- **Flip condition (falsifiable):** `dev-routes` is **deleted and the
  hardened surface becomes unconditional** once all four are true —
  (a) upload routes path-jail to a per-tenant root instead of taking
  an absolute server path; (b) `/mcp` moves inside the auth layer and
  stops trusting peer address; (c) `test_command` is an allowlist, not
  free text; (d) `ShellTool` registration is gated on an explicit
  config key. `net-tools` flips only when the three tools gain a
  runtime allowlist that the chat path actually consults — a cargo
  feature is the wrong granularity for a product capability, and is
  here only because no runtime control exists.
- **Settled by:** the on-prem pilot
  (`sovereign/deploy/onprem/PLAN.md`). If the pilot does not proceed,
  the items above are the standing debt regardless — this crate is
  reachable from the desktop's embedded host too.
- **Review by:** 2026-09-15.

### Headless OCR in the daemon — the `ocr` cargo feature (default **OFF**)
- **Shipped:** 2026-08-03, off by default
  (`sovereign-cli-daemon/Cargo.toml`, `ocr = ["sovereign-tools/paddle-ocr"]`).
- **What is dark:** the daemon can install an `OcrCtx` at boot so
  `svrn corpus watch --ocr` reads scanned PDFs headlessly. Without the
  feature, a scanned PDF lands in `WatchedFolderState.failed_files`
  with reason `scanned_no_text` — reported, not silent, but the
  document does not enter the index.
- **Cost of on:** pulls `ort` + `ndarray` + `imageproc` + `i_overlay`
  into every daemon build, and the runtime needs ~20 MB of staged
  assets (`det.onnx` + `rec.onnx` + `dict.txt` = 12.6 MB, `libpdfium`
  = 7.6 MB) that a default install does not fetch. Off-by-default
  keeps dev builds and the standard release set unchanged;
  `sovereign/deploy/onprem/package.sh` turns it on.
- **Flip condition (falsifiable):** default-on when (a) the added
  clean-build wall time for `-p sovereign-cli-daemon` is measured at
  under 60 s, **and** (b) the OCR assets ship in the standard release
  artifact so the feature is not compiled-in-but-unusable — a build
  that has the code and no models fails `build_engine` at ingest,
  which is worse than not having it.
- **Settled by:** the on-prem pilot's `package.sh`; the general
  release path (`scripts/release-cli-local.sh`) does not stage OCR
  assets today.
- **Review by:** 2026-09-15.

### Comaintainer director (M0 supervised) + review seat — script-invoked, no env flag
- **Shipped:** 2026-08-06, M0. The role is `gym/comaintainer/CHARTER.md`;
  the seat is `scripts/co-review.sh` (advisory: exit 0 always, no hook,
  no gate, verdicts append to `~/.sovereign/comaintainer/verdicts.jsonl`);
  supervision records land via `scripts/co-directive-log.sh`
  (`--stats` = the per-kind edit rate). Vision `docs/COMAINTAINER.md`.
  Since 2026-08-06 the seat also runs unattended: `scripts/co-sweep.sh`
  (launchd, nightly 03:30, this host) shadow-reviews each new commit —
  still advisory, verdicts to the same log; and a warn-only pre-commit
  hook (`scripts/pre-commit.sh`) surfaces peer work-atlas collisions.
  Artifact 4 (the work order, `scripts/co-order.sh` +
  `.sovereign/features/<id>/order.md` + boot-hook index) landed
  2026-08-06: opt-in per session, advisory check, gitignored per-host
  files — journey scenes 1–3 now have their carrier. The director
  seat is `/comaintainer` (`.claude/skills/comaintainer`): the
  operator's primary interface — briefs, intakes orders, spawns
  workers on approval (cap 3), oversees glassbox-style; M0 supervision
  unchanged (every directive drafted for operator approve/edit).
- **What it does:** a trained, measured role between operator and agent
  pool. At M0 every directive it drafts (order/steer/review/briefing)
  passes an operator approve/edit before reaching a worker; the
  (draft, final) delta is the disengagement signal, self-driving style.
- **What is dark:** any autonomy. M0 supervision is charter-enforced
  (remembered, not structural) — acceptable only because the operator
  is in the loop by construction; from M1 on, sends must route through
  the helper with an explicit per-kind operator-ack flag (§7).
- **Proof so far:** the gym (`gym/comaintainer/`, 301 episodes, tier-A
  holdout 72): noise floor exactly 0/90, baseline 36.1%, charter v4
  56.9% (+20.8pt, McNemar p=0.0015, basis-exists 93.2%) — numbers in
  `gym/comaintainer/README.md` §Results. M0 exercise completed
  2026-08-06: 5 supervised directives (order/steer/review/briefing),
  overall edit rate 60% at n=5, first real operator edit captured
  (agent-family-agnostic scheduling), operator audit of the bank
  passed.
- **Flip condition (M1, per directive kind):** over the trailing ≥30
  directives of that kind, the operator edit rate is at or below a
  threshold SET FROM M0 DATA (never invented — §18.4), AND the charter
  meets its predeclared gym margin on the tier-A holdout.
- **Settled by:** `~/.sovereign/comaintainer/directives.jsonl`
  (`co-directive-log.sh --stats`) + the gym.
- **Review by:** 2026-09-06.

### Landing field-diff — `co-review.sh --field` (opt-in flag, no env var)

- **Shipped:** 2026-08-07, opt-in, same commit as this row
  (docs/FIELD_VERDICTS.md Scene 2).
- **What it does:** at a landing review, runs one degraded scratch
  render (`fieldglass --no-dup --out <scratch>`; the default delta
  baseline is structurally untouched) and diffs the changed files' rows
  against the standing sidecar — growth/offender transitions, new
  violation edges, SCIP freshness as a mechanical could-not-judge. The
  `field_evidence` object lands in the bundle and the verdict record; a
  headline finding auto-mints a tier-A episode skeleton to
  `~/.sovereign/comaintainer/field-episodes.jsonl` (unaudited; manual
  promotion).
- **What is dark:** the flag itself — no seat runs it unless invoked.
- **Proof so far:** watched-fail chain 2026-08-07: pre-change binary
  confirmed writing no scratch JSON; post-change writes it while both
  baseline-preservation paths hold; growth diff verified on `b0edbe15`
  (13 real rows); synthetic offender transition minted exactly one
  skeleton; stale-SCIP path emits could-not-judge, never zero-delta.
- **Flip condition (default-on in the seat's landing step):** across 5
  real landing reviews, the field pass completes, adds under 90 s
  wall-clock, and its evidence appears in the drafted verdict at least
  twice. Rejected if cost or noise makes seats skip it — recorded, not
  argued with.
- **Settled by:** `~/.sovereign/comaintainer/verdicts.jsonl`
  (`field_evidence` present + `field:` anchors in basis) against the
  seat's stewardship notes.
- **Review by:** 2026-08-21.

### Per-commit architecture audit — `CO_ARCH` in `co-sweep.sh` → **GRADUATED 2026-08-17, default ON**

- **Graduated same day it shipped, by operator direction, on an AMENDED
  bar — not on the bar it was registered against.** The candidate missed
  bar (c) as written (2.5s/commit); the operator re-anchored the bar to
  the house tolerance for a quality check ("running tests is about the
  anchor... realistically it takes 10 mins") and directed the flip. At the
  shipped config the audit costs ~19s per fired commit and ~2.8 min/night
  at the sweep's 20-commit cap — inside a lint run per commit, and well
  inside the ceiling for the night. The seat proposed the amendment and
  did not make it: a bar moved by the seat after seeing the data it failed
  is not a bar.
- **Standing exit condition (operator's words):** "we can modify if it
  hurts ergonomics too much." That is what review-by asks.
- **Config shipped:** `window = 8`, `max_sites = 16` in
  `quality/arch-probes.toml`, chosen from a 4-point sweep on a frozen
  12-commit set. It is the knee: identical verdicts to the full diff at
  40% of the cost, and the two cheaper configs judge strictly worse
  (could-not-judge 0.38 vs 0.25).
- **Residual, named:** 25% of rule-verdicts are could-not-judge at every
  config tested. They render as an explicit `C` line in the rollup — the
  seat sees "not judged", never "clean".
- **Shipped:** 2026-08-17, same commit as this row.
  `scripts/co-arch.py` + bars at
  `gym/comaintainer/PREREG_arch_probes_20260817.md`.
- **What it does:** per swept commit, judges the added code against the
  §15 smell rows that code cannot enforce, in ONE batched forced-choice
  call (A/B/C per rule, ~3 chars per rule). A model-free gate decides
  which rules can fire and SUPPLIES THE CITATIONS, so no model authors a
  character of the row; §2.1 is decided by an arm counter and never
  reaches the model (§7.6). Rows land as `kind:"arch"`, `shadow:true` in
  `~/.sovereign/comaintainer/verdicts.jsonl`; the seat reads
  `co-arch.py --rollup`.
- **Quality bars, all MET on the 27B and re-run at the shipped config:**
  gate recall 21/21, catch 0.952 on planted violations, **false-B 0.000
  across 13 hard negatives** (clean code that trips the gate), bit-stable
  0/39 across repeats. The 4B is DISQUALIFIED and may carry no rule:
  catch 0.667.
- **MEASURED AND REFUSED, 2026-08-17 (same day):** bars ran on a restored
  daemon. Gate recall 21/21 MET; catch 0.952 on the 27B MET; false-B
  0.000 MET; bit-stability 0/39 MET. **Bar (c) cost MISSED on both
  engines** — 27B median 5,398ms per fired commit (kill tripped at
  ≥4,000ms), 4B median 2,509ms against a 2,500ms bar — and the 4B is
  separately disqualified on catch (0.667). So it stays OFF.
- **Why cost missed:** the shape is right and decode is genuinely free
  (5-12 output tokens per commit); the price is PREFILL, measured at
  ~7-8ms per prompt token. The registration's ~1.2s projection was
  borrowed from a batched register whose speed came from a shared cached
  prefix, which a per-commit bundle does not have. Full-bundle candidate:
  46.3s/commit, well-evidenced. Gate-localised windows: 5.4s/commit but
  could-not-judge on 6 of 12 real commits — speed bought by removing
  evidence. Both refused; data in the prereg's RESULTS section.
- **Open question for the operator (do NOT let the seat self-resolve):**
  bar (c)'s 2.5s came from an interactive register; this is a nightly
  batch where the full-bundle candidate costs ~34 min/night and the
  windowed one ~4 min. Amending the bar to total sweep wall-clock is
  defensible, but a bar moved after seeing the data it failed is not a
  bar — so it is the operator's call, not the seat's.
- **Flip condition (unchanged):** every bar in the prereg met, including
  whatever bar (c) becomes if the operator amends it, PLUS the standing
  reporting duty added 2026-08-17 — the real-commit could-not-judge rate
  reported beside the bank score, because the bank passed while
  production returned all-C on half the commits.
- **Settled by:** `~/.sovereign/comaintainer/verdicts.jsonl`
  (`kind:"arch"`) against the bank's labels;
  `gym/comaintainer/score_arch.py` is the instrument.
- **Review by:** 2026-08-24.

## OWED A ROW — dark capabilities with no flip condition (audit 2026-08-05)

**How this section came about.** Cross-referencing
`quality/env-flags.toml` against this file found **31 retrieval flags, 12
default-off or `status = experiment`, and only 2 with a ledger row**. The
contract in the preamble says a dark ship adds a row in the same commit; these
predate the contract, so nobody broke it — but they are exactly the withering
this file exists to stop, and they were invisible until someone counted.

Stripping out what is not ledger material — `SOVEREIGN_FORENSIC` (debug),
`SOVEREIGN_COMPACTION_DISABLE` (escape hatch), and `ATOM_ENUM_RANK` / `_POOL` /
`DECOMP_DECAY` (tuning params, not on/off capabilities) — **six genuine dark
capabilities are owed a row**. They are listed here rather than given
fabricated flip conditions: a row whose "proof so far" was invented is worse
than no row, because it reads as settled.

Each needs one measurement before it can graduate to a real row. The instrument
already exists and is deterministic (~9 min per arm):
`svrn bench all --bench-root sovereign/bench --filter <bank> --prod-pipeline`.

| flag | capability | measurement owed |
|---|---|---|
| `SOVEREIGN_ATOM_ENUM` | entity-typed atom enumeration for enumeration-class questions | an A/B on a bank with enumeration questions ("which X were involved") — the Enron counterparty case its doc comment cites |
| `SOVEREIGN_ATOM_ENUM_RELATIONS` | relation atoms in the same path | same bank, as a second arm on top of `ATOM_ENUM=1` |
| `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND` | wikipedia graph-neighbour expansion | A/B on a wikipedia bank; watch corpus-mix drift, not just recall |
| `SOVEREIGN_META_BRIDGE` | meta-atlas bridge boost | A/B on a multi-corpus bank |
| `SOVEREIGN_QUERY_DECOMP` | question decomposition + fan-out | A/B on a multi-hop bank; cost is extra retrieval round-trips |
| `SOVEREIGN_TITLE_EXPAND` | LLM question→title expansion | A/B on any retrieval bank; cost is one LLM call per turn |

**One live inconsistency found in the same audit, and it is not cosmetic.**
`SOVEREIGN_ATOM_ENUM` is default-**off** with `status = experiment`, while
`SOVEREIGN_ATOM_ENUM_OVERVIEW` — a sibling path in the same module, reached
through the same `enumerate_typed_atom_chunks` entry point — is default-**on**
and runs in production on every overview-shaped question. That is how audit D1
happened: a production path whose parent feature is nominally an experiment,
so nobody was measuring it. Either the overview path is a shipped capability
(then `ATOM_ENUM`'s `experiment` status is wrong and its own row is overdue) or
it is an experiment (then it should not be default-on). Resolve it with the
`SOVEREIGN_ATOM_ENUM` measurement above.

- **Review by:** 2026-09-05 — the whole section. If a flag has no measurement
  by then, the right move is to DELETE the capability, not re-date it. Six
  unmeasured flags is a labyrinth; six measured ones is a feature set.

## REJECTED — measured no; do not re-litigate without new evidence

### Native grounding, H1 admission **as a gate** — rejected as calibrated, and the gate is deleted
> **Read the scope of this row carefully.** What is rejected here is
> *deciding* on H1's answerability. The `SOVEREIGN_NATIVE_GROUNDING`
> knob itself is no longer off — it was promoted to **default ON for
> DISPLAY** on 2026-08-11; see the GRADUATED row below. Nothing in this
> row was re-litigated to get there, because display and gating are
> different questions and the gate no longer has a switch to flip.
- **History:** shipped dark 2026-08-09 (`bb48e8c6`) with the flip
  condition "both HARD A/B bars clear with no in-curve parameter
  change". The A/B ran the same day. **The condition was refuted, so
  this row moved here rather than sitting dark waiting.** The row is
  doing exactly what the ledger exists for: the answer came back in
  hours, not ten weeks.
- **The numbers** (`sovereign/bench/calibration/ab/`, saltgrass dev
  bank, both arms carrying the reranker so only the flag differs):

  | bar | flag OFF | flag ON r1 | flag ON r2 | bar | verdict |
  |---|---|---|---|---|---|
  | honesty-when-absent | 0.91 | 0.91 | 0.91 | ≥ 0.91 | PASS, delta **0.00** |
  | competence-when-present | **0.74** | **0.26** | **0.23** | ≥ 0.80 / 0.71 | **FAIL, −0.48** |

- **Why, in one sentence:** H1 abstained on 31 of 33 turns because the
  saltgrass median rerank margin is 4.49 against a threshold of 5.885
  fitted on SEP + brothers-karamazov — and at that scale the
  calibration corpus shows a 0.98% false-alarm rate where saltgrass
  shows 50%, a ~50x shift.
- **It bought nothing.** This is not a trade. The incumbent already
  caught 10 of 11 absent probes, so the headroom was one probe and H1
  captured none of it, while turning 15 of 23 correct answers into
  refusals.
- **No in-curve recovery**, for two independent reasons: the threshold
  that would restore competence is priced on a margin scale that
  demonstrably does not transfer, and the honesty it promises is not
  there to buy at any threshold. Thresholds were NOT re-fitted on the
  bank under test.
- **This confirms a registered risk**, not a surprise:
  `NATIVE_GROUNDING.md` §10's first named risk is the reranker head
  failing to transfer across corpora.
- **Rejected as CALIBRATED, not as a mechanism.** The code is sound and
  stays (deletion was explicitly out of scope for this order). What is
  rejected is this calibration shipped as a global default. Do not
  re-litigate a flip on the current artifacts. Step 3 owns the real
  decision with the number now in hand: per-corpus calibration of
  `tau_abstain`, the §7.3 fallback (train the 4B head via the
  verifier-v0 pipeline), or dropping answerability routing as not worth
  its transfer cost.
- **Companion measurement, same order:** certified-claims-skip-judge
  was also refused — resolver precision 0.7429 against a pre-pinned
  0.98 bar (`sovereign/bench/calibration/resolver-precision/`). Per-claim
  verification is untouched.
- **2026-08-10 — what the flag MEANS changed; the default did not.**
  Order `native-grounding-p1-desktop` executed the parity plan's P1
  composition (`sovereign/docs/specs/NATIVE_GROUNDING_PARITY_PLAN.md`
  §4.1). Admission-as-gate — the thing rejected above — is now gone from
  the code, not merely defaulted off: the native decline arm in
  `handlers/knowledge_query.rs` was **deleted**, and so was the typed
  shortcut in `grounding::abstention_action` that let a verdict change a
  turn's gate action. What `SOVEREIGN_NATIVE_GROUNDING=1` turns on is
  **display**: typed answer segments on the wire and in the desktop
  bubble, plus H1's answerability recorded as telemetry with
  `enforced=false` on every event. The withhold decision is the
  incumbent cosine floor on **both** arms.
- **Therefore this REJECTED row is about a mechanism that no longer has
  a switch.** Do not read it as "the flag is rejected" — read it as
  "gating on this calibration is rejected, and the gate is deleted."
  The flag's own default was held **OFF** pending P1's bars (§4.1:
  competence ≥ 0.74, honesty ≥ 0.91 both on-runs, no HARD lane
  regression, every Grounded badge resolves, zero disclaimer-
  confabulations) — promotion an operator call on those numbers.
- **Settled 2026-08-11: the operator made that call and the default is
  now ON.** This row is closed as a gating question; the display
  promotion and its evidence live in the GRADUATED row below. Re-opening
  *gating* still needs a new signal, not a new threshold.

### Run-if-stale launchd triggers — rejected in favor of the seat ritual
- **History:** shipped dark 2026-08-07 with `scripts/run-if-stale.sh`
  (`--write-plists` wrote two LaunchAgents and deliberately never
  called `launchctl`; the flip condition was the operator loading
  them). The operator resolved it 2026-08-08 — an operator product
  decision, not a measurement: **no launchd**. The plists are deleted;
  `launchctl bootstrap` is a dead ask, do not re-raise it.
- **What survives:** `scripts/run-if-stale.sh` itself, run DETACHED
  (nohup + disown, note `b25059e3`) for both lanes as part of the
  comaintainer seat's close-up-shop ritual — the staleness guard fires
  on "closing the shop," not on login. `svrn posture`'s ByHandOnly
  wording already states this honestly (commit `c9224da6`).
- **Why:** a login-time job in the operator's GUI session is exactly
  the invisible mechanism the guard was built to remove; the seat
  ritual keeps the same coverage with a human in the loop.
- **Re-open only if:** seatless stretches (no close-up-shop for >1
  week) let a contract FAIL sit unread again — the failure mode that
  motivated the guard (2026-08-03, three days unread).
- **2026-08-13 — what is rejected is the LOGIN AGENT, and it stays
  rejected.** Order `seat-handoff-hardening` added
  `run-if-stale.sh --write-oneshot <lane>`: a transient plist under
  `~/.svrnmesh/run-if-stale/`, never in `~/Library/LaunchAgents`, so
  bootstrapping it arms exactly one run and nothing survives logout.
  That is the seat's existing "longer than a harness task → launchd
  one-shot" tier given a file you can read, not a new cadence — it
  replaces `launchctl submit`, which is now banned repo-wide for
  carrying implicit keepalive with no plist to find. This mode does
  not re-raise `launchctl bootstrap` of the login agents; that ask
  remains dead.

### GLiNER2 as the vault/conversation extractor — `SOVEREIGN_GLINER_MODEL_ID` (stays `gliner_small-v2.1`)
- **Shipped and settled the same day, 2026-08-03.** The row was written
  in the morning against a flip condition — "holds the vault bar at a
  lower time-to-enriched AND no per-label typing regression" — and the
  afternoon's run refuted **both halves**. Recording it rather than
  deleting it, because the seam it rode in on is staying.
- **Verdict, on all 3,175 obsidian vault chunks**, both backends through
  the production `LabeledEntityExtractor` seam
  (`sovereign-gliner/examples/typing_audit.rs`, artifact
  `research/enrichment-spikes/findings/typing_audit_obsidian.json`):
  - **Time: 881.9 s v1 vs 893.2 s GLiNER2 — no speedup, marginally
    slower.** The 2.52× was real but is a property of the chunk-length
    distribution, not the model (sep p50 761 chars; vault p50 1,808).
    v1's gline-rs stack batches 8 texts per call and amortises; GLiNER2
    is one graph call per text. Note `dc2e4b5d`.
  - **Typing: worse, not fixed.** Mention-level Person accuracy 96.9%
    (v1) vs 81.8% (GLiNER2) on the vault oracle; 99.7% vs 67.3% on sep.
    `Ostrom` — the vault's anchor entity — is `Person` ×6 /
    `Organization` ×6 under GLiNER2. `Work` becomes a catch-all for
    ordinary noun phrases: 16,053 `Work` mentions to v1's 632, 47% of
    its entire output. Note `f42cf7ec`.
- **What is NOT rejected:** the residency finding (GLiNER2 is ~4.8×
  lighter, note `3f47d12e`) and the seam itself. The knob stays — it is
  how anyone re-tests this — and P2.1's steps (b)–(d) were never
  evaluated.
- **Re-open only if:** a GLiNER2 checkpoint or label/threshold
  configuration demonstrably stops `Work` absorbing common noun phrases,
  scored **per mention** on `bench/gliner/` oracles; or a target corpus
  with sep-shaped chunk lengths makes the throughput win real AND typing
  holds. Both halves, not either.

### Cross-encoder reranker slot — `SOVEREIGN_RERANK_MODEL_PATH` (stays unset)
- **Verdict:** 2026-08-04. **The flip condition PASSED and the slot is
  still rejected** — it was a quality condition, and quality was never
  the binding constraint. Rejected on TTFT plus a fourth resident model
  slot. Notes `6a957b47`, `f4150097`; artifacts
  `target/overnight/20260803-225051/block1/`.
- **The condition, answered:** 180-question paired bank on
  `conversations-anthropic` via `eval run --prod-pipeline`. The cheap
  fix measured alone (`dedup-only`, per-article dedup, no model) moved
  the number a lot and still LOST to the cross-encoder 42–89
  (p=0.0000). Gap not closed ⇒ by the letter of the condition, earned.

  | arm | mean RR | both@10 | src ratio | **search p50** |
  |---|---|---|---|---|
  | baseline | 0.2631 | 26.7% | 0.744 | **557 ms** |
  | dedup-only | 0.3362 | 50.6% | 0.856 | **1,240 ms** |
  | reranker | 0.3968 | 75.6% | 0.903 | **4,566 ms** |

- **What killed it:** corpus search runs SYNCHRONOUSLY inside the turn,
  so retrieval latency lands on TTFT. The median turn goes 0.56 s →
  4.6 s **before the model emits a token**. The reranker's margin over
  free dedup is +18% mean RR / +25pp both@10 for **+2.8 s of TTFT** —
  and it needs a 4th resident slot on a daemon already at ~29 GB
  (35B + 2B + embed + a 7.85M-edge wiki graph). `RERANK_EXPERIMENT.md`
  §"Resident-weight cost" predicted exactly this in May.
- **And it is fragile, not merely slow:** the same arm cost 4.3 s/query
  on a quiet box and **>280 s/query** the next day under memory
  pressure (~5 GB free, compressor holding ~5.4 GB of RAM). A ~60×
  degradation with headroom is not a knob you ship behind a default.
- **What shipped instead:** `[retrieval] dedup_by_source = true` on
  `conversations-anthropic` (measured) and `conversations-chatgpt`
  (same shape, inferred — labelled as such in the recipe). ~60% of the
  quality gain for ~20% of the latency, no model, no slot, no VRAM.
  This is `RERANK_EXPERIMENT.md`'s own pre-registered call — "the big
  win is the dedup… don't add the slot, add the diversifier" — decided
  by the arm that doc asked for.
- **NOT rejected:** per-article dedup itself, and the reranker as an
  OFFLINE/batch tool where TTFT is irrelevant (bench scoring, index
  build). The rejection is specifically *a resident slot on the
  interactive path*.
- **Re-open only if:** retrieval moves off the critical path (streamed
  or speculative retrieval), OR a rerank pass lands somewhere TTFT
  cannot see it, OR an `x:rerank` peer capability serves it from a node
  with headroom — the OICP route `RERANK_EXPERIMENT.md` §"Mesh contract
  surface" sketched. Not on a faster GGUF alone: 610 MB was never the
  problem, the 4th slot and the synchronous path were.
- **Code NOT deleted, deliberately** — unlike the cluster-score row
  below. The rerank stack has live non-interactive consumers (the bench
  param-loop drives `SOVEREIGN_RERANK_DEDUP_*` via
  `scaffolding_param.rs::RerankSettings::set_env` +
  `promote.rs:389`, and `bench enrichment-ablate --rerank` scores it),
  and the dedup path that DID ship shares that code. Deleting the slot
  would take the diversifier with it. What must not persist is the
  *expectation* that this becomes a default — hence this row.

### Conversation entity PPR — `SOVEREIGN_CONV_PPR_WEIGHT` (0.25 → **0.0**)
- **Verdict:** 2026-08-04. Default flipped OFF. Notes `6a957b47`,
  `f4150097`; artifact
  `target/overnight/20260803-225051/block1/VERDICT-with-ppr0.txt`.
- **Measured, on the corpus where it actually fires:** 180-question
  paired bank on `conversations-anthropic`, `eval run --prod-pipeline`,
  two-sided sign test on reciprocal rank. Alone: 49–31 vs the off arm,
  **p=0.0567**. Under the strongest retrieval config: 64–43,
  **p=0.0527**. Neither reaches p<0.05. The arm was NOT vacuous — it
  changed ordering on 146/180 questions — so this is "measured and did
  not separate", not "never engaged". (An earlier 2026-07 attempt WAS
  vacuous: it ran on SEP, where this path never fires.)
- **Why the ceiling is low, structurally:** it re-ranks in place and
  never adds a document. `B-in-pool` (87.8%) and `source_ratio`
  (0.9028) were **identical to four decimals** with it on and off —
  only the ordering moved. `bench/conversation-bridge/GATE_FINDINGS.md`
  predicted exactly this before the run ("PPR re-ranks in place and
  never adds"), which is also why that doc pre-registered this A/B.
- **Cost of on:** a per-conversation entity graph rebuilt from SQL on
  EVERY query, plus — because it reads `chunk_entities` on the query
  path — it is the sole reason the GLiNER NER pass must complete
  eagerly at ingest before a corpus is fully useful. Turning it off is
  what makes deferred/late NER safe (`PROGRESSIVE_ENRICHMENT.md`).
- **CODE KEPT, NOT DELETED — operator call 2026-08-04.** ~1,325 lines
  (`conv_entity_graph.rs` + `rerank_conv_chunks_via_ppr` + 23 unit
  tests) were sized for removal and deliberately retained: the code is
  correct and tested, the measurement says *marginal*, not *wrong*, and
  a one-line default is cheaper to reverse than a deletion is to
  rebuild. This is a deliberate departure from the cluster-score row
  below, which was deleted — that one had a measured **0.0000** delta;
  this one has a real-but-unprovable effect.
- **What a user loses:** the "bridge" badge on promoted sources
  (`ppr_seed` / `ppr_mass_norm` → `SourceAttribution.svelte`,
  `EpistemicFooter.svelte`) simply never fires. The UI degrades
  silently and correctly; no dead controls.
- **Re-open only if:** a bank shows it separating at p<0.05 — most
  plausibly one built on *cross-conversation* questions where in-pool
  reordering is the whole game, since this bank's own headroom analysis
  showed 66% of target conversations were already in the pool. Set a
  non-zero `SOVEREIGN_CONV_PPR_WEIGHT`; nothing else is needed.

### Cluster-score blend — `SOVEREIGN_DOC_CLUSTER_WEIGHT` (stays 0.0)
- **Verdict:** 2026-07-31, per this row's own settling condition — the
  T1 P0.4 knob matrix (`bench enrichment-ablate`, 3 sep banks × 3
  reps, artifact `sovereign/bench/ablation/2026-07-31-sep-knob-matrix.json`)
  reports the banks CANNOT separate it: Δ = 0.0000 on every bank,
  zero rep spread. In fact NO knob separated — even
  `SOVEREIGN_RAPTOR_GROUNDING=0` moved only −0.0125 on summarize,
  under the 0.02 floor. Dark since 2026-05-22; settled in one night
  once the lane existed.
- **Honest scope note:** the sep banks do not exercise the
  attached-document search path the blend lives in — this is "the
  current banks can't see it", not "the blend does nothing". Both
  readings route the same way:
- **Re-open only if:** P3.1 golden authoring (T2) produces a bank that
  exercises attached-doc retrieval with cluster-structured answers —
  the same routing as the demand-plan rejection.
- **CODE DELETED 2026-08-01.** Both env vars, the blend branch in
  `attached_document_search.rs`, and the now-unreachable
  `blend_by_cluster_score` / `min_max_normalize` helpers with their 10
  tests are gone; the registry entries in `quality/env-flags.toml` are
  replaced by a tombstone pointing here. A Rejected verdict that leaves
  the code running is the withering pattern this ledger exists to stop —
  the verdict and the deletion belong in the same week, not the same
  hypothetical future tranche. Enrichment knob count **12 → 10**, the
  first movement on the `ENRICHMENT_ROADMAP.md:348` complexity ratchet.
  Recovery for the re-open case: `git show <this commit>^` — the
  rationale survives in `sovereign/docs/specs/CLUSTER_SCORE_BLEND.md`.

### Demand-plan fan-out — `SOVEREIGN_DEMAND_PLAN_FANOUT` (off)
- **Verdict:** 2026-07-19 A/B — net-neutral answer quality at 2–3x
  retrieval latency. Flag stays off; `env-flags.toml` records it.
- **Re-open only if:** a bank exists that separates multi-hop recall
  (P3.1 golden-authoring, T2). A flat-recall bank cannot exonerate it.

### Claim-search ladder — `SOVEREIGN_GATE_CLAIM_SEARCH_LADDER` (stays off)
- **What it did:** used a batched triage judge to decide which claims
  skip the per-claim corpus fan-out. Worth wanting: that fan-out is one
  hybrid search per allowed corpus per claim and measured 25% of
  wall-clock on `bench sep/summarize --synth`; the ladder measured
  **−6.8%** turn wall while skipping ~half the fan-out.
- **Verdict:** 2026-08-05 — **it destroys 23% of the rescues.** Over 78
  claims on the `sep-summarize-slowtail` scratch bank (SHADOW=1,
  LADDER=0, so every claim is still searched and the true rescue set is
  observable): 26 real rescues, 41 claims the ladder would skip, and
  **6 of those skipped claims were real rescues.** Batch-vs-calibrated
  agreement is 86% (67/78), and the 14% disagreement lands exactly where
  it costs most. Trading 23% of the anti-fabrication rescue mechanism
  for 6.8% latency is not a trade this system makes.
- **Why the safety argument failed:** it claimed losslessness *by
  construction* — a rescue fails without re-search, so it must have
  stage-1 `vp >= tau` and always reach stage 2. Sound only while stage 1
  is the CALIBRATED per-claim judge. Stage 1 is the batched text A/B, a
  different instrument with different tau semantics, so their agreement
  is empirical. The claim was withdrawn by its author before this run;
  the run measured what the withdrawal predicted.
- **Both known stage-1 instruments now fail.** A calibrated per-claim
  stage 1 measured net-NEGATIVE (+5.0s wall — a restored pinned prefix
  does not make a forced-choice free; note `a4be8afd`). A batched stage 1
  is fast but lossy, above. Any re-open needs a THIRD instrument, not a
  retuned threshold.
- **Re-open only if:** a stage-1 triage exists whose disagreement with
  the calibrated judge is measured at ~0 on skipped claims — the gate is
  `lost_rescue == 0` summed over a bank, from the `claim_search_shadow`
  event. Note `3850a896`-adjacent; instrument lives in
  `grounding/mod.rs`.
- **Kept, not deleted:** the flag and its shadow instrument stay so the
  next attempt inherits the measurement harness rather than rebuilding
  it. The fan-out it targets is still 25% of wall-clock and still worth
  attacking.

### Acquisition gate armed at 0.45
- **Verdict:** 2026-07-20 — `import_conversations` is a top-1
  attractor at that threshold; arming it misroutes.
- **Re-open only if:** the attractor is fixed and the threshold
  recalibrated against the post-fix distribution.

### Speculative decoding (classic draft)
- **Verdict:** 2026-05-12 — net-negative on this hardware; KV-rollback
  hand-port costed at 2–4 days for nothing the llama-server harness
  doesn't provide.
- **Re-open triggers:** recorded in
  `sovereign/docs/archive/SD_EXPERIMENT.md` §closure.

## INTENTIONAL OPT-IN — off is the designed end state, not a debt

### RSS hard limit — `SOVEREIGN_RSS_HARD_LIMIT_MB` (off)
- Self-SIGTERM is only safe under a supervisor that restarts the
  daemon (2026-07-18). Soft-warn is on. This row exists so nobody
  "fixes" the default.

## GRADUATED — the pipeline completing, for the record

### Claim-search ladder — `SOVEREIGN_GATE_CLAIM_SEARCH_LADDER` → **default ON 2026-08-14**
- **Lifespan dark: 2026-08-05 to 2026-08-14** (shipped as an experiment
  with its safety counter pre-built; flipped by operator close decision,
  order `audit-economy` D6).
- **Flip condition, met non-vacuously.** The registered bar was
  bank-level `lost_rescue = 0` from shadow rows. The 21-turn ladder-shadow
  arm (`runs/audit-economy-ladder-shadow/`, 2026-08-14, baseline verdicts,
  D5 corpus pre-flight applied): **lost_rescue 0/160** with **8 REAL
  rescues present in the bank** and the ladder's skip set disjoint from
  all of them — the zero had every chance to be nonzero; `newly_failed`
  0/160 (the dilution-avoidance direction, reported per §18.6); 96/160
  searches skippable, ~-3.5s/turn at healthy search prices.
- **What flipped, precisely.** `claim_search_ladder_enabled()`
  (`grounding/config.rs`) now returns `true` when the knob is unset. The
  knob is the opt-OUT: `=0` (also `false`/`off`, trimmed) disables;
  every other value including unset and unrecognised leaves it ON. The
  batched stage-1 remains TRIAGE ONLY — the released verdict stays the
  calibrated per-claim forced-choice.
- **Reversal condition:** any production `lost_rescue` evidence
  (re-arm `SOVEREIGN_GATE_CLAIM_SEARCH_SHADOW` to collect it) or a
  CONFAB-LEAK NEW>OLD read on the next paired chaos arm reverts by flag
  (`=0`), not by code. **Review-by: 2026-08-28** — if the next chaos arm
  has not run by then, that is the signal, not noise.
- **Settling plan:** the order closed short (bar missed at the composed
  level; mechanism wins banked). The next chaos/composed arm on the
  longform banks reads the ladder's live effect; no dedicated arm owed.

### Native grounding, DISPLAY — `SOVEREIGN_NATIVE_GROUNDING` → **default ON 2026-08-11**
- **Lifespan dark: 2026-08-09 to 2026-08-11.** Shipped dark (`bb48e8c6`),
  re-scoped from gate to display by order `native-grounding-p1-desktop`
  (2026-08-10), promoted to default-on 2026-08-11 by operator directive
  **`7aa64f29`** ("Let's flip it on. I approve the order"), order
  `native-grounding-flip-soak`. Two days dark, not ten weeks — which is
  what this ledger is for.
- **What flipped, precisely.** `native_grounding_enabled()`
  (`native_grounding/admission.rs`) now returns `true` when the knob is
  unset. **The knob is now the opt-OUT.** Off-form:
  `SOVEREIGN_NATIVE_GROUNDING=0` (also `false` / `off`, trimmed,
  case-insensitive). Every other value — including unset, empty, and
  anything unrecognised — leaves it ON, so a typo cannot silently
  disable grounding (ARCH §18.3).
- **The predicate is a mirror, not an inversion**, and that is the
  safety property: every string that turned the path off before the flip
  still turns it off after it. Only the non-instructions (unset, empty,
  unrecognised) changed meaning.
- **What is ON is DISPLAY.** Typed answer segments + provenance strip in
  the desktop bubble, and H1's calibrated answerability recorded as
  telemetry with `enforced=false` on every admission event. **It decides
  nothing** — the withhold decision is the incumbent cosine floor, the
  same on both arms. The gating question is closed separately and stays
  closed (REJECTED row above).
- **Evidence basis for the promotion** (operator's stated grounds):
  P1 landing — display-only composition with zero added model calls
  verified, A1 decision identity, citability 1.0, real-app render
  witnessed — plus the incumbent-competence landing of 2026-08-11 at
  **0.871 on both arbitration runs**.
- **Reversal condition, pre-stated.** The flip is one line and reverses
  to the same line. Flip back OFF same-day if the 2h desktop soak
  (`scripts/desktop-soak.py`, order `native-grounding-flip-soak` D2)
  surfaces **either** a per-turn latency regression past the noise bands
  (`sovereign/docs/RUNBOOK.md` §6) **or** a rendering failure class.
  Sustained free RAM < 2GB during the soak is an abort-and-report, not a
  push-through. The report stands as the evidence either way.
- **Review-by:** the landing verdict of order
  `native-grounding-flip-soak` — the 2h soak's scorecard, latency
  percentiles (p50/p95, never single-turn), display telemetry, and
  memory profile. If that verdict is not recorded here, this row is
  overdue and any session touching grounding should raise it.
- **RAISED OVERDUE 2026-08-14** (seat, on worker D0 of order
  `gate-tombstone-ladder`, note `e1e9e7a3`): the review-by verdict
  (`e2b474da`, merged `e73fc760`) was never recorded here, and the
  promotion evidence "real-app render witnessed" does not hold on
  BeefyMac now — `answer_segments` is NULL on 17/17 live desktop turns
  (2026-08-13/14). Segment production is gated on
  `native_verdict.is_some()` (`streaming.rs:1799`), whose only margin
  sources are reranker-derived (`admission.rs:191-205`) — i.e. this
  DISPLAY row's render depends on the slot the REJECTED row above keeps
  unset. What renders in practice is the claim-level epistemic ledger,
  not the span strip. Reconciliation is a backlog item (recorded
  2026-08-14); until it lands, the display claim is DARK-IN-EFFECT on
  hosts without a margin source.

### `SOVEREIGN_SKIP_MOTIFS` / `vault-report --no-motifs` → **deleted**
- **Lifespan: 2026-08-02 to 2026-08-02.** Shipped dark in the morning
  as an ablation arm; the code it ablated was deleted the same day. The
  knob is gone with it — this row is the record, not a live default.
- **What it proved.** Motif extraction was **22.3m of a 52m03s cold
  vault build — 42.8% of time-to-enriched** (330 notes,
  `~/.svrnmesh/bench-runs/vault-report/1785678945/`), and its output
  table `conv_motifs` had one INSERT, two DELETEs and **no reader
  anywhere in the workspace**. The briefing-signposts claim at
  `conv_tiered_provider.rs:232` traced to `CONV_TIERED_PORT.md:385`,
  which is future tense and was never built for the conv/vault side.
- **The measured result** (three cold builds + `eval run
  --prod-pipeline`, obsidian vault, sweeper paused):

  | config | wall | speedup | facts | sources |
  |---|---|---|---|---|
  | motifs + GLiNER | 52m03s | 1.00x | 58/68 | 8/12 |
  | **motifs off, GLiNER on** | **29m32s** | **1.76x** | **58/68** | **8/12** |
  | motifs off, GLiNER off | 14m15s | 3.65x | 58/68 | 5/12 |

  Motifs-off matched the full build **per question exactly** on facts
  (6,6,5,3,4,6,5,4,3,6,6,4). Run-to-run variance on one build was zero.
- **Resolution: deletion, not a flip.** `build_folder_artifacts` now
  calls `build_raptor_nodes_with_checkpoint`, which has no motif
  concept in its return type — the pass cannot be re-enabled by
  setting anything. `save_conv_motifs` and `ConvMotifRow` are deleted.
  The `conv_motifs` table and its purge DELETE are retained so existing
  databases still shed their legacy rows.
- **Untouched:** the attached-document path keeps its motifs
  (`asset_motifs`, read by `list_asset_motifs` for the doc briefing).
- **Notes:** `3f47d12e` (the result), `e10bf96e` (the no-reader
  census), `de25ebe9` (why the confirmation arm was cancelled),
  `0b8b6cae` (sweeper contamination), `d39af2dc` (the 68/68 correction).

### Extractive summary mode default for memory corpora (T1 P1.1)
- **Flip condition met 2026-07-31, same day it was written:** the
  production seam held parity on the sep banks — both arms rebuilt
  through `enrich raptor` at identical 14-article scope, |B−A| =
  −0.0125 on summarize (band ±0.025), 0.0000 on obscure (band
  ±0.0167), r1–r3 deterministic, rawindex guard 0.0000 both banks
  (`research/enrichment-spikes/runs/prodAB/`).
- **Default flipped:** memory corpora (vault notes, imported
  conversations via `build_folder_tiered_provider`; memory-pool trees
  via `mem_atlas`; the vault-wide theme synthesis) now build
  EXTRACTIVE trees. Attached documents keep abstractive — now
  verifier-gated (T1 P1.2, same push). `enrich raptor` CLI default
  remains abstractive with explicit `--summary-mode`.
- **Registry/env:** no env flag — the default is code-level policy at
  the memory-corpus construction sites, provenance-stamped per node.

### Measured capability probe — `SOVEREIGN_CAPABILITY_PROBE`
- **Default ON 2026-08-10**, opt out with `=0`. Shipped dark for one day
  and **made load-bearing the same day** once the shadow sweep was in:
  `prefix_cache_gate` now follows the measurement, not the arch ladder.
- **What `=0` costs, now that it decides.** Turning the probe off leaves
  every slot `CouldNotJudge`, and the gate falls back to the
  pre-2026-08-10 declared ladder — i.e. **exactly the old behaviour**,
  not a blanket veto. That was chosen deliberately: a flag whose off
  position silently costs a full prefill on every turn is a trap. The
  fallback is reported at `info` on the `capability` target as
  `authority=declared-fallback`, never silently (§18.3). The same path
  serves distributed children, which never probe.
- **Why the flip was safe (measured, note `bca4ae8e`).** Shadow sweep
  over the local zoo — 12 models, 9 architectures, production config —
  found the measurement agreeing with the pre-flip gate on **12/12**, so
  the flip changed no answer on this host. The ladder it displaces was
  **wrong on 4/12** (three dense `qwen35`, plus `nemotron_h_moe` — a
  Mamba2 hybrid whose arch string carries no ssm marker) and
  **load-bearing on 0/12**: no model it vetoed was one libllama's flags
  did not already veto.
- **The asymmetry that makes a measurement acceptable in a safety gate.**
  libllama's `is_recurrent`/`is_hybrid` keep an unconditional veto; the
  probe may only ever ADD one. So a probe that loses sensitivity costs
  prefill time and cannot cost correctness — the inverse of the plan's
  original shape, where a false `Safe` would have cleared a corrupting
  model.
- **Why:** one property — can this model survive a partial KV op — was
  declared in six places, in three vocabularies, and measured in none
  (`embedded/capabilities.rs` module doc has the table). §10.6: "a
  duplicated decider diverges and you get a plausible number, with
  nothing red anywhere." It produced the dense-`qwen35` miss
  (2026-06-09), two FastShort ladders of different width, and a repro
  harness that recommended deleting a gate it never exercised.
- **What it measures:** TWO rollback arms differing in exactly one
  variable — whether a decode-pass boundary is crossed — compared to a
  straight prefill by L2 over the full logit vector. The `gen_before=0`
  arm is the model's own float-noise floor; the `gen_before=2` arm is
  the signal. `Safe` iff signal <= 4x floor.
- **Why a ratio and not a threshold (measured 2026-08-10,
  `rs_rollback_spike::logit_delta_calibration`):**

  | model | floor | signal | ratio |
  |---|---|---|---|
  | `qwen35moe` 36B | 97.6 | 1656-1793 | **17x** |
  | `qwen35` 2B | 19.9 | 459-644 | **23x** |
  | `gemma4` (correct) | 94.3 | 94.3 | **1.00x** |

  Absolute floors differ 5x across models — `gemma4`'s CORRECT delta
  exceeds `qwen35`'s floor — so any fixed constant misclassifies
  somebody in a zoo. Each model supplies its own control.
- **Why not sampled tokens.** The first detector compared greedy
  continuations and had a MEASURED false negative: `qwen35moe` probed
  `Safe` while the sweep showed it corrupt at every depth. Greedy argmax
  absorbs a perturbed state; `top1` was unchanged in every calibration
  row, including the corrupting ones, while L2 moved 17-25x. Tuning the
  constants moved the holes around rather than closing them.
- **Cost:** 76-653ms per chat-slot load measured across the three
  models (three 192-token prefills + 2 decodes; the logit detector is
  ~2.5x FASTER than the token one it replaced, which generated
  continuations). Skipped
  for distributed children and for the FastShort sibling (its batched
  path carries no prefix reuse at all).
- **Review by:** the flip of `prefix_cache_gate` onto the measured
  verdict is a SEPARATE change, gated on shadow data showing whether
  probe and ladder ever disagree — `journalctl --user | grep capability`
  is the dataset. Zero disagreements is a legitimate reason to stop
  here rather than a failure. Notes `8291000e`, `2022a071`, `923ca1e1`.

### Caller-directed prefix-cache pin — `SOVEREIGN_PREFIX_STATE`
- **Default ON 2026-08-03**, opt out with `=0`. Genuinely flipped this
  time — `env_enabled()` now defaults true.
- **This row was FALSE for thirteen days and that is the lesson.** It
  claimed "default-on 2026-07-21" when the flip had never happened:
  `BATCHED_GATE_VERIFY.md` *recommended* flipping after two hardenings,
  those hardenings landed, and the row recorded the recommendation as
  executed. A false GRADUATED row is worse than no ledger, because it
  is trusted. Nothing parses this file's review-by dates (T1 B2 is the
  gate that would have caught it).
- **Earned by:** controlled A/B through the production answer path,
  `svrn bench enrichment-ablate sovereign/bench/obsidian/questions.toml
  --prefix-state --reps 2`, on `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`:

  | arm | reps | mean wall | fact ratio |
  |---|---|---|---|
  | off | 901.7s, 835.2s | 868.4s | 0.4736 |
  | on  | 671.1s, 667.0s | 669.0s | 0.4597 |

  **1.30x, −199s per rep**, against an OFF-arm spread of 66.5s — the
  delta is 3x the noise. Arms proven distinct by pin telemetry: OFF
  `LEARNED=0 HIT=0`, ON `LEARNED=28 HIT=86`. Reproduces the 2026-07-21
  result (1.35x, 786.3s → 584.5s) on HEAD.
- **The earlier "worth ≈0" result was never a contradiction.** The
  2026-07-12 A/B measured ONE synthesis prefill; the pin's only
  consumer is the grounding gate, which issues ~35 judge calls per turn
  each re-prefilling the same evidence. Two workloads, not two answers.
- **Open caveat, stated rather than buried:** the quality delta is
  −0.0139 mean fact ratio (~1 fact in 60). That is below the ablation's
  0.02 separation floor and reports as NOT SEPARABLE, but it was
  IDENTICAL in both reps — a small reproducible difference, not noise.
  If restore is bit-exact it should be zero. Settle it by checking
  restore bit-exactness, not by adding reps (the eval is deterministic
  per arm, so more reps of the same config cannot move it).
- **Model scope:** measured on `qwen35moe`. The pin's value scales with
  prefill cost, and `prefix_cache_gate` vetoes ordinary partial-KV
  reuse on both `qwen35moe` and dense `qwen35`, so on those the pin is
  the ONLY caching available. On a small primary the win will be
  smaller and the ~64KB/token state cost proportionally larger; the
  byte-capped LRU (`_MAX_MB`, default 2048) is what bounds it.
- **Instrument:** `svrn bench enrichment-ablate --prefix-state` is
  committed and is the template for any daemon-side knob. The original
  harness (`scratchpad/arm_runner.py`) never was.

### RAPTOR grounding — `SOVEREIGN_RAPTOR_GROUNDING`
- Default **on**, status shipped (`env-flags.toml`). Summary nodes as
  virtual chunks earned the default.

## `SOVEREIGN_DR_AUDIT_BATCH_LOCATE` — the audit's location loop, batched

**Landed 2026-08-26 default ON. TURNED OFF THE SAME DAY BY ITS OWN FLIP
CONDITION.** Now DEFAULT OFF. Paired measurement mode:
`SOVEREIGN_DR_AUDIT_LOCATE_SHADOW`; bed: `sovereign-core/tests/binder_replay.rs`.

**Read this row for the pattern, not just the flag.** The argument for shipping
it on was structural and, as far as it went, correct: the triage can only cost
support, never manufacture it, because the calibrated register still decides
every chunk that binds. That argument is about DIRECTION. The ledger asked for
a MAGNITUDE — `shadow_lost = 0` — and the magnitude is what failed. A
direction argument is not a measurement, and shipping on the strength of one is
the thing this row now exists to discourage.

**What it changes.** `audit::assess_claim` located a claim's origins with one
calibrated model call per candidate chunk. With the flag on, the model-free
stages run for every chunk first, ONE triage generation over the pinned
evidence window shortlists the candidates, and the calibrated per-span judge
runs only for those it admits or cannot parse a verdict for.

**Why.** Measured on the pin-validate flight
(`research/deep-research/arms/runs-pin-validate/pinned-1.log`, 328 claim
audits, 102.5 minutes):

| claims | share | wall-clock | share of audit |
|---|---|---|---|
| reached the location loop | 35 (11%) | 90.6 min (~130s each) | **88%** |
| short-circuited before it | 285 (87%) | 8.6 min (1.85s each) | 8% |
| everything else | 7 (2%) | 3.3 min | 3% |

130s is 52-57 calibrated calls against a 54-chunk window, binding 0–2. Three
hours per question is not shippable (operator, 2026-08-25). Note that the
batched STAGE-1 pre-pass named in the session frame would have attacked the 285
fast claims — at most 8.6 minutes of the 102.

**What the bed measured (2026-08-26, claim 1 of the binder bed, 54 chunks).**

| arm | wall-clock | verdict | bound |
|---|---|---|---|
| per-span | 343.4s | **Passed**, 2 origins | `ev-25`, `ev-30` |
| batched (first cut) | 144.4s | **CouldNotJudge**, 0 origins | — |

A verdict change, in the abstention direction, on the first claim that had
support to lose. Stage split for the batched arm: locate 62.3s (model-free
embedding, unchanged by this flag), **triage 72.1s**, calibrated 7.9s (3 calls).

**Two causes, both fixed, NEITHER YET RE-MEASURED.**

1. *The prompt asked the wrong question.* It asked whether a passage supported
   the claim "ON ITS OWN" — a stricter bar than the calibrated judge's — where
   a triage owes RECALL. It voted B on 49 of 52. The wording now tracks
   `claim_prompt`'s exactly and states the asymmetry outright: a wrong admit
   costs one ~2.5s call, a wrong reject loses the evidence permanently, so when
   in doubt answer A.
2. *The window shared a prefix with nothing.* It was built from
   claim-conditioned best-spans, so every claim paid a fresh 12k-token prefill:
   71,947ms at this host's ~160 tok/s, against the 1,613ms the same claim's
   whole-window judge paid on a warm prefix. The triage now runs over the
   PINNED window — the identical slice the whole-window judge was handed — so
   `EvidenceFamily` renders the same prefix bytes and the daemon restores it.

**What does NOT change when it is on.** Every chunk that binds has cleared
`SUPPORT_FLOOR` on the same calibrated forced-choice register. Also untouched:
the verbatim-figure precheck, the `MIN_LOCATE_SIM` locate floor, the
corroboration floor, the custody veto, the containment witness, and the chunk
order `supporting_chunk_ids` is recorded in.

**Reversal condition.** Flips back ON only when a binder-bed run over all 6
loop-reaching claims shows `per_span_only` EMPTY for every claim — the batched
arm binds everything the per-span arm bound — at a wall-clock that is
materially better, and a subsequent flight run with
`SOVEREIGN_DR_AUDIT_LOCATE_SHADOW=1` reports `shadow_lost = 0` across the bank.
Anything short of that is a latency win bought with comprehensiveness, which is
one of the two dimensions we are furthest behind AIQ on (note bdf94683: insight
−18.06, comprehensiveness −16.67). **Re-measurement in flight.** Review by
2026-09-09.

## `SOVEREIGN_DR_AUDIT_LOCATE_BUDGET` — a declared bound on the search

**Landed 2026-08-26, DEFAULT 0 (unbounded).** Composes with, and depends for
its legitimacy on, `SOVEREIGN_DR_AUDIT_LOCATE_EARLY_EXIT`.

**Why this is the lever and the other two are not.** A calibrated per-span call
costs ~2.4s, and that cost is a ~360-token prefill which **cannot be shared**:
the register exists to judge one passage in isolation, so there is no prefix
family to put it in (its calls log `stable_prefix_bytes=None` by design, and
that is correct). Batching the triage cut a claim from 52 calls to 27 — 1.2–1.7×
measured across three bed claims, which is real and is not an answer to three
hours per question. The only remaining way to spend less is to make fewer
calls.

**What the spend buys today.** Bed claims 2 and 3 spent 50 and 14 calibrated
calls to reach could-not-judge. On the source flight 1 of 6 loop-reaching
claims passed. Most of this loop's cost purchases an abstention.

**Why a bound is defensible here.** Only because the candidates are judged
best-first by the cosine stage 2 already computes. A claim whose origins exist
is expected to find them early; a bound on an unordered sweep would be a
coin-flip and should not ship.

**It is not a silent truncation.** A claim that exhausts its budget without
reaching the corroboration floor carries `SEARCH BOUNDED: stopped after N
calibrated call(s) of M candidate(s)` in its own reason. The record keeps "we
looked at everything this claim could rest on" apart from "we stopped after N"
(§18.3). Watched red by
`the_call_budget_bounds_the_loop_and_names_itself_in_the_record`, which asserts
BOTH halves — the call count and the disclosure — because a bound that shrank
the count while reporting a plain floor miss would be the silent truncation
wearing a finding's clothes.

**The risk, stated plainly.** A claim whose two binding origins rank below the
budget in cosine order loses a `Passed` verdict it would otherwise have earned.
That is comprehensiveness traded for latency, against a bar where
comprehensiveness is already −16.67 (note bdf94683).

**Reversal condition.** Ships on only if the bed's `fast` arm reproduces the
`per-span` arm's VERDICT on every claim that passes, at a materially better
wall-clock — and the budget is then set from the measured rank distribution of
binding origins, not guessed. If binding origins routinely rank deep, this
stays off and the loop stays expensive. **Measurement in flight.** Review by
2026-09-09.

## `SOVEREIGN_DR_AUDIT_LOCATE_EARLY_EXIT` — stop at the floor, best first

**Landed 2026-08-26, DEFAULT OFF.** Composes with, and is measured separately
from, `SOVEREIGN_DR_AUDIT_BATCH_LOCATE`.

**What it turns on.** The location loop judges candidates in descending cosine
order and stops once the claim has two distinct origins.

**Why.** The batched triage got a passing claim from 52 calibrated calls to 27
— about 1.55× on the warm per-claim term (127s → 82s), which is real and is not
an answer to "three hours per question". On the bed's claim 1 the two binding
origins were `ev-25` and `ev-30`, found part-way through a sweep ordered by
nothing. Best-first with an exit at the floor stops when it has what the floor
needs.

**Verdict-identical by construction.** The exit fires only when the claim has
ALREADY cleared the floor, which is the condition under which it passes. A
claim that never reaches the floor visits every candidate exactly as before —
which is most claims, and is why this is a lever on passing claims specifically.

**What it does change.** `supporting_chunk_ids` and
`corroboration.support_chunks` shrink: the record carries the origins that
settled the claim, not every chunk that would have bound. Fewer citations per
claim is a product change, not just a latency one, and it is not obviously good
— which is exactly why this is its own flag with its own arm in the bed rather
than shipped inside the batch.

**Reversal condition.** Flips on only if the bed's `early` arm reproduces the
`per-span` arm's VERDICT on all 6 claims at a materially better wall-clock, AND
the shrunken citation sets are judged acceptable against the AIQ criteria that
reward substantiation. If per-claim citation count turns out to carry score,
this stays off regardless of what it saves. **Not yet measured.** Review by
2026-09-09.

## `SOVEREIGN_DR_AUDIT_LOCATE_SHADOW` — pricing the triage

**Landed 2026-08-26, DEFAULT OFF.** Measurement mode, never a product one.

**What it turns on.** The calibrated per-span judge runs for the spans the
triage REJECTED as well, and `shadow_lost` counts how many of them the
calibrated register would have bound (INFO on the `t5 binder` event, WARN
whenever non-zero).

**Why.** With the triage on and this off, a rejected span is never judged and
its lost support is unobservable BY CONSTRUCTION — the same blindness that
made the grounding ladder's safety argument unmeasurable until
`SOVEREIGN_GATE_CLAIM_SEARCH_SHADOW` existed. This is the only configuration
in which `SOVEREIGN_DR_AUDIT_BATCH_LOCATE`'s cost can be priced rather than
assumed.

**Cost.** The full pre-batch call count PLUS the triage generation, so a
shadow run is slower than the loop it replaced. That is the point.

**Reversal condition.** Graduates (stays off, permanently available) once the
batch flag's condition above is settled. It is an instrument, not a
capability, so it is never expected to flip on by default.

## `SOVEREIGN_DR_REPORT_OUTLINE` — the report outline is not the search frontier (drb1-r5)

**Landed 2026-08-24 default OFF. DEFAULT FLIPPED ON 2026-08-27.** Campaign
`drb1-race`. Requires `SOVEREIGN_DR_COMPOSED_REPORT=1`. Set
`SOVEREIGN_DR_REPORT_OUTLINE=0` to restore the frontier.

**Why the default moved, and it is NOT a score argument.** Operator direction,
2026-08-27: stop optimising against the RACE judge and impose our own ethos on
the deliverable. The frontier is a list of retrieval targets — the planner
prompt asks it for "the specific measure or statistic it implies — an index, a
ratio, a share, a rate, a count". Those are good queries and they are
indefensible as section headings a reader receives. The shipped task-69
deliverable carried `## Median Session Establishment Time` and `## Schema
Mismatch Failures in Third-Party Tool Integration` as TOP-LEVEL sections: our
search plan printed as prose, 20 of them, 11,270 words. No judge is needed to
call that wrong, and no judge was consulted for this flip.

**What it is expected to cost.** RACE overall, probably. The judge compares
head to head against references running 6,898-13,348 words and this makes our
deliverable materially shorter (see the `TARGET_REPORT_WORDS` retirement in
`synthesize.rs`, landed the same day — length now derives from the evidence
each section receives). That trade was made deliberately and with the operator
naming it: the bench is a tripwire here, not the gate. **Reversal condition:
not a RACE regression** — set `=0` if readers report the planned outline
omitting subjects the frontier covered, or if the fallback-to-frontier warning
fires on a material share of flights (it means the planner is refusing and
nobody is reading the trace).

**Safe to default because the fallback is named.** A refused or unusable
outline falls back to the search frontier and logs "outline unavailable —
sections fall back to the search frontier (named, never silent)" (§18.3,
`mod.rs`). The flip changes which path is normal, not whether a failure is
visible.

**What it turns on.** The composed report's sections come from an outline
planned over the gathered evidence — each distinct subject given standalone
treatment where it needs explaining on its own terms, then the sections
relating them, then what follows — instead of one section per planned
sub-question.

**Why.** The sub-questions ARE the acquisition frontier, and the planner
prompt tunes that list for retrieval: it asks for "the specific measure or
statistic it implies — an index, a ratio, a share, a rate, a count". Good
search queries; bad section headings. The task-69 web arm's real section list
included *"Count of distinct error handling states defined in the A2A message
schema"*.

**The evidence is the judge's own words**, on the arm with 98 sources and
2.18M chars — so this is not an evidence gap:

| criterion | ours | ref | the judge's reason |
|---|---|---|---|
| Breadth and Depth of MCP Protocol Description | 5.0 | 9.0 | "Article 2 dedicates Section III to MCP… Article 1 lacks a comprehensive standalone explanation" |
| Clarity and Substantiation of A2A/MCP Connections | 6.0 | 9.5 | "Article 2 has a dedicated Section VI ('Interplay and Relationship')" |
| Clarity and Logical Rigor in Problem-Solution Mapping | 6.5 | 9.5 | "Article 2 explicitly maps problems to solutions in Section IX" |
| Depth and Nuance in Comparative Analysis | 7.0 | 9.0 | ours "risks being overly granular or speculative" |

Three of the four largest weighted losses are structural. The fourth says the
fragmentation costs **insight** — our worst dimension against AIQ (−18.06).
It also explains why widening the frontier 8 → 20 did not help: it made the
deliverable more fragmented, not better organised.

**What does NOT change.** Acquisition. The frontier keeps its job and its
width; this changes only what the writer is asked to build. Citations,
corroboration floor, custody veto and the audit are untouched.

**Refusal.** An outline parsing to fewer than 2 briefed sections is refused
and the loop falls back to the frontier, naming the fallback. A bare heading
with no brief is not a section — watched red in
`deep_research::synthesize::tests::the_outline_refuses_a_frontier_shaped_list`,
which under the permissive rule parses the literal task-69 frontier as a
five-section outline.

**Cost.** One extra `Speed::Slow` draft call per run.

**Reversal condition.** Flips on only if, replayed against a FIXED cached
estate (acquisition held constant, so writer variance is the only noise), the
outline arm beats the frontier arm on RACE overall with the honesty floor
intact — and the comprehensiveness and insight dimensions move, since those
are what the diagnosis predicts. **Not yet measured.**

## `SOVEREIGN_DR_REPORT_SECTION_EVIDENCE` — show the writer the evidence (drb1-r9)

**Landed 2026-08-26, DEFAULT OFF.** Campaign `drb1-race`. Requires
`SOVEREIGN_DR_COMPOSED_REPORT=1`. Deliberately separate from
`SOVEREIGN_DR_REPORT_ARCHITECTURE`: that flag decides the deliverable's shape,
this one decides how much evidence stands behind each part of it.

**The measurement that produced it** — task-69 control flight `dr-1787742429`,
counted off its own `evidence-window-1.json`, not reasoned about:

| | |
|---|---|
| evidence window | 46 chunks, **1,060,308 chars** |
| chunks mentioning MCP | **40 / 46** |
| chunks carrying MCP primitives (Tools/Resources/Prompts/Roots/Sampling) | **41 / 46** |
| what ONE section's writer sees | 8 passages × 1400 chars = **11,200 chars — 1.06%** |
| what the whole 8-section report sees | **8.5%** |
| distinct sources the 11,345-word deliverable cites | **22 / 46** |
| sections describing MCP | **0** |

**Acquisition is not the constraint, and this is the sentence that matters:
the writer is not failing to use the evidence, it is not being shown it.** The
material for the MCP section we lose 3.24 points on — in 25 of 25 draws — is
sitting in 40 of the 46 chunks that were retrieved for it.

This supersedes, for this bed, the older reading at the foot of the
`SOVEREIGN_DR_COMPOSED_REPORT` row ("the deliverable SHAPE is no longer the
binding constraint on this score; acquisition is"). That was measured on a run
whose entire window was 4 chunks and 1,866 chars. On a window three orders of
magnitude larger the binding constraint moved, and it is now the budget between
the window and the writer.

**What it turns on — and these are not free parameters.** 28 passages per
section at a 5-per-source cap, from 8 and 3. That **restores the configuration
the composed report's own quality number was measured at.**

`compose_report` was ported to Rust in `a50d2fdf3` (2026-08-23) from the Python
prototype `research/deep-research/arms/lab/compose2.py`. That commit's own
message says: *"The 44.40 composite that stood in for its quality was measured
by `arms/lab/compose2.py` — a Python reimplementation."* The prototype chunks
passages identically (`passages(chunks, size=1400, overlap=200)` against our
`PASSAGE_CHARS`/`PASSAGE_OVERLAP`), ranks by the same cosine, applies the same
per-source cap mechanism — and ran at `k=28, repeat_cap=5`, recording the
consequence in its own manifest as `evidence_chars_per_section: k * 1400` =
**39,200**.

**The port shipped 8 and 3 — 11,200 chars, a 3.5× cut — and no commit, note or
ledger row records that as a decision.** It is a port artifact, not a tuned
bound. Commit `14ddccf49` later OBSERVED the consequence — its own registry
text reads *"ours showed each section eight passages by cosine, so on the
logged task-69 flight a 38-chunk window reached the writer eight chunks at a
time"* — and responded by adding the research-notes flag rather than by
restoring the number. **So the shipped Rust path has never been run at the
configuration whose measured quality justified building it.**

Both knobs move through ONE decider (`synthesize::section_evidence_budget`)
because widening the count while holding the cap at 3 fills the new room from
new SOURCES only — the opposite of what a section needing depth on one protocol
requires. The test pins the pair to the prototype's values and to the 39,200
figure, so a later edit away from them fails rather than quietly testing
something no measurement stood behind.

**What does NOT change when it is on.** The citation contract, and therefore
the gate. The same passages, from the same window, ranked by the same cosine,
carrying the same `ev-N` handles — only how many of them the writer sees
changes. No new evidence enters, nothing is re-chunked, the audit locates spans
exactly as before.

**Cost.** ~3.5× the section-writer prefill (11.2k → 39.2k chars per section).
No extra calls. On a bed where the audit already dominates wall-clock this is
not the expensive part, but it is not free and the arm must report it.

**Reversal condition.** Flip default-ON only on a pre-registered arm whose mean
exceeds the like-for-like control by more than the measured band, with
comprehensiveness moving in the direction claimed. Revert on any arm that costs
score beyond the band, and — the specific risk of this lever — on any rise in
audit refusals or could-not-judge verdicts: three times the evidence in front
of a writer is three times the opportunity to assert something the section's
own citations do not carry.

**THE ARM REPORTED — 2026-08-27. The flag stays OFF, and the DEFAULT moved
instead.** The reversal condition above asks for a pre-registered arm beating
the like-for-like control. The sweep flew five points on bed `dr-1787807617`
(task 69), one judge (`Qwen3.8-27B-UD-Q6_K_XL`), scored on the same ruler as
the flights. The compose replay is a zero-noise instrument — writer re-fly
byte-identical across a daemon restart (sha `517f692874b5d496`), judge
identical to the last digit — so these are exact for this bed, not one draw:

| arm | RACE overall | delta | words | min |
|---|---|---|---|---|
| 8x3 (the then-default) | 45.9166 | +0.00 | 10829 | 10.6 |
| **16x4** | **51.3347** | **+5.42** | 10707 | 11.2 |
| 28x5 (THIS FLAG) | 50.9864 | +5.07 | 10200 | 12.3 |
| 44x6 | 50.9510 | +5.03 | 10178 | 14.1 |
| 60x8 | 51.9689 | +6.05 | 10064 | 16.2 |

The lever is real and this row's diagnosis was right: the writer was starved,
and showing it more evidence is worth ~5 points. But **28/5 is not where the
gain lives.** The whole effect is the first step, 8→16; 16/28/44 then sit
inside 0.4 of each other while cost climbs monotonically. So the action is to
move the SHIPPED DEFAULT to 16/4 (`synthesize::SECTION_PASSAGES` /
`PER_SOURCE_CAP`, pinned by test) rather than to flip this flag on — the flag
would buy 0.35 points LESS than the new default for +1.1 min and 2.5× the
per-section prefill.

Read the 60x8 row carefully: it is nominally the highest, by +0.63 over 16x4,
and it is NOT evidence for going wider. The middle of the curve is
non-monotone (44x6 is the lowest of the top four), which is the signature of a
plateau with task-level jitter. Treating +0.63 as a trend across a
non-monotone plateau is the single-run delta §18.5 forbids, and it would cost
+5.0 min and ~19.9 GB of unreclaimable host memory per flight (the writer's
output buffer is `n_vocab * prompt_tokens * 4`; see
`research/deep-research/arms/mem-forensics/PREREG-buffer-threshold.md`).

**n=1 ACROSS TASKS.** One bed, one question. The curve's SHAPE is what moved
the default; its third digit is not load-bearing. The flag is kept as-is, now
as a dominated widening rather than a configuration to restore.

**Evidence at landing.** NOT YET MEASURED — landed dark. No claim about its
effect may be made until a pre-registered arm exists.

## `SOVEREIGN_DR_REPORT_ARCHITECTURE` — the deliverable is a report (drb1-r8)

**Landed 2026-08-26, DEFAULT OFF.** Campaign `drb1-race`. Requires
`SOVEREIGN_DR_COMPOSED_REPORT=1`. Composes with `SOVEREIGN_DR_REPORT_OUTLINE`:
that flag decides WHICH sections exist, this one decides what surrounds them.

**What it turns on.** Three things, together, because they are one property —
the deliverable is shaped like a report rather than like the pipeline that
produced it. (1) The report gets its OWN title; the default H1 is the user's
raw prompt sentence. (2) An `## Executive Summary`, written last and read
first, answers the question in its opening two sentences; the closing section
is titled `## Conclusion` rather than `## Synthesis and Assessment`. (3) The
section cap rises from 8 to 12.

**Why it exists — diagnosed from the judge's own per-criterion scores, not
guessed.** Across the 25 scored task-69 judge records on disk, ranked by our
deficit against the reference article:

| Δ | ours | ref | k | criterion |
|---|---|---|---|---|
| −3.24 | 6.12 | 9.36 | 25/25 | comprehensiveness / Breadth and Depth of MCP Protocol Description |
| −2.90 | 6.60 | 9.50 | 25/25 | readability / Logical Structure and Coherent Flow of Argumentation |
| −2.50 | 6.98 | 9.48 | 25/25 | readability / Formatting, Layout, and Typographical Consistency |
| −2.22 | 6.80 | 9.02 | 25/25 | readability / Paragraph Cohesion, Sentence Fluency, and Conciseness |

Six of our seven worst criteria are readability — a property of how the
article is WRITTEN, not of what was retrieved. The worst single criterion is
the *second* subject the question names: task 69 asks about A2A **and** MCP,
and our outlines spend their 8 sections before MCP gets one of its own, while
the reference article gives it a dedicated section. AIQ's own published
task-69 article (`inputs/aiq-subset-articles.jsonl`, read directly rather than
inferred) carries an executive summary, a numbered section per subject, a
consolidated comparison table, a section mapping innovations to the problems
they address, and a conclusion. Ours carried 48 headings, every one of them a
retrieved statistic — "Search Visibility Surge Following A2A Announcement",
"Payload Efficiency in Agent Handshakes" — and no section describing MCP.

**What does NOT change when it is on.** The citation contract, and therefore
the gate. The title and summary introduce no claims of their own: the summary
is instructed to assert nothing the report does not already establish and to
reuse the `[Source: ev-N]` handles already used below it, and both are composed
BEFORE the audit pass, so every sentence they carry faces the same gate as the
sections. Nothing about the window, the handles, the corroboration floor, the
custody veto or the containment witness moves.

**One decider for the cap.** `synthesize::outline_max()` is read both by the
prompt that ASKS for n sections and by the parser that ADMITS them, so the
writer can never be asked for 12 and admitted at 8 — a silent truncation that
would read as "the model planned 8". Watched red in
`the_section_cap_is_one_decider_for_the_prompt_and_the_parser`.

**Two refusals, both named, never silent (§18.3).** A title outside 20–160
chars is refused — a paragraph is the model answering instead of naming, a
stub names nothing — and the H1 falls back to the question with the fallback
in the trace. A failed summary lands the report without one, also traced.
Watched red in `the_title_parser_refuses_what_is_not_a_title`.

**Cost.** Two extra draft calls per run (title, summary).

**Reversal condition.** Flip default-ON only on a pre-registered arm whose mean
exceeds the like-for-like control by more than the measured band, with
readability moving in the direction claimed. Revert on any arm that costs score
beyond the band, or on any rise in audit refusals — an executive summary must
never become a place to assert what the sections could not. Per-draw sd is 2.97
and the judge contributes **zero** of it (note `403a218a`), so n is the only
lever on resolution.

**Evidence at landing.** NOT YET MEASURED — landed dark.

**FIRST MEASUREMENT 2026-08-27 — EXPLORATORY, NOT THE PRE-REGISTERED ARM. The
default does NOT flip on this.** A like-for-like A/B on the compose-replay bed
`dr-1787807617` (task 69), both arms at the new 16/4 default, one judge, the
only difference being this flag:

| | overall | readability (ours) | h2 |
|---|---|---|---|
| control | 51.3347 | 8.07 | 21 |
| **this flag on** | **52.1293** | **8.79** | 22 |

+0.79 overall, and readability — the criterion this row claims — moves +0.72,
closing 59% of the gap to the reference. The replay is a zero-noise instrument
(note `680940ce`: writer re-fly byte-identical, judge identical to the last
digit), so on THIS bed the band is 0 and the delta is exact. Per-criterion, two
close outright: Comparative Presentation -1.50 -> 0.00 and Formatting
-1.00 -> 0.00. Technical Language slips 9.5 -> 9.0, the one regression.

Why this is NOT the arm the reversal condition asks for, stated plainly so
nobody mistakes it later: it was exploratory rather than pre-registered, it is
n=1 across TASKS (one bed, one question), and the per-draw sd of 2.97 quoted
above is a FLIGHT figure — the replay pins the window, so its zero band does
not transfer to the flight the condition governs.

What it does establish is the mechanism, and it is not the one this row
assumes. The flag did NOT reduce fragmentation: 22 h2 headings against the
control's 21, because `outline_max()` is read by `plan_outline`, which
`compose_report` never calls — the bed replays a pinned 20-section outline, so
the 12-section cap is inert on this path. The gain came from the report title
and the Executive Summary alone. The two readability criteria still at -1.00
are Logical Structure and Paragraph Cohesion — the fragmentation ones — which
is why the outline itself is the next arm (`arms/bed/fly-outline-arm.sh`,
via the new `COMPOSE_SECTIONS`).

## `SOVEREIGN_MTP_PREFILL_TAIL_LOGITS` — stop reserving a logits row per prompt token

**Landed 2026-08-27, DEFAULT OFF.**

**What it is for.** The MTP prefill flags every position for logits, which
makes `n_outputs_all` the whole prompt (`llama-context.cpp:1700`), so
`output_reserve` sizes the logits buffer at `n_vocab * n_prompt_tokens * 4` —
**993,280 bytes per prompt token** at the qwen35 family's 248,320 vocab. Past
~3,650 tokens that buffer exceeds this device's 4 GiB `maxBufferSize`, fails to
pin, and falls back to a host CPU buffer that is retained at high-water until
the process exits (`ggml-vulkan.cpp:16191`; `llama-context.cpp:2092` grows and
never shrinks). One 20k-token request strands ~19.9 GB. It is the daemon's
single largest unreclaimable allocation, and the reason "restart between runs"
kept being proposed as a workaround the operator had already ruled out.

**The justification for the flag is false, and that is measured, not argued.**
`model_slot.rs` cites an upstream invariant — `get_embeddings_pre_norm_ith`
errors with `batch.logits[N] != true`. That error comes from
`output_resolve_row`, which `get_embeddings_nextn_ith` calls ONLY on the masked
path (`llama-context.cpp:966`). The unmasked path returns early at :954-960,
indexing nextn rows "densely, by raw token position", and `common_speculative`
initialises the MTP **target** unmasked (`speculative.cpp:1371`) — confirmed at
runtime, the daemon logs `set_embeddings_nextn: value = 1, masked = 0`.

**The derisking ladder, in the order it was climbed**
(`tests/mtp_prefill_logits_spike.rs`, Qwen3.5-4B-UD-MTP, 681-token prompt):

| | pre-norm | next-token |
|---|---|---|
| FLOOR (all vs all) | 0e0 | 0e0 |
| SIGNAL (all vs last) | **0e0** | **1.46e-1** |

Pre-norm hidden states — what MTP actually consumes — are BIT-IDENTICAL at
every one of the 681 positions. 128 greedy tokens generate identically.

**Why it is nevertheless default OFF.** The floor arm is what makes the second
column a result rather than noise: the model is bit-deterministic run-to-run in
one configuration, so the 1.46e-1 next-token shift is REAL and attributable —
`n_outputs_all` going from n to 1 changes the output-gathering graph and hence
the float accumulation order for the row read. It did not flip a greedy
decision over 128 tokens, but "did not flip in 128 tokens on one prompt" is not
"cannot flip", and a composed report is ~10,000 words.

**The vendored precondition has a tripwire.** A bump flipping that `masked`
argument would not degrade — it would make `output_resolve_row` throw on an
unflagged position and `GGML_ABORT` the daemon, so by the time a runtime check
could see it the process is gone. `the_mtp_target_context_is_still_initialised_unmasked`
asserts it at rest, in the normal test run.

**Cost when on.** None. It removes work and removes an allocation.

**Reversal condition.** Flip default-ON when a byte-identity arm on a real
composed report reproduces the control's draft sha256 exactly, with the memory
step measured on the same run. Revert on any divergence in composed output, or
on any arm that costs RACE score beyond the band.

**Evidence at landing.** The spike above. The byte-identity arm is IN FLIGHT
and its result belongs here before the flip; until it lands, this row's claim
is "the mechanism is clear", not "the change is safe".

## `SOVEREIGN_DR_SECTION_CONTEXT` — the section knows it is part of a report

**Landed 2026-08-27, DEFAULT OFF.** Requires `SOVEREIGN_DR_COMPOSED_REPORT=1`.
Deliberately separate from `SOVEREIGN_DR_REPORT_ARCHITECTURE`: that flag
decides the deliverable's SHAPE, this one decides whether a section knows it is
part of one.

**The measurement that produced it, and the one that killed the obvious
alternative first.** After the section-evidence default moved to 16/4,
readability is the ONLY RACE dimension still behind the reference, and the only
one the evidence budget does not move:

| dim | 8x3 | 16x4 | 28x5 | 44x6 | 60x8 | gap at 16x4 |
|---|---|---|---|---|---|---|
| comprehensiveness | 7.58 | 8.92 | 8.92 | 8.75 | 9.08 | **+0.42** |
| insight | 7.50 | 9.12 | 9.00 | 9.25 | 9.12 | **+1.12** |
| instruction_following | 8.60 | 9.70 | 9.00 | 9.60 | 9.70 | **+0.60** |
| readability | 7.86 | 8.07 | 8.00 | 8.43 | 8.21 | **-1.21** |

The judge's objection is one sentence: the report has *"a somewhat fragmented
structure with many short sections that jump between topics ... reads like a
collection of research findings rather than a single narrative arc"*.

**The obvious cause was flown and REFUTED.** Ours carried 21-22 h2 headings
against a reference that reads as nine, so the outline looked like the culprit.
A 10-section outline over the SAME window — planned by production
`plan_outline`, flown via the new `COMPOSE_SECTIONS`, architecture flag held on
in both cells — left Logical Structure at **exactly 8.5, unchanged**, while
costing 0.61 overall and dropping Formatting 9.5 -> 8.5:

| | overall | readability | Logical Structure | h2 | words |
|---|---|---|---|---|---|
| 20-section control | 52.1293 | 8.79 | 8.5 | 22 | 10974 |
| 10-section arm | 51.5225 | 8.64 | **8.5** | 10 | 9100 |

Section COUNT is not the cause. What is left is the prompt: `synthesize.rs`
hands a section writer the question, its own sub-question, its evidence and a
word budget, and NOTHING about its neighbours. Every section is composed in
isolation no matter how many there are — ten isolated sections read as
disjointed as twenty. This flag prepends the ordered section list with an arrow
on the writer's own entry, and tells it to assume the reader has read what is
above and will read what is below.

**What does NOT change when it is on.** The citation contract and therefore the
gate. No new evidence enters, no passage is re-ranked, and no extra model call
is made — only the prompt's preamble grows, by roughly 40 characters per
planned section (~800 on a 20-section plan, about 3.5% of a 16x4 section
prompt).

**The failure it is built to avoid.** The arrow follows `si` (an index into
`subs`), while the numbering follows write order (`kept`). Confusing the two
points every section at the wrong neighbour and produces fluent, correct prose
about the wrong place in the report — which surfaces as a failure nowhere. Hence
`section_arc` is a function with tests, not an inline `format!`; watched red in
`the_section_arc_points_at_the_writers_own_section` and
`the_section_arc_numbers_by_write_order_not_by_sub_index`.

**Cost.** No extra calls. Prompt preamble only, as above.

**Reversal condition.** Flip default-ON only on a pre-registered arm whose mean
exceeds the like-for-like control by more than the measured band, with
readability — specifically Logical Structure and Paragraph Cohesion, the two
criteria still at -1.00 after the architecture flag — moving in the direction
claimed. Revert on any arm that costs score beyond the band, and on any rise in
audit refusals: telling a writer to assume what an earlier section established
is an invitation to assert it without carrying its own citation.

**Evidence at landing.** NOT YET MEASURED — landed dark. The diagnosis above is
measured; this flag's own effect is not.

## `SOVEREIGN_DR_WRITER_CONTRACT_V2` — the graded evidence steer (drb1-r7)

**Landed 2026-08-26, DEFAULT OFF.** Campaign `drb1-race`.

**What it turns on.** Two things, together, because either alone is
incoherent. (1) The evidence block handed to each section writer is GRADED:
findings carry `[ANCHOR]`/`[SUPPORT]`/`[WEAK]` from their existing 0-100
`usefulness`, and passages carry the same three tiers from their existing
descending-cosine rank. (2) `WRITER_CONTRACT` gains four obligations ported
from AIQ's `writer.j2` — how to use the grade, name consensus and combine
complementary evidence, inference that is licensed but marked and
confidence-weighted, and when a table earns its place. Showing a grade without
stating its obligation would be decoration; stating the obligation with no
grade to apply it to would be noise.

**Why.** The grade is not new information — **we already computed it on both
paths and threw it away.** `notes::findings_block` has always sorted findings
by `usefulness` and then dropped the number; `synthesize::rank_passages` has
always returned passages best-first and the evidence block has always
flattened that ordering. The writer received an ordered list and was never
told the ordering meant anything. AIQ steers synthesis by exactly this signal
(`evidence_judgment`: anchors carry, medium supports, low is for gaps and
caveats). This is the cheapest structural item in their writer we lacked, and
it costs **zero extra inference calls**.

It is the next lever because the alternative was measured and failed. Note
`403a218a`: `SOVEREIGN_DR_ALL_LEGS_SLOW` moved the writer from the 4B to the
27B on every drafting leg, routing witnessed in both directions, and produced
Δ = −0.46 against a like-for-like control. The model rung available on this
host does not pay, so the gap must be attacked through the prompt contract.
Targets insight (−15.20) and readability (−11.93), note `bdf94683`.

**What does NOT change when it is on.** The citation contract, and therefore
the gate. Grades are advisory prose inside the prompt. They do not alter which
chunks are in the window, which `ev-N` handles exist, what the audit locates,
the corroboration floor, the custody veto or the containment witness. The
honesty floor is likewise untouched: the ported inference clause still
requires an inference to rest on cited evidence and to read visibly as
reasoning rather than as a sourced fact.

**Two floors that keep the grade from lying.** The bands straddle
`DEFAULT_USEFULNESS` (50), so a finding whose worker declined to score it
lands in `SUPPORT` and is never read as a judgement in either direction
(§18.3 — an absent value is not a verdict). And a section holding fewer than
three passages grades every one `ANCHOR`, because a naive top-third rule would
mark a thin section's only evidence `WEAK` and the contract would then
instruct the writer not to build on anything it has. Both are watched red in
`synthesize::tests`.

**Reversal condition.** Flip default-ON only on a pre-registered arm of n ≥ 5
per side on the task-69 bed whose mean exceeds the like-for-like control by
more than the measured band, with insight or readability moving in the
direction claimed. Revert on any arm that costs score beyond the band, or on
any observed rise in audit refusals — the grade must never become a licence to
assert. Per-draw sd is 2.97 and the judge contributes **zero** of it (note
`403a218a`), so n is the only lever on resolution: n=5 resolves ~2.7 points.

**Evidence at landing.** Compile + `deep_research::` 217/217 green, parallel,
three consecutive runs. No flight has been run with this flag on — it is
landed dark and UNMEASURED, and no claim about its effect may be made until
that arm exists.

## `SOVEREIGN_DR_RESEARCH_NOTES` — the researcher worker (drb1-r4)

**Landed 2026-08-24, DEFAULT OFF.** Campaign `drb1-race`. Requires
`SOVEREIGN_DR_COMPOSED_REPORT=1`: notes feed the composer's sections, so with
the composed report off there is no writer to feed and this flag does nothing.

**What it turns on.** One researcher worker per planned sub-question reads the
WHOLE merged evidence window and returns structured findings — a claim, the
`ev-N` window chunks it rests on, and a 0-100 usefulness judgment. The
composer then writes each section from that sub-question's findings instead of
from the top-`SECTION_PASSAGES` passages by cosine.

**Why.** Our AIQ teardown (`research/deep-research/aiq-teardown.md` §1.3)
attributes that system's DRB-II InfoRecall lead — 49.23, above o3 (39.98),
Gemini-3-Pro (39.09) and Grok (33.52) — to exactly this: a dedicated worker per
sub-question returning research notes, with the writer reading notes and
holding no search tools. The teardown's own read is that "the ranking is the
InfoRecall ranking" and that AIQ's lead is "a breadth result, not a scorer
trick". Ours showed each section eight passages. On the logged task-69 flight
(`dr-1787604870`) the window held 38 chunks, so most of what acquisition paid
for never reached the writer.

**What does NOT change: the citation contract, and therefore the gate.** A
finding names the `ev-N` handles it rests on, the writer cites those same
handles, and `audit` locates spans exactly as before — the corroboration floor
still counts origins and the custody veto still sees a chunk. Distillation
moves WHERE the reading happens, not what a citation means.

**The refusal that carries the safety case.** A finding citing an id the
window does not hold is refused WHOLE and recorded with its reason and the
unknown ids. It is never admitted with the bad id dropped: that would leave a
true-looking claim re-attributed to a chunk which never supported it. Watched
red — `deep_research::notes::tests::a_finding_citing_an_unknown_chunk_is_refused`
and `..::a_partly_resolvable_finding_is_refused_whole_never_re_attributed` both
fail under exactly that wrong fix, the first printing the surviving
`Finding { claim: "…", evidence_ids: ["ev-1"] }`. An uncited claim is refused
too. A sub-question whose findings are ALL refused falls back to passages,
named in the trace — never composed from nothing.

**Cost.** One extra `Speed::Slow` draft call per sub-question per run. On the
slow slot deliberately: this leg's output is what the writer reads instead of
the evidence, so a fabricated finding here becomes a cited sentence the audit
cannot locate.

**Reversal condition.** The flag flips on only if, on the shipping Rust path
with both arms otherwise identical, the notes arm beats the passage arm on
RACE overall with the honesty floor intact (0.0 ungrounded on P4-v0 and R-12)
and with the refused-finding count reported alongside the score. A win bought
by refusing so little that unverifiable claims reach the writer is a failed
arm, not a win. **Not yet measured — no arm has been flown.**

## `SOVEREIGN_DR_COMPOSED_REPORT` — the composed deep-research deliverable (drb1-t5)

**Shipped 2026-08-22, DEFAULT OFF.** Campaign `drb1-race`, order `drb1-t5`,
pre-registered in `research/deep-research/adversarial/pre-registration.md`.

**What it turns on.** The deliverable is composed — one section per planned
sub-question, retrieved per section over the whole merged evidence window by
embedding, plus a closing synthesis — rather than rebuilt from atomised,
individually-audited claim rows.

**Why.** Measured on the logged t7a flight (`runs-t7a-graded-shadow`, the
benchmark's own RACE criteria, 27B judge): the ledger shape scored a weighted
mean of **2.16/10** against the reference's **9.32**, and `## Findings` was
empty or near-empty on all nine deliverables because 127 of 137 claims landed
could-not-judge. The reference class that scores 40.46 runs ~2,200 words over
six to eight sections.

**What does not change.** The gate. The audit runs over the composed text, so
the audited artefact and the delivered artefact are the same document. The
corroboration floor, custody veto, containment witness and verdict set are
untouched; refuted claims are marked in place; unverified claims are named in a
closing Verification section rather than dropped.

**Correction 2026-08-23.** This row previously read "The reference class that
scores 40.46 runs ~2,200 words over six to eight sections." That conflated two
different articles. 2,206 words is **Perplexity's** mean over the DRB-I subset
— the competitor scoring 40.46. The benchmark's REFERENCE articles, which form
the denominator in `overall = T/(T+R)`, run **6,898-13,348 words** (measured
from `deep_research_bench/data/test_data/raw_data/reference.jsonl`). The
mistake is load-bearing: 6-8 sections x the 300-380-word budget in
`synthesize.rs` reproduces "~2,200" exactly, so the sentence very likely
authored the constant. Nothing about the flag's on/off state changes; the
sizing rationale does.

**Reversal condition.** If the composed arm does not beat the ledger arm on the
graded-probe composite with the honesty floor intact (0.0 ungrounded on P4-v0
and R-12), it stays off and the finding is reported with the curves.

**Correction 2026-08-24 — the evidence this row leaned on is contaminated, and
the flag STAYS OFF.** The 44.40 composite was measured by
`research/deep-research/arms/lab/compose2.py`, a Python REIMPLEMENTATION of
`compose_report` — and over estates that contain, as evidence, the DRB-I
reference article for the task being answered. Six of the ten subset estates
carry their own answer (5.8%-16.2% of evidence by volume; tasks 56, 58, 59, 62,
65, 69). `demo13/build-ceiling-deck.py` declares that caveat correctly for its
own arm; `arms/lab/build_estate.py` pooled those windows into the lab estate
without carrying it forward. Full accounting in
`research/deep-research/adversarial/pre-registration.md` §"CORRECTION 2026-08-24".

**What the shipping code actually scores.** First measurement of the Rust path
against this bar (task 69, pinned greedy judge, same estate/binary/window both
arms): composed **28.4100** vs ledger **27.2127** — composed wins all four RACE
dimensions, +1.20 overall, insight widest at +11% relative. Perplexity on the
same task replays at 43.8759.

**Why that is not yet a flip.** The direction the row asks for holds, and the
honesty floor is untouched (the audit still runs over the composed text; the
corroboration floor, custody veto and containment witness are unchanged). But
n=1 task is a direction, not the composite the row names. The flip wants the
four CLEAN subset tasks (78, 83, 90, 95) measured on the shipping path, both
arms. At the composed arm's current cost — 68 minutes per run, 93% of it 27B
compose+audit — that is ~6 hours of local inference, which is why the
throughput work gates the decision rather than the other way round.

**The larger finding, recorded because it reprioritises this row.** That
composed run's entire evidence window was 4 chunks / 1,866 chars / ~466 tokens,
two of which were an Amazon job posting. A 5,773-word report was composed from
~1,185 chars of relevant evidence, which is why 142 of 190 claims landed
could-not-judge. The deliverable SHAPE is no longer the binding constraint on
this score; acquisition is.
