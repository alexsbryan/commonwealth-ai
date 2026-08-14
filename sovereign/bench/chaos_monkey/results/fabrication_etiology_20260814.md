# Fabrication etiology — the named-attribution failure class, from recorded forensics

D0 of the drafter-attribution-discipline order, 2026-08-14. Classification of
EVERY named-attribution failure on record against the recorded evidence window
the drafter actually saw. No new runs; byte-faithful windows from the
`SOVEREIGN_GATE_AUDIT_FORENSICS` ledgers.

## Sources (all failed-claim records, n=33; named-attribution n=23)

| ledger | build | turns/passes | failed claims | named |
|---|---|---|---|---|
| `gate_audit_forensics_20260814_landed.jsonl` (this dir) | cc19f26e post-B | 12 turns / 22 passes | 26 | 18 |
| `gate_audit_forensics_20260813_arm2.jsonl` (this dir; was session-53a08260 scratchpad, note 221b3b71) | post-§7.8 repair | 7 passes | 3 | 3 |
| `gate_audit_forensics_20260813_d0pre.jsonl` (this dir; was session-9ec04cf0 scratchpad) | pre-land-B | 5 passes | 4 | 2 |
| saltgrass harvest transcripts (8 artifacts, 258 rows swept) | land B | — | 44 failing rows | **0** |

The harvest arm contributes zero specimens: its failures are extraction-fidelity
misses on the fiction corpus (short factual QA), not attribution minting. The
etiology below is a long-form phenomenon.

All three forensics ledgers record the same evidence universe per turn:
28 leaf chunks (27,847 chars) + 8 RAPTOR summary chunks (3,712 chars), constant
across every audited turn (same question, warm cache). The drafter saw all 36;
per-claim judges saw 28 or 36 by claim class (see Mechanism 2).

## Taxonomy (from the order, plus one class the evidence forced)

- (i) position stated in the window; drafter GARBLED delivered evidence
- (ii) entity present, position absent — the pool invited a stitch
- (iii) entity absent entirely — parametric padding
- (iv) position present ONLY in a Summary chunk — carried faithfully by the
  drafter, inadmissible to the judge (the RAPTOR quote_spans carriage gap)
