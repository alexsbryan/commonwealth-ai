# E-wall-time on the landed set — two arms, 2026-08-14

Build: `cc19f26e` = D0 + D1a + land A + land B. Land C is parked on branch
`wip/land-c-blocked-on-tau` and is NOT in this measurement.

Quiet box cleared by the seat. Fresh conversation per turn via the bridge driver.
Arm 1 turn 0 (130.8s) was the first turn after an app relaunch and is EXCLUDED as
warmup. Arm 2 needed none — primary was resident and the app had already served 6
turns. Primary RESIDENT for both arms (loaded 23:25:40Z, no unload since), unlike
the B and C harvest arms which started cold; cross-arm absolute walls are therefore
not directly comparable.

## Walls

| arm | n | walls (s) | median | p90 |
|---|---|---|---|---|
| arm 1 warm | 5 | 80.0, 142.0, 140.5, 101.1, 103.1 | 103.1 | 141.4 |
| arm 2 | 6 | 71.8, 87.9, 111.6, 91.5, 66.0, 99.9 | 89.7 | 105.8 |
| **combined** | **11** | | **99.9** | **140.5** |
| before (20260813) | 5 | 68.7, 118.4, 62.1, 122.6, 149.8 | 118.4 | 138.9 |

**Bar: median <=75s AND p90 <=90s -> BOTH MISS** (median 99.9, p90 140.5).
Combined vs before: median -18.5s, p90 +1.6s.
The before-set PREDATES D0/A/B, so any delta attributes to the whole landed set and
to nothing in particular within it. n=11 vs n=5; one run is not a measurement.

## Clean/rewrite composition — COULD-NOT-JUDGE

| draw set | clean | rewrite | clean rate |
|---|---|---|---|
| arm 1 | 0 | 6 | 0% |
| arm 2 | 2 | 4 | 33% |
| **combined post-B** | **2** | **10** | **17%** |
| pre-B app draws | 3 | 3 | 50% |

(pre-B = 2 clean of the 5 before-set turns + the operator holdout's 1 clean.)

**Fisher exact, two-tailed: p = 0.268.** Not significant at 0.05.

Arm 1 drew zero clean turns and arm 2 drew two, so clean turns are NOT gone and
arm 1's zero was substantially an unlucky draw. But the combined rate is a third of
the pre-B rate. At these n the test cannot distinguish that from chance, so this is
recorded as COULD-NOT-JUDGE rather than as either a finding or an all-clear.
Settling it needs ~20 post-B turns; that folds into the next order's baseline draw,
where the same turns also collect drafter fabrication specimens.

## Per-turn gate actions

| ts (UTC) | action | gate_s | path |
|---|---|---|---|
| 00:25:07 | rewrite_annotated | 80.9 | rewrite |
| 00:26:26 | rewrite_released | 42.9 | rewrite |
| 00:28:49 | rewrite_annotated | 76.2 | rewrite |
| 00:31:09 | rewrite_released | 93.8 | rewrite |
| 00:32:50 | rewrite_annotated | 54.3 | rewrite |
| 00:34:33 | rewrite_annotated | 56.7 | rewrite |
| 00:41:48 | released | 31.8 | CLEAN |
| 00:43:16 | rewrite_released | 47.2 | rewrite |
| 00:45:07 | rewrite_annotated | 69.6 | rewrite |
| 00:46:39 | rewrite_annotated | 48.4 | rewrite |
| 00:47:45 | released | 25.1 | CLEAN |
| 00:49:25 | rewrite_annotated | 53.9 | rewrite |

## What is actually failing: the named-attribution family

26 failed claims across the 12 post-B turns (12 per_claim_judge, 14 specifics_scan).

**15 of 26 (58%) carry a proper-name attribution**, and for the
calibrated per-claim judge it is 10 of 12.

These fire at vp 0.98-1.00 against tau=0.9 — they are not marginal calls, and the
judge is right about them. Each one buys a rewrite, which is the mechanism keeping
the rewrite path lit. It is upstream of every latency lever in this order, and it
answers the composition question from the other end: **if the drafter mints a false
name on most turns, a low clean rate is the expected steady state and land B may
have little to do with it.**

Specimens (per-claim judge; `**RECURRING**` = also recorded by the 2026-08-13
forensics arm, note 221b3b71):

- `vp=1.000` C.D. Broad argued for Hard Incompatibilism by rejecting both compatibilist conditional analyses and viable forms of libertarian indeterminism.
- `vp=1.000` Robert Kane is cited as a compatibilist who bridges sides regarding the ability to do otherwise.
- `vp=1.000` C.D. Broad and Derk Pereboom are identified as Hard Incompatibilists who argue free will is impossible regardless of whether the universe is determini
- `vp=1.000` Jonathan Edwards provided classical compatibilist responses regarding whether divine foreknowledge acts as a causal force compatible with libertarian 
- `vp=0.999` John Martin Fischer and Paul Russell are modern compatibilists who argue for reasons-responsiveness.
- `vp=0.998` **RECURRING** Van Inwagen’s "No Forking Paths" thought experiment illustrates how agents might lack alternative possibilities if everything is causally predetermine
- `vp=0.998` David Hume and Thomas Hobbes argued that the ability to do otherwise should be analyzed conditionally.
- `vp=0.989` Robert Kane, Timothy O'Connor, T.L. Haji, Hector-Miguel Mele, Roderick Chisholm, and Derk Pereboom developed models like event-causal, noncausal, and 
- `vp=0.987` C.D. Broad represents a pessimist version of metaphysical libertarianism by rejecting deterministic necessity while finding no viable indeterminism fo
- `vp=0.984` **RECURRING** William James coined the term "hard determinism" to describe a fatalistic and binding view of necessity.
- `vp=0.978` **RECURRING** Peter van Inwagen proposed the No Forking Paths argument against incompatibilism.
- `vp=0.966` C.D. Broad represented a skeptical variant arguing no viable form of determinism supports genuine freedom.

The van Inwagen "No Forking Paths" and William James coinage specimens are the same
ones the 2026-08-13 forensics arm recorded (note 221b3b71) — still recurring on this
build. This is the evidence base for the drafter-side attribution-discipline order.
