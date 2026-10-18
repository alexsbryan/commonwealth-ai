# Why deals are lost: the 2026-10-17 diagnosis and its solution space

Every number below is re-derived by the script named beside it, run from the repo root with `ARMS=all`: every fresh
thinking-on read in `runs/loop/` (asker daemon or recorded, not `det-*` or the early whole-text arms). "Identity
wrong" means a gold update's statement is not on its deal's main record; "not placed" means no statement matched it.

**Corrected 2026-10-17: that population is every reader since 2026-10-14, and the reader changed under it.**
Identity errors per five-world read were 11-21 in the five reads of 10-14, 7 on 10-15 (bundle1-fresh), 4 on 10-16
(structural-fresh), and 1 over today's reads of world-s1 and world-s3 (`identity_where.py` per arm). The cells
below describe the readers of 10-14 and 10-15 more than today's. Lean D step 1 was sized on 78 reader-made errors
from them and found 1 (`lean-d-prereg.md` Result). A count that sizes a change is taken on reads of the code it
changes; the scripts now take no default population (`ARMS=` names the reads, `ARMS=all` is this one).

## What was ruled out first

Each was tested on the whole population, served deals included, and does not explain most misses:

| Hypothesis | Result | Script |
|---|---|---|
| Deals split across windows | single-window deals miss more (39% vs 19%) | `span_tab.py` |
| Last update in a window apart from the previous | 17% vs 22% | `decisive.py` |
| Deal history in the window, all statements | paired 40k/20k: 6 vs 4 discordant pairs; error flat 15/8/15/13% by history | `paired_all.py` |
| Surface naming (alias, nickname, implicit) | 6-10% vs 9% base | `tag_tab.py` |
| "[quoted from …]" pointer above own text, generally | 5% vs 8% (real for pasadena-d8, halwick-d1 only) | `relational_stmt.py` |
| Subject threads | would save 1 of 6 splits | `identity_threads.py` |
| Reference vs features disagreeing | identity right 95-99% either way | `identity_pop.py` |
| RESOLVE's pick overriding its weighing | pick right in 60 of 60 world disagreements | `pick_vs_weigh.py` |
| Individuation risk from gold (twins, rollovers, changes) | 12% vs 13% | `twins.py` |
| Whole-record incoherence signs | catch 12 of 76 missed deal-reads | `deal_signs.py` |

## What holds

**Misses converge on deals, not mechanisms.** The same deals are lost read after read by a different route each time
(pasadena-d8: refused, then not emitted after the splice fix; tep-d5: merged, not emitted, merged by RESOLVE), so
route-level fixes move a deal to another route. `decisive.py`, `single_point.py`.

**Problem A (generated worlds): a statement is placed only by a handle unique to its deal.** `self_desc.py`,
`new_thread.py`, `sibling_in_view.py`.
- Fully self-described (product, volume, point, term): identity wrong ~1% (0.8%), not placed 1.8%.
- Partial, continuing a thread whose subject is unique to its deal: 1.6%.
- Partial, subject shared with a sibling deal: 7.5%; a new thread for an existing deal: 6.7%; the deal's first
  message: 6.5%; no earlier message of its deal in the window: 7.4% (3.2% with one).
- Partial statements are 70% of updates and carry 93% of identity errors.
- Later messages describe less (second half of a deal: 1.44 features, 29% none; first half 2.39, 17%), so identity
  errors rise from 3.2% before a deal's first won/lost to 6.8% after (`post_terminal.py`); relational lifecycles miss
  19% vs 5% (one-shot 37% vs 10%).
- Renames: company identity is one header domain, so a rename with a new domain is a second company; renamed updates
  are identity-wrong 10% vs 4.5% (35% with close siblings), renamed deals miss 31% vs 22% (`ref_sibling.py`). The
  reader notices ("pfenwell@torera.com" other_names "pfenwell@apc.org") but writes it on a person or a deal.