- (v) position present past a presentation boundary (ordering/dedup/confusor)
- (vi) claim SUPPORTED in the mechanism's own view, failed anyway — judge/scan
  false positive (not in the order's taxonomy; the specimens forced it)

## Distribution (primary class, 23 named-attribution specimens)

| class | n | share | specimens |
|---|---|---|---|
| (iv) summary-carriage | 9 | 39% | 04, 06, 09, 10, 19, 22, 26, 28, 32 |
| (i) garble of delivered evidence | 6 | 26% | 00, 03, 16, 25, 27, 31 |
| (ii) stitch (entity present, position absent) | 5 | 22% | 02, 05, 14, 15, 20 |
| (vi) judge/scan false positive | 2 | 9% | 07, 21 |
| (iii) parametric padding | 1 | 4% | 11 |

**20 of 23 (87%) had the claim's entities AND substantive content delivered in
the drafter's window.** Pure parametric recall is 1/23 — far below the order's
60% not-worth-continuing bar. Curation is licensed; decode-time constraint is
not the indicated lever.

Class (v) as primary: 0. As secondary signal: specimen 05 (sole support for the
Hume half at leaf[27] of 28 — last rank).

## Specimen table

vp is the calibrated per-claim judge's violation probability (tau=0.9);
specifics_scan rows carry no vp. Chunk cites are indices into the recorded
window (L=leaf, S=summary; label in parens). Dossiers with full context
excerpts: session scratchpad `dossiers/spec_NN_*.txt`, index.json alongside.

| # | ledger | mech | vp | claim (abbrev) | class | evidence citation |
|---|---|---|---|---|---|---|
| 00 | landed14 | judge | .966 | Broad: skeptical variant, "no viable form of determinism supports freedom" | (i) | S2 says no viable INdeterminism; drafter swapped the operative term. n_shared=36, judge saw S2, right to fail |
| 02 | landed14 | judge | 1.000 | Broad & Pereboom identified as Hard Incompatibilists | (ii) | "hard incompatibilism" L0/L1/L20 with NO named proponents; Pereboom tied to hard determinism L4(broad); Broad only S2. Binding minted |
| 03 | landed14 | judge | .989 | Kane, "Timothy O'Connor", "T.L. Haji", "Hector-Miguel Mele", "Roderick Chisholm", Pereboom developed event-causal/noncausal/agent-causal models | (i)+(iii) | S7 lists surnames "Kane, Clarke, Haji, Mele, Chisholm, Pereboom" + the models; drafter minted first names (two false: Hector-Miguel Mele, T.L. Haji), swapped Clarke for O'Connor (L4) |
| 04 | landed14 | judge | 1.000 | Broad argued Hard Incompatibilism, rejecting conditional analyses + libertarian indeterminism | (iv) | Position ≈ S2 near-verbatim; 0 leaf hits for "Broad". n_shared=28 (classified factual → leaf-only) → support invisible to judge BY POLICY |
| 05 | landed14 | judge | .998 | Hume AND Hobbes analyzed ability-to-do-otherwise conditionally | (ii) | Hume+conditional at L27 (RANK 27/28 — class-v signal); Hobbes only at L18 with the DIFFERENT necessity-distinction position. Hobbes half stitched |
| 06 | landed14 | judge | .998 | van Inwagen's "No Forking Paths" illustrates lack of alternatives | (iv) | "Forking" appears NOWHERE in 28 leaves; ONLY S0: "thought experiments like van Inwagen's No Forking Paths". L4 correctly has "The Consequence Argument (van Inwagen)". SUMMARY-MINTED |
| 07 | landed14 | judge | .984 | James coined "hard determinism" (fatalistic, binding necessity) | (vi) | FULLY SUPPORTED at L13: "he defined the common terms hard determinism and soft determinism… did not shrink from such words as fatality, bondage of the will, necessitation". L13 was IN the judge's 28. Judge false positive; its claim-conditioned re-search ('extra') surfaced prior-answer confusor chunks |
| 09 | landed14 | scan | — | "reasons-responsiveness or Strawsonian reactive attitudes" | (iv) | S1 verbatim ("reasons-responsiveness or Strawsonian approaches"); 0 leaf hits; scan reads leaves only |
| 10 | landed14 | judge | .978 | van Inwagen proposed No Forking Paths "against incompatibilism" | (iv)+(i) | Same S0 carriage as 06, plus drafter inverted the direction (S0 argues FOR incompatibilism) |
| 11 | landed14 | scan | — | "John Maynard Keynes' contemporary, William James" | (iii) | Keynes absent from all 36 chunks — pure parametric garnish on a James sentence that L13 supports |
| 14 | landed14 | judge | 1.000 | Kane cited as a COMPATIBILIST who bridges sides | (ii) | L1 states Kane is a metaphysical LIBERTARIAN (incompatibilist); no chunk says compatibilist or "bridges". Inversion of a delivered position |
| 15 | landed14 | scan | — | "(like Robert Kane, though he bridges both sides)" | (ii) | Same turn/claim family as 14 |
| 16 | landed14 | judge | 1.000 | Jonathan Edwards: classical compatibilist responses on divine foreknowledge as causal force + libertarian free will | (i) | Mash of L17 (Edwards most famous proponent of classical-compatibilist theological determinism) + L5 (Edwards's fatalism argument vs libertarian freedom); the composite is incoherent vs both |
| 19 | landed14 | judge | .999 | Fischer & Paul Russell: modern compatibilists arguing reasons-responsiveness | (iv) | S1 VERBATIM ("Key figures such as John Martin Fischer and Paul Russell advance strategies like reasons-responsiveness"); leaves have only Fischer-1994-denial (L25) and Bertrand Russell (L8). n_shared=28 → S1 invisible. S1 itself part-minted from its leaves |
| 20 | landed14 | scan | — | Agent-causal accounts "associated with Chisholm and Pereboom" | (ii) | L4 ties agent causation to Kane/O'Connor/Clarke; S7 lists Chisholm+Pereboom only in a generic "cited in discussions" roster. Binding minted from roster adjacency |
| 21 | landed14 | scan | — | "Dennett and Stephen Wolfram have argued from cellular automata perspectives" | (vi) | VERBATIM support at L10 (cellular-automata): "both Daniel Dennett and Stephen Wolfram argued that adopting the CA perspective…". Chunk is 900 chars — inside the scan's 1500 cap. Scan false positive |
| 22 | landed14 | scan | — | "Dennett uses high-level design stances… Wolfram posits computational irreducibility" | (iv) | S5 verbatim; scan reads leaves only |
| 25 | landed14 | judge | .987 | Broad represents a pessimist version of metaphysical LIBERTARIANISM | (i) | S2 makes Broad a pessimist about free will tout court (incompatibilist + no viable indeterminism); "libertarianism" binding is the garble. n_shared=36, judge right |
| 26 | arm13 | judge | 1.000 | Broad rejected conditional analysis, requiring categorical substitutability | (iv) | S2 NEAR-VERBATIM ("rejecting conditional analyses of substitutability… insisting instead on categorical substitutability"). n_shared=28 → invisible. Note 221b3b71 called this "garbled attribution" — the window says otherwise: faithful carriage |
| 27 | arm13 | judge | .996 | James coined "bondage of the will" | (i) | L13 has the phrase inside James's CHARACTERIZATION of hard determinism ("did not shrink from such words as… bondage of the will") — drafter promoted a quoted term to a coinage |
| 28 | arm13 | judge | 1.000 | van Inwagen proposed "No Forking Paths" | (iv) | Same S0-only carriage as 06/10 |
| 31 | d0pre | judge | .988 | Dennett uses cellular automata via computational irreducibility | (i) | S5 delivers the pair correctly (Dennett=intuition pumps/design stances; WOLFRAM=computational irreducibility); drafter swapped the binding |
| 32 | d0pre | judge | .985 | Fischer & Paul Russell advanced reasons-responsiveness | (iv) | Same S1 carriage as 19 |

Unnamed failures for completeness (10 of 33): 8 specifics_scan flags on bare
concept phrases, of which FOUR are verbatim-present in leaf chunks within the
scan's own view ("hard incompatibilism" L0@208/L20@798 — flagged twice;
"soft determinism" L13@411; "hard determinist" L0/L1/L6/L9) — scan false
positives, same (vi) family as specimen 21; 2 are summary-carried phrases
("problem of luck" S6, "computational irreducibility" S5) — (iv) family.

## Fresh-draw fold-in: the D1 portfolio baseline (added 2026-08-14, post-D0)

The 20-turn shared baseline (commit 4cb8ee5c;
`gate_audit_forensics_20260814_portfolio_baseline.jsonl`, records with
ts >= 01:57Z — the file also re-carries the earlier landed-arm passes) produced
51 fresh failed claims, 33 carrying proper names, of which 30 are
person-attribution claims (3 concept-only scan flags excluded). Same question,
same 28L+8S window shape on every audited pass. Classified with the same key
(mechanical pass: `../fabrication_etiology.py`; judgment reads on the
undecided specimens):

| class | n | share | D0 share | notes |
|---|---|---|---|---|
| (iv) summary-carriage | 13 | 43% | 39% | van Inwagen NFP x8 (S0), Fischer/Russell reasons-responsiveness x3 (S1), Broad categorical substitutability x2 (S2) — all three corrupted/unverifiable summary sentences still re-delivered every turn |
| (iii) parametric | 6 | 20% | 4% | the NEW cluster: John Maynard Keynes x3, Joan Robinson, Paul Sweezy/Schrodinger, van Inwagen's "1983 book An Essay on Free Will" (true in reality, absent from window; deterministic_veto catch). The Keynes seeding is visible in the specimen "William James attributed the definition of soft determinism to John Maynard Keynes" (vp 1.000): leaf[13] has James defining soft determinism, S1 delivers "John Martin Fischer" — the "John M—" completion pulls Keynes from parametric space. Confusor-adjacent autocomplete, not free invention |
| (i) garble of delivered | 5 | 17% | 26% | Dennett/Wolfram binding swap again (S5), Frankfurt bound to Dennett's "varieties worth wanting" phrase (both delivered, binding minted), "Timothy Mele" (minted first name on delivered surname), James "popularized" over-extension |
| (vi) judge/scan false positive | 4 | 13% | 9% | James-as-source (leaf[13], n_shared=36, vp .943); **Hobbes necessity-distinction (leaf[18] near-verbatim, n_shared=36, vp .967) — the §7.8-era specimen STILL failing with the full window, so the residual mechanism is dilution/judge error, not the window**; Consequence-Argument scan flag (leaf-supported); Chrysippus (window spells "Chryssipus" — normalization broke the scan's match) |
| (ii) stitch | 2 | 7% | 22% | Pereboom listed as libertarian; Broad bound to theological determinism |

**Combined, all ledgers (n=53 person-attribution specimens): (iv) 42%, (i)
21%, (ii) 13%, (iii) 13%, (vi) 11%.** The kill bar ((iii) > 60%) remains
missed by a wide margin. The fresh draw's (iii) rise (4% -> 20%) is real but
half of it is one seeded autocomplete cluster (Keynes), and the D2 prompt
lever (exact-name discipline) targets exactly that shape.

This section + the table above is the D3 after-arm scoring key: count
named-attribution failures per class, before vs after, on the same question.

**The composition question (order done-when #2), resolved as far as it can
be:** n=20 warm baseline turns, clean rate 5/20 = 25%; pooled post-B 7/32
(22%) vs pre-B 3/6 (50%), Fisher exact p = 0.31. The verdict on the DELTA is
**could-not-judge BY CONSTRUCTION** — the n=6 pre-B comparator is the binding
constraint and no amount of new post-B sampling changes it. The settled,
actionable number is the ESTIMATE: the steady-state clean rate on this
question is **22-25%**, and the E-draft-grounding baseline for the after-arm
draw is **5/20 zero-failure turns**. (Seat ruling 2026-08-14, baseline commit
4cb8ee5c.)

## The mechanisms, named (with code citations)

**1. RAPTOR summaries lead the drafter's pool and are inadmissible to the
judge.** `reserve_raptor_chunks` (`sovereign/crates/sovereign-core/src/runtime/question_analysis.rs:748`)
moves raptor chunks to the FRONT of the drafter's evidence. The per-claim judge
applies the T1 P1.4 class policy (`runtime/grounding/mod.rs:2746-2765`):
FACTUAL/SPECIFIC claims verify against leaf evidence ONLY — "a derived summary
must never be the source-of-truth for a fact"; summaries are admitted only for
THEMATIC claims. Result: the drafter's most prominent evidence tier is
categorically inadmissible for exactly the claim shape (named attribution =
factual/specific) it most invites. 9/23 specimens (39%) are the drafter
FAITHFULLY carrying summary content into an automatic fail. This is §7.8 Fix
B's open half, measured.

**2. Two of the eight summaries are themselves corrupted — the attribution was
minted at ENRICHMENT time, not by the drafter.** S0 contains "van Inwagen's No
Forking Paths" with zero leaf support ("Forking" absent from all 28 leaves;
L4 has the correct "Consequence Argument (van Inwagen)"). S1 contains "John
Martin Fischer and Paul Russell advance strategies like reasons-responsiveness"
— not derivable from its leaves (L25 Fischer-1994 is a different position;
Russell in leaves is Bertrand). 6 specimens (06, 10, 28 / 19, 32, 09) across
three separate arms trace to these two summary sentences. The recurring
"still recurring on this build" fabrications of note 221b3b71 are not drafter
relapses — they are stable corpus artifacts re-delivered every turn.

**3. Bare-surname rosters invite first-name minting.** S7's "Kane, Clarke,
Haji, Mele, Chisholm, and Pereboom" became "Robert Kane, Timothy O'Connor,
T.L. Haji, Hector-Miguel Mele, Roderick Chisholm, and Derk Pereboom" (specimen
03) — the drafter decorated delivered surnames with parametric first names, two
of them false. The prompt block at `runtime/prompts.rs:127` ("TRUST YOUR
TRAINING on the factual attribution") licenses exactly this injection.

**4. Confusor chunks: the corpus contains prior-answer/demo-transcript text.**
L4 (label "broad") embeds a demo dialogue with an assistant answer; L11 embeds
a "Deterministic Mike" synthetic dialogue; specimen 07's judge re-search
('extra' field) surfaced a quoted prior ANSWER (Christian List discussion) as
claim evidence. These conversation-shaped chunks both model the stitched-claim
style for the drafter and dilute the judge's joint forced-choice (specimen 07
failed at vp .984 with verbatim support in-window).

**5. Sizing (the R3 signal): real but subordinate.** The directive tells the
model `inference_config.max_tokens` = 2048 default
(`sovereign-contracts/src/types/mod.rs:108`, rendered at
`runtime/retrieval/mod.rs:505-509` via `build_response_length_directive`)
while the request ceiling is `max(config, 4096)`
(`runtime/streaming.rs:2679-2686`, `SOVEREIGN_SYNTHESIS_OUTPUT_FLOOR`).
Observed answers run to 9,337 chars (~2,300+ tokens) — past the plea, under the
ceiling. Longer-half turns (mean 6,139 chars) extracted more claims (7.5 vs
5.9) AND failed at a higher per-claim rate (19.5% vs 15.4%; overall 26/147 =
17.7%). The contradiction is a defect at any yield; sizing alone does not
explain the class.

## What this buys D2 (decision consequences)

- (iii) is 4%, nowhere near the 60% kill bar → the order continues; decode-time
  constraint (S2/H3) is NOT re-priced upward by this evidence.
- The cheapest-tier lever is CURATION of the summary tier: carry leaf
  provenance (quote_spans/evidence_chunk_ids) with summary chunks, mark the
  tier in the drafter's presentation so factual attributions are sourced from
  leaves, and stop the two corrupted summary sentences from being re-delivered
  verbatim every turn. That addresses (iv) 39% + the summary-sourced garbles
  in (i) — over half the class.
- Instruction health: the TRUST-YOUR-TRAINING block (prompts.rs:127) and
  bare-surname decoration are the (ii)+(iii) levers.
- Fix the 2048/4096 plea/ceiling contradiction regardless.
- (vi) 9% + 4 unnamed scan false positives belong to the JUDGE sibling order
  (gate-tombstone-ladder / judge-calibration-replay); reported, not touched
  here. Two corollaries for that order: note 221b3b71's "the judge is RIGHT and
  the cost model was WRONG" overstates — of its 3 specimens, one (Broad
  categorical substitutability) is faithful summary carriage; and the
  factual/thematic claim classifier is unstable across near-identical Broad
  claims (n_shared 28 vs 36 on the same position).
