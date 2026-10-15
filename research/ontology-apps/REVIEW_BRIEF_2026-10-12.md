# Ontology layer: status brief for outside review (2026-10-12)

Written for reviewers who have not followed the work. Every figure has a run directory, commit or note behind it,
cited where it appears. The 2026-10-10 brief is kept as `REVIEW_BRIEF_2026-10-10.md`. Read its sections 1 and 2
for the objective and the method in full. This brief covers what changed after that review, what we now
believe the problem is, and how we plan to converge.

## 1. The objective, in one paragraph

A user who understands their domain writes one declarative recipe: the record types, what makes two mentions
one particular, how state changes, and which statements to look for. They run the CLI's default commands and
get typed, cited records:
- the deals in a sales mailbox (Ward);
- the cases in an issue tracker (uv);
- the incidents in a news archive (GVC).

The records come out very similar whoever wrote the recipe. No recipe carries a number tuned on data. A run
uses a local model and replays without it. The three systems are instruments, never targets.

## 2. Where we stand: the records a user gets today

Numbers are from the repair-2 run (`runs/about`, binary bbe043a9b, one run per leg). "Ours" is our recipe;
"blind" is a recipe written by an author who saw only a domain description.

| System | What a user needs | Ours | Blind | Model calls per document (ours) |
|---|---|---|---|---|
| Ward | current deals at the right stage | 0 of 47 | 1 of 47 | 52.9 |
| Ward | deals matched to gold (party and messages) | 2 of 39 | 12 of 39 | |
| uv | cases served at the right current state | 19 of 44 | not comparable (other fold) | 7.1 |
| uv | gold case states hit (stage ladder) | 76 of 132 | 70 of 182 | |
| GVC | event mentions on a record serving their type | 50 of 977 | 0 of 977 | 24.2 |

The motivating case, Ward, gives a user nothing usable.

We also have a reference reader, run on 2026-10-08 (crm-proof loop 12): Claude Haiku put behind the same
downstream, reading the same Ward messages. Scored with today's ladder (`research/ontology-apps/ladder_ward.py`):

| Reader | Deals whose latest message is read at the right stage | ...and sit on a record with their party |
|---|---|---|
| Loop 9, local model, one pass per message | 14 | 5 |
| Loop 12, Haiku, one pass per message | 32 | 9 |
| Today, local model, our recipe | 25 | 0 |
| Today, local model, blind recipe | 2 | 1 |

Two days of machinery improved stage labels: wrong labels on the latest message fell from 20 to 4. It did not
improve the records.

## 3. What the 2026-10-12 review's four repairs did

All four are built, accepted by unit tests and the six contracts (C1-C6), and measured live on the same inputs.

| Repair | Commit | Measured effect |
|---|---|---|
| 1. A reference holds a record of its type, or nothing | d3b12ba9c | Ward deals matched 11 to 7, stages served 4 to 1. The lost credit was the scorer matching company names held as text. More honest, fewer records. |
| 3. Every field question reads the whole document | 72924293f | uv up (ladder 61 to 66). Ward down: deals matched 7 to 3, because counterparty picks moved from header records to mentions. GVC 40 to 36 of 977. Note b08106f4. |
| 4. A state moves only by declared transitions | fd211d3f9 | uv served state 13 to 15 of 44. Its pre-registered H4 was killed: one case was lost to a duplicate read as a decline. |
| 2. A statement names the particular its document is about; a document is a member of what it is about | 6adb15618, ad6360ac2, bbe043a9b | Table below. Note 40ebab18. |

Repair 2 on all six legs (before is the whole-text run, `runs/whole-text`):

| Leg | Before | After | Calls per document |
|---|---|---|---|
| ours-ward | 0 of 47; recovery B3 .375; B3 .921 | 0 of 47; .529; .819 | 37.6 to 52.9 |
| ours-uv | 66 of 132; served 13 of 44 | 76 of 132; served 19 of 44 | 4.4 to 7.1 |
| ours-gvc | 36 of 977; recovery .408; B3 .471 | 50 of 977; .382; .426 | 23.6 to 24.2 |
| blind-ward | 1 of 47; recovery .327; deals 11 | 1 of 47; .566; deals 12 | 36.6 to 38.3 |
| blind-uv | 78 of 182; recovery .663; B3 .882 | 70 of 182; .627; .818 | 7.4 to 8.7 |
| blind-gvc | 0 of 977; recovery .232; B3 .543 | 0 of 977; .244; .505 | 38.0 to 49.3 |

B3 fell on all six legs and recovery rose on four. Only our uv recipe shows a gain a user would see. On
blind-uv, About read 794 of the blind recipe's 956 documents as about no case.

Repair 2's pre-registered claim H1 was also killed on Ward (`particulars-prereg.md`). H1 said the "how many
particulars is this document about" question is answerable from one document. About's count was right on .495
of messages (ours) and .596 (blind), below the .65 kill line. The counterparty was right on .252 and .196 of
(message, deal) pairs, below .60.

## 4. What we learned: the problem is on the input side

### 4.1 We had never looked at what the model is shown