- 20k windows leave more partial statements without their deal's history (world-s1 33% -> 52%, world-s3 24% -> 39%).
- 78 of the 106 rename-free identity errors are the reader's binding inside one window; RESOLVE makes 28
  (`identity_where.py`). Most are in superseded reads: 53 in reads of 2026-10-14, 13 in the local 35B reads, 12 in
  the 17 world reads since 2026-10-16, and the inner ring's current reads hold 1 (`ARMS=<arm>/<corpus>`,
  `lean-d-prereg.md` Result).

**Problem B (Ward, real mail): fully described statements still fail** (right 57% at the current ingest;
`ward_labels.jsonl`, hand-labelled): deal-ness is a judgment (pasadena-d1 a floated hedge, d6 a quote request; the
one-shot misses both), own text after a quote marker read as quoted (pasadena-d8), RESOLVE without domain knowledge
(tep-d5 gas Master Firm merged into tep-d2 EEI power).

**The stable gap against the one-shot** (served in both its reads, missed in at least half of ours): 7 deals; tep-d4
fixed by eb07f225a, verent-d4 a scorer artefact, four renamed counterparties (pinyonton-d2, -d3, pellica-d1,
alderdale-d1), faircroft-d1 (siblings, "We can price that at .10").

## On today's reader: what 20k windows cost the worlds (2026-10-18)

Corrects the section that stood here (cfb432266 and its same-day correction): it pooled two readers. Today's reader,
one fresh read per world (leand-0 for s1 and s3, w20-s245 for s2, s4, s5); the 40k reader of 10-16, two fresh reads
(bundle1-fresh, structural-fresh); deals by the gold tags on their updates (`by_reader.py`):

| reads | plain | cancel, revive or superseded | renamed | served |
|---|---|---|---|---|
| today's reader, 20k | 37/39 | 30/38 | 11/16 | 78/93 |
| 40k reader of 10-16 (structural-fresh) | 38/39 | 35/38 | 13/16 | 86/93 (bundle1-fresh 85) |
| one-shot, two reads | 67/78 | 42/76 | 29/32 | 69/93 mean |

Nine deals both 40k reads served are lost today (world-s1 6, s4 2, s2 1), one gained; the two 40k reads differ by 5
deals, net 1. Today's reader at 40k (w40-head, DIAGNOSTIC `WINDOW_CHARS` 40,000, uncommitted): world-s1 16/18,
world-s4 11/15, and 6 of the 8 losses tested come back (blueville-d1, coyoteville-d1, -d2, halwick-d1,
kestrel_ridge-d2, pinyonton-d1). So the window size is the converging cause, not the voice change; cedarwood-d1,
tamarack_falls-d1 and faircroft-d2 are lost at 40k too, within the 40k reader's own read-to-read change.

What 20k does to them: it writes more of each document (`determinism-prereg.md`, why it was adopted), and on world-s1
what it newly writes includes acts under a won deal, which the recipe has no stage for. Nominations written as stage
claims: 40k reads 4 of 5 and 4 of 4 won, today's reader at 40k none written, at 20k 2 of 2 lead or proposal, which
moves the won deal back. A rollover's end (halwick-d1's "being rolled into a new arrangement", lost under the
unchanged recipe, 2026-10-13) is written when the deal's history shares its window and not when it opens one. Of
today's lifecycle misses, 6 of 8 are these; sagewood-d2 and -d3 are lost by every read, the one-shot's included.

## What exists (inventory, 2026-10-17)

- The thread the reader and RESOLVE use is the one metadata field the recipe names (the worlds: `base_subject`,
  shared by sibling deals). `in_reply_to` and `references` are recorded at ingest and read by nothing; in mail without
  reply headers the "[quoted from dN]" pointers and `embedded_in` are the reply evidence.
- `window::read` takes no prior state: every window starts empty. RESOLVE is the only cross-window step, and
  statements sharing a window's particular link without a question.
