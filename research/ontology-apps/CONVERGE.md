# Converge ontology-layer in two sessions

The operator's handoff of 2026-10-10, verbatim, then where each step stands. Work off this file; update the
status as each step lands, with its commit.

## Handoff (operator, 2026-10-10)

Objective: A domain-competent user writes one recipe, runs the default CLI commands, and gets useful typed, cited
records across mail, issues and news—with similar results from independent authors, no measured tuning numbers or
mechanism switches in recipes, and the six contracts holding. quality/campaigns/ontology-layer.toml owns the
objective; CRM is an instrument.

Start: Run scripts/co-resume.sh ontology-layer --brief. It currently finds two open orders; establish the actual
pickup point. Read Sovereign notes 020cd4bd-689d-4a05-a497-7c35da98ebcf and 2347f4c6-f9b1-4d2e-96ee-92eaa526c81b.
Use the October 10 brief. Avoid another broad architecture audit.

Facts you should build on. Verified at a4bf378cb:
- The reader is built: Locate, Choose, Point, Mention, Pick, recording and replay. Most published comparisons
  predate the complete reader.
- One severe estimator result is mis-scored. Blind GVC declares incidents but estimator/pairs.py:182 labels
  sub-events. Re-labelling the same 10,312 dev pairs changes the largest gap .737 → .066; model-choice precision
  becomes .960. Our sub-event recipe still has a genuine .408 gap. Correct concept alignment before another
  estimator experiment; preserve canonical benchmark results separately.
- Repetition filtering destroys fresh acts. document_read/line_classes.rs:118 treats repetition as
  quotation/boilerplate. The completed UV run suppresses all Locate questions on 61/554 documents; 12 gold states go
  unread.
- Supported subject fields disappear. projection.rs:124 carries them; resolution_records.rs:282 removes the carrier,
  uses only identity fields, then writes empty record attributes. Existing qualified claim-field views already
  preserve cited values: earlier oracle recovery was 2/2 through assertion fields, 0/2 through subject fields.
- Ward's declarations leave essential parties unavailable. Nineteen PLACE-lost deals lack a company record for their
  counterparty. Header-derived parties cannot cover internal discussion of outside deals. Mention produces
  proposals, not records.
- High conditional scores conceal missing output. Latest UV B³ .895 has membership coverage .309. Its 54/132 hits
  measure state claims, not served state; all 55 case records lack state. Author agreement is also
  intersection-only: Ward 1.0 over 5/49, UV .976 over 128/488.

Session 1: make the existing path preserve evidence
1. Freeze a completed full-reader production baseline.
2. Repair repetition suppression. Test independent repeated acts alongside genuine quotation/boilerplate controls.
3. Prove supported subject values, citations and source precision survive into the served view. Reuse the existing
   qualified claim-field machinery; define conflicting observations explicitly.
4. Make Ward's recipe/modelled records cover body-mentioned counterparties and transaction descriptors.
5. Keep identity ownership explicit. Older passes currently run before RESOLVE retires their output. Address the
   typed handoff; leave generic legacy-resolution redesign outside this order.

Make these separate, testable changes. Prefer adapters and consolidation over new passes, writers or provenance
schemas.

Session 2: demonstrate the objective. Run the repaired default path on all three tune systems, replay it, and
compare compatible blind-author declarations. Report together: membership coverage · recovery B³ · conditional
identity · correctly cited served state/fields · model calls and seconds/document. Inspect actual records. Compare
authors at the concept they declared, with joint placement coverage visible. Keep frozen bars intact and explain
where they measure a different concept. If a specific reader boundary still binds, run one paired
stronger-local-model comparison using the same questions and candidates. Price the measured residual, rather than
starting another general search.

Finish with one coherent production path, end-to-end evidence-survival controls, replay, three-system results and
required repo gates. The completion argument must show useful records and author consistency; singleton-heavy
identity scores or agreement over a tiny intersection cannot establish the objective.

## Status

