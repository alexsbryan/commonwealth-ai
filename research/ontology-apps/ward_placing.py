"""Ward's PLACE rung, read a second time: where does each gold deal's counterparty live? (order ontology-layer-12)

    ward_placing.py [RUN] [--json out.json]

Every tune gold deal ladder_ward.py counts (its items, its Facts) gets one row: per message of the deal, whether the
gold counterparty is among the message's header-derived outside parties (the recipe's `outside_parties` path,
`document / (from | to | cc) / employer [!ours]`, and `party_of_message` = the first of them), whether the text the
run indexed for that message names it, the `party` each stage_update claim on it carries, and what RESOLVE decided
for those claims (resolve_decisions.jsonl and the recorded log). A PLACE-lost deal is classed by CAUSES, a closed list
committed before any row was read; the rest are classed by their ladder stage, so the classes sum to the ladder.

"Names it", case-insensitive: one of gold's written forms (ward/score.py gold_forms: the name and each parenthetical
alias, through name_core) as a whole-word run of the folded text, or one of gold's domains as a substring of the
lowercased text. "Outside party": an address in From, To or Cc whose domain does not end in enron.com (the `ours`
set), equal to a gold domain or a subdomain of one.
"""
import enum


class Cause(enum.Enum):
    """Why a PLACE-lost deal's latest claim sits on no record matched to it. Decided on the deal's latest messages
    (gold's latest stage_update files, the ones ladder_ward's PLACE fact reads), in this order:

    NO_JOIN        gold's counterparty joins no company atom of the run (by a gold domain): the order's stop condition
    DERIVATION     gold's counterparty IS an outside party of a latest message's headers, yet no stage_update claim on
                   the latest messages carries a `party` resolving to it (nothing derived, or another company first)
    NO_LINK        gold's counterparty is an outside party of a latest message's headers AND a stage_update claim on
                   the latest messages carries it as `party`: the party was there and nothing linked by it
    BODY_ONLY      not an outside party of any latest message's headers, but the indexed text of one names it
    ABSENT         neither in the latest messages' headers nor in their indexed text
    OTHER          anything else (e.g. a claim carries it as `party` though no latest header shows it)
    """
    NO_JOIN = "gold counterparty joins no company atom"
    DERIVATION = "party derivation wrong"
    NO_LINK = "party in headers but no linking source"
    BODY_ONLY = "party only in body"
    ABSENT = "party absent"
    OTHER = "other"


# What each cause points at (named with the counts; the seat picks): domain-free changes only.
POINTS_AT = {
    Cause.NO_JOIN: "the instrument or the company source, not placing",
    Cause.DERIVATION: "the party derivation (the `document` step / `first` fold), before any RESOLVE change",
    Cause.NO_LINK: "declared derived roles (stage_update.party) as a RESOLVE identity source",
    Cause.BODY_ONLY: "the unbuilt Pick pass: a reference read from text among declared candidates",
    Cause.ABSENT: "neither: the deal's latest messages do not carry its party",
    Cause.OTHER: "read case by case",
}


def classify(joined, in_headers, party_claimed, in_body):
    """One PLACE-lost deal's cause from four facts on its latest messages: `joined` (gold counterparty has a company
    atom), `in_headers` (it is an outside party of some latest message), `party_claimed` (some stage_update claim on
    a latest message carries a `party` resolving to it), `in_body` (some latest message's indexed text names it)."""
    if not joined:
        return Cause.NO_JOIN
    if in_headers:
        return Cause.NO_LINK if party_claimed else Cause.DERIVATION
    if party_claimed:
        return Cause.OTHER
    return Cause.BODY_ONLY if in_body else Cause.ABSENT