Every instrument we built (bars, ladders, contracts) inspects outputs. On 2026-10-12 we recorded production
requests byte for byte through a proxy (`research/ontology-apps/seat/`, c19287139): 253 requests for four Ward
messages, about 53k prompt tokens per message. The main defect was visible on the first screen.
- **The stage question was asked of physical lines.** It read "which kind of claim does line 8 state?", and on
  Ward line 8 is a 75-character wrap fragment: "Attached please find the Cover Sheet, General Terms and Conditions
  and". Half of Ward's asked lines are under six words, and 18% end mid-sentence. On uv it is 50% and 11%.
  On GVC a line is a sentence. One question meant different things on each system.
- **The rendered email header was shown twice:** as the document's declared facts and again as body lines 2 to 6.
  Mention then offered a whole subject line as a "company". A cc candidate reached the model as
  ".ward@enron.com (houston)".

Note 9fba6f1e.

### 4.2 Ask each question at the level where its answer lives

A deal's stage is something the message does ("here are the contract drafts"), not a fact one line states. On
the lines that carry the stage, the local model answers the line question "none" with .84 to .99. Asked about
the whole message, the same model gets the stage right on the messages below.

Scored against gold on the same documents. The line-level figures are b1's, from the pre-registered Locate probe
(`locate-probe/`, dab1d8eab). The message-level probes are in the session scratchpad (note a4b85978).

| Population | Asked of one line | Asked of the whole message |
|---|---|---|
| Ward, b1's 26 documents with a gold stage line | 10 (a gold stage line read right at kind and value) | 18 to 22, across two wordings (scored on 27: one more document, gold from our own table) |
| uv, 125 documents with a gold state (value right) | 76 | 96 to 98, across four wordings |

b1's probe also shows the information is present at line level: the lines we lose rank well above unmarked lines
(AUC .88 Ward, .85 uv). Within a document, Locate's own ranking puts a gold line in its top three for 24 of
32 Ward (document, value) items (chance .22), and for 23 of the 30 uv documents with more than three asked lines
(chance .26). On GVC, per (document, happening type), it is .717 against chance .460 (`cite-probe/PREREG.md`).
The argmax against "none" is what loses these lines. No line-level
question shape tested does better than a plain paraphrase of today's question.

### 4.3 This model is sensitive to framing and wording

- **An abstract system prompt costs most of the gain.** On 25 Ward messages whose answer flipped between two
  probes, changing one thing at a time gave:

  | Version of the prompt | Right, of 25 |
  |---|---|
  | Original | 18 |
  | Only production's declared facts added | 18 |
  | Only "standing" in place of "stage" | 17 |
  | Only an abstract system prompt ("a particular... the values its standing can take") | 8 |

  A system prompt that names the declared type and field ("where a deal it is about stands") does better.
  Production's system prompts are abstract (note a9b36e8c).
- **Paraphrase moves single answers a lot.** Between a question and its paraphrase, the answer changes on 15% of
  uv documents and 18% of Ward documents. The changes offset in the totals. Line level is similar: 16% of Ward
  lines, 6% uv, 10% GVC (b1).
- **Agreement across wordings predicts correctness, with no labels.** Where four wordings agree, the answer is
  right on 91% of uv documents and 69% of Ward. Where they disagree, it is usually wrong on uv. On Ward, at
  least one wording is right on 27 of the 33 disagreements, so the information is there and the framing decides.

### 4.4 The party has nowhere to land (identity, not reading)

- 44 of 47 Ward deals have no deal record carrying their party, and 68 of 74 deal records hold no counterparty.
  The picked words are mostly the right company ("Smurfit-Stone Container Corporation").
- For 19 of the 25 deals we read at the right stage, no company record can exist. Company records come only
  from header domains, and the company is named only in prose. Only 10 of 92 tune messages carry the outside
  party's address even in the body.
- For the other 6, a header record exists, but the mention naming the same company is never linked to it.
- The blind recipe gets the party, by folding over header records, and misses the stage: 34 of 47 deals have no
  stage claim. Each recipe gets the half the other misses.
- Linking each picked mention to a record has a no-model ceiling of 2 to 9 deals matched and 0 to 6 at the
  right stage (note 217e01cd).

### 4.5 GVC is a different problem

GVC's lost lines rank like unmarked lines (AUC .62, b1). They are references and reactions that gold marks as
event mentions: an example gap in gold's unit. Its placement loss is decomposition: 175 of 320 statements span
several sub-events of one incident ("shot and killed" is a firing, a hit and a death). Neither is a reading
problem that the message-level change addresses.

### 4.6 Why this took so long

- Our instruments looked only at outputs.
- An output requirement, that every answer cites verifiable lines, set the unit of reading on the input side.
- Each downstream symptom got a mechanism (corpus-fitted weights, settling, membership, local subjects, About)
  instead of a fix to the question.
- The model has about 3B active parameters at 4-bit. It does not recover from an ill-posed question; Haiku
  does, by reading the whole message.
- We read single-run deltas without a wording floor. On this model the floor is larger than most deltas we
  reported.

## 5. Unresolved

