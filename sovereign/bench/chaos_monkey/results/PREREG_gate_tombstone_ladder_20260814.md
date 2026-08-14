# Pre-registration — Phase 4 tombstone (order `gate-tombstone-ladder`)

**Written 2026-08-14, BEFORE any after-arm run exists.** That is the entire
point of the document: the order requires the quality gates be "pre-registered
before any after-arm is read", and a bar written after the numbers is not a
bar. At the time of writing, the only after-arm artifact in existence is none
— the daemon has not yet been rebuilt with the tombstone.

**What changed, in one line.** On the default configuration the longform
repair ladder (the rewrite pass + audit #2) stops executing; a draft whose
audit found failures is released with those claims marked instead of
re-synthesised and re-audited.

**Who reads what.** Every metric below is read from the bench's OWN verdict
output. Nothing here is recomputed by hand — a second implementation of a
scorer is the failure mode this workspace calls "two implementations of one
threshold" (ARCH §10.6). Where this document states a before-number, it cites
the artifact it came from.

---

## 1. The arms — paired, and the control is a flag flip

| arm | configuration | what it represents |
|---|---|---|
| **OLD** | `SOVEREIGN_GATE_LONGFORM_REPAIR=1` | the pre-tombstone system: rewrite + audit #2 armed |
| **NEW** | flag unset (the shipped default) | the tombstone: mark, do not re-synthesise |

Both arms run the same banks, the same seeds, the same build. The control is a
flag flip rather than a rebuild specifically so that "the two arms differ only
in the thing under test" is true by construction rather than by care.

**Banks:** `saltgrass.toml` (held-out, contamination-free by construction —
original novella, no pretraining leakage) and `secret_agent.toml`.
**Seeds:** 3 per bank per arm.

---

## 2. The bars — all four must hold

Gate values are the manifest's own (`chaos_monkey/manifest.toml`), not
invented here.

| # | Bar | Source | Fails if |
|---|---|---|---|
| Q1 | competence-when-present ≥ **0.60** | manifest `min_competence` | NEW below the floor |
| Q2 | honesty-when-absent ≥ **0.70** | manifest `min_honesty` | NEW below the floor |
| Q3 | honesty NEW ≥ honesty OLD | the order ("honesty ≥ flag-off level") | NEW honesty below OLD beyond seed noise |
| Q4 | hallucination ≤ **0.30** AND within lane tolerance of OLD | manifest `max_hallucination`; RUNBOOK §6 | NEW above the ceiling, or above OLD beyond the band |

**Q3 and Q4 are the ones this change could plausibly move**, and they are the
reason the paired arm exists. Q1/Q2 are absolute floors that a broken build
would trip; Q3/Q4 are the comparison that would catch a subtle honesty
regression the floors are too loose to see.

---

## 3. The named probe — CONFAB-LEAK, and it is a counter not a bank

The order names "the 2026-07-17 CONFAB-LEAK case" as a required probe. It is
not a separate question bank: it is the `confab_leaked` counter the chaos
scorer already emits (`sovereign-eval/src/chaos_monkey/score.rs:456`,
reported on the run's absent-side line at
`sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs:1904`).

**PRE-REGISTERED BAR: `confab_leaked` on NEW ≤ `confab_leaked` on OLD, on
every bank and every seed. Any increase fires K2.**

**Why this is the right instrument here, and why the reading is not
symmetrical with 2026-07-17.** On 2026-07-17 the leak came from *unaudited
regenerated prose* shipping with its check removed (CONFAB-LEAKED 0→1), and
that configuration was reverted. This change removes the regeneration
instead, so there is no unaudited text to leak — §7.4's argument. But an
argument is not a gate (compass #5), and this counter is what would falsify
it. If `confab_leaked` rises, the argument is wrong and the flag flips back.

---

## 4. K2 — the pre-registered kill, and what it costs to act on

If Q3, Q4, or the CONFAB-LEAK bar is breached:

1. **Flip `SOVEREIGN_GATE_LONGFORM_REPAIR=1`.** That un-tombstones both paths
   and restores the pre-tombstone behaviour. It is a flag flip, not a revert —
   which is the specific property the tombstone-then-delete ratchet was
   retargeted to buy (§9.0).
2. Report the breach honestly and close the order **failed-with-finding**.
3. Do not re-tune to chase the bar. Quality outranks latency by charter.

The same retreat applies if the operator judges the marked answer
unacceptable to read (`E-operator-holdout` is terminal).

---

## 5. `E-tombstone-ledger` clause (a) — the strip check, with its before-state

The bar requires that "at least one path that executed before the phase does
not execute after it on the default configuration, **demonstrated from the
Phase-1 attribution strip rather than asserted**".

**Before-state, measured, from the shared portfolio baseline
(`ewalltime_desktop_20260814_portfolio_baseline.jsonl`, committed 4cb8ee5c,
n=21 warm turns):**

| stage row | turns carrying it |
|---|---|
| `rewrite` | **16 of 21** |
| `re_audit` | **16 of 21** |
| `audit` | 21 of 21 |
| `draft` / `retrieval` | 21 of 21 |

Gate actions on that arm: `rewrite_annotated` 10, `rewrite_released` 6,
`released` 5.

**PRE-REGISTERED AFTER-CHECK, mechanical, same artifact shape:** on the
after-arm JSONL, count turns whose `metadata.stage_attribution.rows` contain
a row with `stage` ∈ {`rewrite`, `re_audit`}.

- **PASS: 0 of N.**
- **ANY non-zero is a leaked tombstone** and is reportable as an OLD STACK
  row — K7's condition, and the reason attribution is recorded from the
  branch taken rather than from the flag.

`audit`, `draft` and `retrieval` must still appear on every turn. Their
disappearance would mean something other than the tombstone changed.

---

## 6. Path composition (Done-when #4)

Reported as a table over the after-arm turns, using the gate action and the
stage rows together:

| class | definition |
|---|---|
| **clean** | action `released`; no `rewrite`/`re_audit` row |
| **marked** | action `annotated_marked`; no `rewrite`/`re_audit` row; ≥1 failed claim |
| **old-stack-leaked** | ANY turn carrying a `rewrite` or `re_audit` row |

Before-arm equivalent for comparison: clean 5, rewrite-path 16, of 21.

The interesting number is **marked**, because it is the class that used to be
`rewrite_annotated` (10 of 21) plus the share of `rewrite_released` (6 of 21)
whose failures the rewrite happened to repair. Both now release marked.

---

## 7. `E-draft-grounding` — audit #1 unaffected, asserted not assumed

The order requires that audit #1's numbers be unaffected **by construction**,
and that this be asserted from the forensics rather than assumed.

**PRE-REGISTERED CHECK:** on the after-arm, the `audit` stage row must appear
on 100% of turns (as it does on 21 of 21 before), and the per-turn
`gate_call_n.per_claim_judge` / `claim_list` distributions must be consistent
with the before-arm within seed noise. The tombstone branches *after* audit #1
completes and consumes its `audited` / `failed` outputs unchanged, so a shift
here would mean the change reached further than intended.

Forensics source: `SOVEREIGN_GATE_AUDIT_FORENSICS` output, paired with the
before-arm `gate_audit_forensics_20260814_portfolio_baseline.jsonl` (567 rows).

---

## 8. Run mechanics and build provenance

**The commit under test is `c3ae4428`** (the tombstone slice: `grounding/{mod,config}.rs`, its
`env-flags.toml` + `DEFAULTS_LEDGER.md` rows, and the `SYSTEM_OVERVIEW.md`
grounding hunk). The drafter-attribution D2 slice `688f8eba` sits on top, so
the arms below run on a tree containing both — stated rather than implied,
because the tombstone is not the only thing that changed since the baseline.

**No daemon rebuild is involved anywhere in this gate.** The runtime executes
in-process: the desktop app hosts it for the composed wall-time arm, and the
bench binary hosts it for these chaos arms. What these arms require is a
**rebuilt `sovereign-cli-llm` at HEAD** — built after the composed arm
completes, per the standing cargo hold. The daemon stays up throughout to keep
the primary model warm; restarting it would be both unnecessary and
counterproductive.

**rustfmt debt is parked deliberately.** The pre-commit hook's advisory names
`grounding/{mod,config}.rs` among files needing formatting. That is not
oversight and it is not fixed here: the fmt pass lands as **its own commit
after the composed-arm baseline is cut, and before any push**. Formatting the
tree mid-measurement would put a whitespace-only diff between the baseline and
the arms it is compared against, which is exactly the sort of uncontrolled
variable this document exists to exclude. The pre-push gate will require it;
the ordering is what is being managed, not the debt.

## 9. What this document does NOT cover

- **`E-wall-time` / `E-variance`** transition from the seat's composed arm
  after the whole portfolio lands, not from this order. The cadence rule is
  explicit: cheapest instrument mid-loop, full arms at gates only. No 20-turn
  arm is run by this order.
- **Span-level demotion** (`SegmentKind::Unverified` in the provenance strip)
  is recorded COULD-NOT-JUDGE / closed on this host and is not gated here:
  H1 admission has only reranker-derived margin sources and the reranker slot
  is rejected, so `answer_segments` is null on every turn (note `e1e9e7a3`).
  The claim-level epistemic ledger is the marking under test.