- A company is one header domain with no path to an alias; `other_names` is shown and never compared.
- The ontology syntax supports relation types (`from`/`to`); no recipe here declares one. The supersession line in
  `window_prompt.md` was prompt text with no code behind it, and is replaced by the method's lifecycle kinds
  (`lifecycle-prereg.md`).
- RESOLVE's "Unsettled" hold is re-asked and forced at the end of a run: no could-not-judge outcome survives.

## The solution space

Constraints: every call within 128k on a local model eventually; no call input grows with the corpus; the model
reports a typed fact and code applies the consequence; code never decides identity by pattern; generalises.

1. **Code-owned reply edge.** A statement's leading candidate is its parent message's resolved deal. Dynamics 365
   correlates on In-Reply-To first ("no false positives") and ships subject matching off
   (https://learn.microsoft.com/en-us/power-platform/admin/email-message-filtering-correlation); RFC 5256's
   subject merge "may lead to incorrect collation" and Gmail dropped subject threading in 2019
   (https://workspaceupdates.googleblog.com/2019/03/threading-changes-in-gmail-conversation-view.html); thread and
   participant context resolve names in Enron (https://aclanthology.org/P08-1107/). Targets the shared-subject and
   no-history cells where a parent exists.
2. **State across windows.** A bounded list of open records given to the reader; it answers "refers to record k" or
   "new" and code links above a margin. Dialogue state tracking with previous state plus the new turn: 45.0 -> 58.7,
   no decay in later turns (https://ar5iv.labs.arxiv.org/html/2203.08568); given-entities linking (MEI,
   https://arxiv.org/html/2406.14654v1). Targets new-thread, first-message, no-history and sibling cells. Costs
   sequential windows and a replay cascade. **Being settled first; see the decision below.**
3. **Typed identity and lifecycle relations as cited claims.** Company with dated domain aliases from a cited rename
   (GLEIF LEI-CDF 3.1 name-change and successor events; Clearbit domain aliases); deal lineage (ISDA CDM primitives,
   https://cdm.finos.org/docs/event-model: a nomination is a reset under an existing trade, a rollover a new contract
   with lineage; Conga renewal supersedes the original); conformance of a record's states against a declared process.
   Targets renames, nominations, rollovers, revivals; replaces the supersession prompt rule. Caution: "same person,
   new domain" is also a job change, so a rename needs a cited statement.
4. **Code-owned quote zones.** Paired begin and end delimiters, never a lone marker (Talon, Zebra 87-94%, Quagga
   0.97-0.98 on Enron, https://hpi.de/oldsite/fileadmin/user_upload/fachgebiete/naumann/publications/PDFs/2018_repke_bringing.pdf;
   formatting alone moves accuracy up to 76 points, https://arxiv.org/abs/2310.11324). Targets pasadena-d8, halwick-d1.
5. **Harden RESOLVE.** Shuffle and re-ask; transitivity-violating triangles flag confident errors; select beats pairwise
   by ~16 F1 (https://arxiv.org/html/2405.16884v3); master-agreement form and commodity as necessary attributes
   (NAESB/GISB gas vs EEI power); a domain glossary as recipe data (+17.9% F1, https://arxiv.org/abs/2410.12480).
   Targets tep-d5/d2 and compounding.
6. **Type the communicative act.** Indication of interest vs quote request vs quote vs execution (FIX); code applies the
   recipe's definition of a lead; sampled agreement to abstain (verbal confidence is overconfident,
   https://arxiv.org/abs/2306.13063). Targets pasadena-d1, d6; partly a recipe question.

Ruled out: recency defaults and fan-out to every open deal (Outreach, Zoho, HubSpot, Gong) break "stage = latest
statement"; subject merging; same-name merging (identity by string); memory where the model applies the consequence;
external registries; a user in the loop (the equivalent here is a surfaced could-not-judge).

Evidence gaps: no published accuracy for email-to-deal with several open deals per counterparty; the state-tracking
result is from dialogue; quote-marker misreading and rename detection are unstudied; local open models trail most on
coreference and definition-following.