1. **Internal messages read as stage moves.** 7 of the 8 Ward messages that gold leaves stageless and the model
   gives a stage are Enron writing to Enron. Gold's rule is that a stage moves only in an exchange with the
   counterparty. Showing the "ours" facts did not change the answers. Untested: the act framed as the sender's
   move toward the recipients, gated by "is this about a deal".
2. **About's own reliability.** Its count was right on .495 / .596 of Ward messages, and it read 794 of 956
   blind-uv documents as about no case. Its wording is abstract; it has not been measured under paraphrase.
3. **Messages about several particulars.** 21 Ward messages belong to several gold deals, for example 8 won deals
   in one list and one RFP for 5 clients. The per-particular act question is untested.
4. **The party.** A company named only in text needs a record. The proposal is in section 6. It revises repair
   1's rule that a mention never becomes a record.
5. **GVC decomposition.** b1 proposes asking "which sub-event", then "which else, or no other", with an explicit
   stop label. Per-type yes/no over-says yes (precision .464).
6. **Label-free precision.** No estimator has come within .1 of measured precision (bar layer-estimator).
   Agreement across wordings is a candidate and is not pre-registered.
7. **Gold.** Ward's gold was drafted by another model family and has 127 uncertain labels and contested stage
   boundaries. The scorer matches company names only on an exact name core. GVC's gold marks pronouns as event
   mentions.
8. **Cost.** About added 18% to 41% calls per document. Message-level questions could replace most per-line
   questions; unmeasured.
9. **Stored embeddings are mostly zero off Ward.** The whole-text legs' chunk vectors are zero for 3,072 of 3,152
   uv chunks and 512 of 743 GVC chunks (installs copied from runs/scaffold-3sys). Any plan that uses stored
   embeddings, including the citation ranking above, must re-embed uv and GVC first.
10. **Debt.** It is not decided what the new question order makes redundant: per-line Locate for acts, settle,
   possibly the weights. Nothing is retired yet.

## 6. How we plan to converge

The principle: master the input, then the output. Put the right question, at the right level and in the
declaration's own terms, in front of the model. Then decide how answers become records. Reuse the passes we
have (About, Choose, Pick, Point, RESOLVE) in a new order before building anything.

1. **Question plan, pre-registered with b1 before any build.** One end-to-end leg on Ward and uv, the same
   inputs as `runs/about`:
   - About first, as the gate: is the message about a particular, and which.
   - For each particular, the act (stage, state) and its fields, asked of the whole message, with system prompts
     that name the declared type and field.
   - Each act asked in two wordings; a disagreement is held, not settled.
   - Citation after the answer, with no per-line Locate (19.7 calls per Ward document today). For a report
     kind, rank sentences by embedding similarity to the answered value's declared description, then one forced
     choice among the top three: on GVC the gold sentence is in the top three .896 of the time, against .717
     for Locate's ranking (b1's M1, `cite-probe/`, 7b78f12ab). For an act kind, embeddings fail (Ward 22 of 32
     against Locate's 24; uv 11 of 30 against 23), so after the act is answered, one forced choice over the
     document's numbered sentences. That second step is proposed, not yet measured.
   - GVC's reports stay at line level.

   Read by error class and cost, beside the line-level run. Not a single bar.
2. **The party.** A mention is a statement of its type and is resolved by the existing RESOLVE: to a record the
   document's declared fields name, to an open record, or to none, which opens one. A mention that resolves into
   a declared exclusion set is dropped, which keeps "Enron" out.
3. **The output side.** The hold rule above. Wording agreement pre-registered as the label-free precision
   estimate. Then the handoffs: particular to record, act to fold.
4. **GVC.** Sub-event decomposition with sequential forced choice and a stop label (b1). Gold's unit gap is
   named apart, not fitted.
5. **Then the six legs, both authors, the same inputs.** Then the held-out folds, once.
6. **Retire what the new order makes redundant,** and count it.

What would redirect us:
- the message-level plan does not produce better records end to end on Ward and uv;
- agreement across wordings stops predicting correctness inside the pipeline;
- the party link does not move Ward's deals matched toward its ceiling.

In any of those cases the next lever is the model itself: a stronger local reader, or training on recorded
answers. That should be a deliberate decision, not a default.

## 7. Questions for reviewers

1. **Choosing the level.** Is "ask each question where its answer lives" the right general rule? Should a
   recipe declare whether a claim kind is the author's act or a reported fact, or should the layer infer it?
2. **Wording agreement as reliability.** Agreement across paraphrases may fail when every wording shares one
   misreading. What are the known pitfalls, and what should the pre-registration guard against?
3. **The party.** Is "a mention resolves to a record, new or existing, through the same decider" the right
   generalization, or should particulars known only from text stay unresolved?
4. **The model.** On a 3B-active, 4-bit model this sensitive to framing, what is the better investment:
   asking in several wordings, a larger local model, or training on recorded answers?
5. **Domain rule or general property?** Ward's gold says internal messages do not move a deal. Is that domain
   knowledge the recipe should carry, or a general property of acts (a move is made between parties) the layer
   should know?
6. **What to retire.** What machinery would you retire first?
