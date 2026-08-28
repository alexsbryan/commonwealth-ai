# 枯山水 — the garden as an architectural criterion

**Status:** pre-registration. Companion to `quality/TOPOLOGY.md`. That document
asks whether the types tell the truth. This one asks what SHAPE the whole
should take, and answers it with a form that has spent six hundred years on
exactly one problem: **how do you compose a bounded field so that a person
seated at one edge can hold it?**

That is this repo's problem, stated in another discipline. 1,024,868 lines of
first-party Rust across 54 crates, a nominal configuration space TOPOLOGY §2
measured at ≈4.6×10¹⁸, and a documented cost of ~15 non-obvious facts an agent
must hold to make a correct top-level change.

**This is not decoration.** Every principle below is stated as a property you
can measure, is measured against this tree as of 2026-08-27, and carries the
check that falsifies it. Render the current state with `scripts/garden.py`.
Where the metaphor fails, §10 says so rather than stretching it.

---

## 1. 枯 KARE — the gravel is dry, and that is the founding problem

A karesansui represents water with raked stone. The pattern is exact,
beautiful, and there is no water in it. **The failure mode of this repo is that
its documentation is samon**: a precise rendering of a flow that lives
elsewhere, maintained by hand, and true only until the tree moves.

| Measured | |
|---|---:|
| Markdown lines (first-party) | 198,464 |
| Rust comment lines | 222,903 |
| Structural assertion — files carrying a test | 1,380 of 2,004 |
| `TARGET_ARCHITECTURE.md`'s five declared profiles, occurrences in the tree | **0** |
| Files naming `commonwealth-daemon`, deleted 2026-08-24 | 21 |
| Narrative docs whose invariants no longer hold (TOPOLOGY's own count) | 7 |

**The rearrangement.** ARCH §7.2 already names the smell — *"an assertion in
English prose rather than in a test."* Nothing counts them. Every normative
claim in a narrative doc either becomes an executable assertion or is deleted;
prose keeps the *why*, never the *what*.

**Falsifier.** A census of normative claims (`must`, `always`, `never`, `is
the only`) in the four gated docs, each resolved to the test or gate that
holds it. The number with no holder is the debt, and it only shrinks.

---

## 2. 石 ISHI — few stones, and small ones

Stones are placed once and never moved. In a karesansui they are also *small
relative to the field* — the field is what you are looking at.

| Measured | |
|---|---:|
| Layer 0 (`contract`) as a share of first-party Rust | **3.4%** |
| `sovereign-contracts` as a share of layer-0 mass | **66.2%** (23,317 of 35,243) |
| Remaining six layer-0 crates | 46 – 4,933 lines |

**Half of this already holds and half of it is inverted.** At the field level
the stones are correctly small — 3.4% is a kernel, not a monolith. At the group
level one stone is two thirds of the mass, and `TARGET_ARCHITECTURE §10.8`
already names the failure by name: *"contracts becomes the megablock."*

**The rearrangement.** Split `sovereign-contracts` along the seams it already
has internally. Target: **no single layer-0 crate exceeds 30% of layer-0
mass.** Not a size limit — a *composition* limit. One stone dominating the
group is the arrangement a gardener would reject on sight.

**Falsifier.** `scripts/garden.py` prints the share. It is one number.

---

## 3. 間 MA — the void is the composition, not the leftover

The empty gravel is not the space between stones. It is the subject. In a
system the analog is exact: **the states the types cannot represent are the
design; the states they permit are what is left over.**

| Measured | |
|---|---:|
| States recorded as made unrepresentable (TOPOLOGY §10) | **9** |
| Independent `Option` fields across the four composition roots | 62 |
| Nominal configurations those admit | ≈4.6×10¹⁸ |

**The rearrangement.** TOPOLOGY already requires every phase to name the state
it makes unrepresentable, and calls a phase that cannot name one *scaffolding*.
Extend it from a per-phase rule to a **standing count**: the headline number
for architectural health is states-forbidden, not lines-of-code. LOC is what a
system weighs. Forbidden states are what it *is*.

**Falsifier.** The count only rises. A release that adds capability and
forbids nothing has added flags, not structure.

---

## 4. 簡素 KANSO — one thing, one way

Elimination of the inessential. Not minimalism for its own sake: **two ways to
do one thing is worse than one awkward way**, which `quality/HOT_PATH_REUSE.md`
already states in those words about flag surfaces.

| Measured | |
|---|---:|
| `SOVEREIGN_*` environment reads | 249 |
| …of those, unregistered | 157 |
| …riding the shrink-only baseline | 149 |
| `concept-gate` verdict today | **PASS**, delta −1 (fixed 2026-08-27) |
| Competing arg-parsing convergence targets | 2 (`args.rs` 33 adopters, `flag_surface.rs` 4) |

**A gate that cannot judge is gravel raked into a pattern nobody checks.**
`concept-gate` was the instrument for this principle and was blind on every
host — the cause was NOT a stale sibling, which is what its own error text
claimed. The arm read `body["freshness"]`; `converge status --json` has always
published `graph_lag`, and `freshness` is what the sibling `redirect` command
spells the same `lag.verdict_word()` as. So the gate could never reach a
verdict against any binary, ever, and sent every reader to a two-minute rebuild
that could not fix it. Two names for one concept — §10.6's own smell — inside
the gate that exists to catch exactly that.

**The rearrangement.** DONE 2026-08-27: the relay reads the key the relayed
command publishes, watched failing and then passing before the line changed.
It STAYS `Advisory` in `cargo xtask quality` and the promotion in the original
draft of this row was wrong: the count is derived from the graph at the last
indexed commit, not the working tree being gated, so a habit-run would go red
for an indexer minutes behind. It is already `Hard` where the graph is
authoritative — CI and every landing verdict call `svrn code converge status`
and gate on its exit. Still open: retire one of the two flag surfaces before
converging anything onto either — otherwise the convergence produces a third.

**Falsifier.** `concept-gate` returns PASS or FAIL, never COULD-NOT-JUDGE, on
a clean tree. Holds as of 2026-08-27.

---

## 5. 静寂 SEIJAKU — at rest, silence

A garden at rest emits nothing. Every signal it does emit means something.

| Standing signal | Count | Age |
|---|---:|---|
| `svrn posture` rows not passed | 4 of 7 | up to **102d** |
| Enforcing xtask gates failing | ~~6~~ **1** of 8 (arch-gate) | 2026-08-27 |
| Oversized-file baseline rows | 137 | — |
| Env vars riding the baseline | 149 | — |
| Clippy warnings, full workspace | 573 | — |

**This is not vigilance, it is alarm fatigue.** A row yellow for 102 days does
not warn; it teaches every reader that yellow means nothing, which costs more
than having no row. The 2026-08-26 session found the auto-collaborate loop dark
since July — under a control plane that kept reporting healthy.

**The rearrangement.** A signal standing unactioned past its review-by is
**deleted or fixed, never renewed**. `sovereign/DEFAULTS_LEDGER.md` already
encodes this contract for dark capabilities ("a row past its review-by date is
not noise: it is the signal"). Apply the same rule to every standing counter.

**Falsifier.** Count of signals older than their review-by. Target zero, and
zero reached by *fixing or deleting*, never by re-dating.

---

## 6. 枯淡 KOKO — age shows as bareness

A garden that has been tended for thirty years has *less* in it than one
tended for three. Weathering removes.

| Measured, 90 days | added | deleted | shed per added |
|---|---:|---:|---:|
| First-party Rust | 619,150 | 173,888 | **0.28** |
| Process surface | 41,477 | 7,253 | **0.17** |

*為學日益，為道日損* — "in the pursuit of learning, daily accretion; in the
pursuit of the way, daily letting go." The del:add column is literally that
line, and this system accretes in both registers — the governance faster than
the code it governs.

**The rearrangement.** Landed 2026-08-27: a shrink-only byte ratchet on the
instruction surface (it had regrown 30,010 → 42,142 in nineteen days after a
slimming order shipped with no ratchet), and seven previously-unrun gates wired
into pre-push. `quality/DELETION.md` carries the rest.

**Falsifier.** `scripts/deletion-manifest.py --verify`, and the arch-gate
instruction-surface row. Both fail on growth.

---

## 7. 不均整 FUKINSEI — asymmetry that follows from role

Never a grid, never centred, odd groupings. But irregularity is *composed*, not
merely permitted — the distinction between asymmetry and sprawl is whether the
shape follows from what the element is.

Crate mass runs 46 (`sovereign-time`) to 77,728 (`sovereign-mesh`). That range
is correct: a leaf that carries one trait *should* be tiny. What is not
composed is 137 oversized-file rows, `notes.rs` at 7,794 lines and +1,328 past
its own baseline, `state.rs` at 2,500.

**The rearrangement.** Keep the crate asymmetry; it is the design. Treat the
file-size baseline as sprawl and burn it down. **The test is whether the size
follows from the role.** A leaf crate of 46 lines passes. A 7,794-line file
inside a carved-out leaf does not.

**But burning it down is not enough on its own, and this repo already has the
counter-evidence.** Measured 2026-08-27 against the sizes ARCH §3.3 records for
files this tree has ALREADY decomposed:

| file | §3.3 | now | |
|---|---:|---:|---:|
| `runtime/streaming.rs` | 1,950 | 4,596 | **2.36x** |
| `embedded/model_slot.rs` | 3,475 | 5,727 | 1.65x |
| `corpus-engine-notes/notes.rs` | 5,634 | 7,795 | 1.38x |
| `runtime/prompts.rs` | 733 | 1,058 | 1.44x |
| `desktop/state.rs` | 1,430 | 1,824 | 1.28x |
| `runtime/turn.rs` | 680 | 849 | 1.25x |
| `daemon_cmd/mod.rs` | 2,378 | 1,372 | **0.58x** |
| total | 16,280 | 23,221 | **1.43x** |

Every split refilled but one, and `prompts.rs` and `turn.rs` — PRODUCTS of the
June `runtime.rs` decomposition — have climbed back into the 800–1200 band
themselves. A split with no ratchet is a one-time payment on a recurring bill.

**The exception is the whole lesson.** `daemon_cmd/mod.rs` is the only file that
SHRANK, and it is the only one whose §3.3 entry declares an accepted END STATE
("the remaining ~22 phases are interleaved and stay inline"). Splits that named
their terminal shape held; splits that only cut lines refilled, because nothing
told the next agent where new code belonged and the nearest file always wins.

**The cheap half landed 2026-08-27**: arch-gate now ratchets ARCH §3.1's
800–1200 approach band (162 files / 157,647 lines, counter ratchet, no slack).
The >1200 baseline froze the TAIL and said nothing about the APPROACH, so a file
travelled 400 → 1,199 unwatched and then failed as a "NEW oversized file",
blaming whoever wrote line 1,201. Both arms were watched failing before the
ratchet was trusted: growth inside the band fails the band, growth across 1,200
fails the oversized baseline, and the handoff is covered because the two run in
the same pass. **The expensive half is not automatable and is not done** — a
split must state what each module is FOR, or the ratchet only slows the refill.

---

## 8. 借景 SHAKKEI — framed, not owned

Borrowed scenery is the mountain beyond the wall. You compose *with* it. You do
not own it, cannot change it, and it moves without you.

`vendor/` is **783,386 lines** inside the wall. That is not borrowing a
mountain; it is hauling it into the garden.

**This is an open question, not a finding.** Vendoring `llama-cpp-4` is a
defensible reproducibility decision and its pin has caught real breakage. But
it should be named as annexation rather than framing, and the cost stated:
every census, every grep, every clone carries it. **Operator call, not an
agent's.**

---

## 9. 作務 SAMU — the raking is the practice, not the overhead

The gardener rakes daily. It is not maintenance *of* the practice; it *is* the
practice.

Measured 2026-08-27: nine of ten xtask gates had no caller — `ci.yml`'s gates
job has been commented out since the commit that wrote it (deliberately;
`docs/CI_ECONOMY.md` argues the real gate is local), `pre-push.sh` ran only
`docs-gate`, and the definition of done named neither. `docs-gate` itself was
failing, so every push was blocked and nobody had said so.

Wired the same day. **What is left is the distinction the ledger already
drew:** the operator rejected run-if-stale automation on 2026-08-08 in favour
of the seat ritual. That decision stands, and the garden honours it — it
reports gravel age and refuses only the moving of stones.

---

## 10. Where the transposition FAILS — read this before extending it

A form borrowed without its failures is decoration.

1. **A garden does not grow.** Its wall bounds it permanently. This system
   added 1.07M lines to `research/` alone in 90 days. Karesansui has no
   vocabulary for that, and the honest response is the ratchet, not the
   metaphor.
2. **Wabi-sabi accepts decay; a ratchet refuses it.** These are opposed. The
   split this repo needs: be wabi-sabi about *prose* — it ages, let it — and
   never about *invariants*.
3. **A garden has one gardener.** This has agents across six machines and a
   102-day-stale rake. Shared tending is a genuinely different problem and the
   form does not solve it.
4. **龍安寺's fifteenth stone is in tension with TOPOLOGY's thesis.** Ryōan-ji
   seats fifteen so that fourteen are visible from anywhere: the lesson is that
   completeness is *not viewable*, and knowing so is the point. TOPOLOGY wants
   the opposite — an allowlist with a cardinality proof, a whole you *can*
   hold. **Both are right about different layers.** The state space should be
   small enough to enumerate (TOPOLOGY). The system as a lived artifact never
   will be, and a view claiming otherwise is the one to distrust. The renderer
   resolves it the only honest way: every panel names its own blind spot.

---

## 11. Sequencing

Ordered by what each buys per unit of disturbance. No phase is funded that
cannot name the state it forbids or the number it moves.

| # | Move | The number it moves | Risk |
|---|---|---|---|
| 1 | Retire or fix every signal past its review-by (§5) | 4 posture rows → 0 stale | none — deletion |
| 2 | ~~Make `concept-gate` judge~~ **DONE** — promotion refused with cause (§4) | COULD-NOT-JUDGE → PASS | low |
| 3 | Burn the failing gates to green (§5, §7) | 6 of 8 → **1 of 8**; arch-gate remains | **not mechanical** — see below |
| 4 | Census normative claims without a holder (§1) | new baseline, shrink-only | low |
| 5 | Split `sovereign-contracts` (§2) | 66.2% → <30% of layer-0 mass | **high** — it is a stone |
| 6 | Adopt states-forbidden as the headline (§3) | 9 → rising | none — a reframe |
| 7 | Decide `vendor/`: framed or annexed (§8) | operator call | — |

**Move 3 was mispriced and the correction matters.** Four of the five fell to
real fixes in an afternoon (env-gate: two undeclared knobs; boundary-gate: a
budget amended in a `Cargo.toml` on 2026-08-20 that nobody taught the gate;
layout-gate: ten hand-spelled layout sites, one of them a latent bug where an
empty corpus id built the prefix `-partition-`, matching every corpus under the
root; layer-gate: five new fan-in edges, all from `27c0fe03` "Noun
convergence", where the ratchet reads convergence as regression because giving
a noun one owner necessarily raises its fan-in).

**arch-gate is not mechanical and its 84 findings are not one backlog.** The
baseline was last genuinely re-minted 2026-07-30; 79 of the findings are on
files the working tree never touched. Split by what is actually oversized:

| | |
|---|---:|
| Findings whose PRODUCTION code exceeds 1200 | **50** |
| Findings under 1200 in production, over only with their test block | **34** |

The second group is an instrument artifact, and the tempting fix is the wrong
one. §3.1 calls the line count "a proxy for concern count," and §3.2's split
pattern says in its own words *"Move the tests with the code."* Sweeping every
`#[cfg(test)]` block into a sibling file would clear 25 findings without
touching a single concern — raked gravel. The 50 are the real debt, and the
first of them is burned: `doctor_cmd.rs` (3042) → seven files along the three
layers its own module doc had always declared, largest 1048, identical function
set, same four tests, 191 passing.

**Move 5 is the one to be slow about.** Moving a stone is the single thing a
karesansui forbids, and layer 0 is where every other layer's meaning is
anchored. It is right, and it is not urgent, and it should happen after 1–4
have made the field quiet enough to see what you are doing.
