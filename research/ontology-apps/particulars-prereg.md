# Pre-registration: particulars before statements (2026-10-11)

Written before any of these tests read data. The approach (note 0f8f8c6c): a document is read for the
particulars it is about, from its declared frame and whole text; identity is decided once per document
particular; parts are identified inside their whole; a protocol move names who may make it. Each claim below
is falsified by its kill bar. Between a kill bar and a pass bar a result is could-not-judge (one run, small n),
never a pass. Baselines are the current approach on the same documents (runs/converge-s3, binary d21197d75).

## H1. The About question is answerable from one document and its frame

The local reader (daemon `commonwealth/primary`, forced choice as production asks it: prefill facts first, the
document's lines numbered, temperature 0, no thinking) answers, per document and decided type, questions
generated from the declaration alone (type name, description, criterion, identifying attributes; no word per
system): is the document about a particular of the type (none / one / more than one); on which line is it
first named or described, then which words (Point); is it about another, different under the criterion (repeat,
at most 5); and each identifying attribute of each particular, from the whole document (Pick among the records
its header fields name less exclusion sets, or a text span; Point for open values).

Populations: Ward's 99 tune messages (gold: the deals each message belongs to); GVC's 78 dev articles (gold:
the incidents of their mentions), with a probe recipe declaring `incident` as the decided type; uv: every
document of the third whose gold case differs from its thread's, plus 100 others drawn by a fixed seed (gold:
its cases).

- Pass: particular count (none / one / more) right on >= 80% of documents in each system; Ward counterparty
  right on >= 75% of (message, gold deal) pairs; GVC's incident pointed at words overlapping a gold mention of
  its gold incident on >= 85% of articles.
- Kill: count right < 65% on any system, or Ward counterparty < 60%, or most failures (read one by one) are
  information absent from the document and its declared fields rather than misread.
- Baseline beside it: the current per-statement counterparty on the same (message, deal) pairs.

## H2. Resolving document particulars beats resolving statements

H1's particulars become one statement each for the production RESOLVE (`svrn enrich resolve-statements`): its
span is the words H1 pointed at, so RESOLVE sees them marked in the whole document beside the candidates' quoted
words, as it does today; no recipe of the three declares an identity key, so no statement carries one. Candidates
come from declared structure as today. Two arms: weighed (default) and `--answer select` (the model's choice
decides). Scored as membership of documents in gold particulars, against the current approach on the same units.

- Pass: Ward recovery B3 >= .55 (current .375); GVC incident-level B3 >= .85 (current .607); uv recovery B3
  >= .80 (current .804) with no gold case split inside one document (10 of 18 multi-statement documents today).
- Kill: Ward <= .40, GVC <= .65, uv < .75.
- The principle change (a choice decides identity) is supported only if `select` >= weighed on all three; it is
  refused if `select` is worse by .05 or more on any.

## H3. A part identified inside its whole fixes sub-event identity

GVC's gold sub-event mentions (oracle mentions and types), each assigned to its document's incident record from
H2, grouped by (incident, type). Ceiling beside it: gold incident and gold type. No model call.

- Pass: B3 over gold chains >= .75. Kill: <= .60 (flat RESOLVE alone on gold mentions read .515 to .599).
- The ceiling says how much a part's own participant (which victim) still has to carry.

## H4. A move names who may make it

uv's case_state claims of the converge-s3 replay, re-folded with one declared rule: confirmed, in_progress,
resolved and declined are moves only a maintainer makes (a document in `maintainer_post`, or an event whose actor
holds maintainer association on some post: a role joined by login); anyone reports; a case a maintainer resolved or
declined returns to reported only by a reopen event. Another's statement is evidence, never a move. No model call.

- Pass: >= 18 of 44 gold cases serve the right current state (13 today) and no case right today turns wrong.
- Kill: < 15 of 44, or any right-today case turned wrong by the rule.

## H5. After the build, what remains is misreading

Built into the reader and RESOLVE (not probed), re-trace the nine cases of note 0f8f8c6c end to end.

- Pass: every remaining failure is a misread question whose answer was in its input.
- Kill: any failure whose deciding information was outside the document and its declared frame.

## Cost and order

H4 and H3 call no model. H1 is about 2,000 forced choices (~40 min of the daemon), H2 a few hundred RESOLVE
calls. Order: H4, H1, H2, H3; H5 after the build. Every answer is cached by request, so a re-score costs nothing.