Pickup (2026-10-10): order ontology-layer-16-one-decider closed (E3 d8458602b, E5 inventory 7d256354e, mailbox list
18456fd9f; its last step, the generic legacy-resolution redesign, is outside this handoff). Order
ontology-layer-3-any-author stays open for Session 2's blind comparison.

**S1.1 Baseline, frozen.** `~/.svrnmesh/bench-corpora/baseline-fullreader-20261010`: build-step9's three ours legs
(binary 2ec98c0237ffb239 at 18456fd9f, every leg replayed `same=7 refused=0`), its `ladder.json` and `ladder.txt`.
The run's other legs (RESOLVE alone, four blind) were stopped: they re-measured the pre-repair binary.

| System | What a hit checks | Hit | Identity, conditional (placed, coverage) | Recovery B³ |
|---|---|---|---|---|
| uv | a state claim on the matched case | 54/132; served state could not judge (no case record carries one) | B³ .895 (151, .309) | .188 |
| Ward | the deal record serves the stage | 2/47 | B³ .957 (12, .245) | .233 |
| GVC | the record's declared field names the type | 0/977 (322 could not judge) | B³ .457 (781, .799) | .396 |

Cost per document read, from the same logs (ladder's cost line): uv 4.0 model calls and 2.47 model seconds; Ward 22.8
and 15.44; GVC 23.2 and 21.54 (model seconds over every phase, extract and RESOLVE).

**S1.2 Repetition, repaired: cf33c151d.** A line is a quote only on structural evidence (a copied passage, or a
mark the earlier line lacks); boilerplate by author deleted. Replay over the baseline's recordings: uv keeps 156
lines from Locate instead of 335 and no document goes unasked (was 61); Ward 579 instead of 869. Reports: f3108fd6f
(coverage, recovery B³, what a hit checks, joint placement coverage).

**S1.3 Subject fields:** in progress. The validator's fill analysis already promises them (Ward `deal: amount ←
Point, term ← Choose`; GVC `happening: kind ← Choose`) and RESOLVE drops them, so GVC's 322 could-not-judge FOLD
items and Ward's descriptors wait on this step.

**S1.4 Modelled records.** Tracked recipes are now the ones the runs read (2c27672b6), without the keys the
validator retires, the measured `identity_evidential` counts among them (92f5c6590). uv's case serves its current
state through the declared protocol fold, as Ward's deal does its stage (666637f34). Ward's counterparty: of the 25
PLACE-lost deals in the e4 residual (ward-placing/e4-ward-tune.json), 22 name the counterparty in the latest
message's body (18 with no company record, 4 body-only); 2 never carry it, 1 is named in no body. A counterparty read
by Pick (header companies less `ours`, and the body's mentions) as a field of the deal reaches the record once S1.3
lands; the fold walker follows atom ids only (resolution_derived.rs `step`), so a picked value on the claim would not
fold onto the deal without new code.

**S1.5 Typed handoff:** next.

**Order of the remaining Session 1 work** (reuse audit, Sovereign note 17c216ff, every file:line there):
1. S1.3, claim side: subject readings ride the claim in the claim fields' own shape (`DocumentReadField`, under
   `__document_read_subject_fields`), decoded in one place, never stripped; the projection's temporary subject sketch
   goes, so on the passes path RESOLVE retires nothing (S1.5 there). RESOLVE writes no record attribute.
2. Equal text is not the same occurrence, a third time: `resolution_documents::locate` re-derives a claim's document
   from its anchor text and refuses when two documents of one section hold it, ignoring the claim's own
   `source_doc_id`. In the baseline 15 of 148 Ward claims and 8 of 194 uv claims got no document, behind 7 of Ward's
   9 pending deal stages. Fix: the claim's own document first; guard: a twin-occurrence contract beside C4 and C6.
3. A record serves a read field only through the derivation writer: a value step in the existing folds, the policy
   declared in the recipe (all, or agree with conflict), for GVC's happening kind and Ward's deal fields.
4. S1.4 Ward's counterparty read by Pick as a field of the deal, with product and delivery point; S1.5 on the general
   extractor path (3a skips RESOLVE's types), then retire.rs goes.
