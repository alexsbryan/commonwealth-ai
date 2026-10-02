# Federal Case Law (CourtListener)

I'm a securities litigator. I want a corpus of federal published opinions
I can search, with a citation graph that lets me trace which cases cite
which, and an investigation schema that surfaces counsel-of-record
patterns — which firms appear together as co-counsel, which firms tend to
appear opposite each other, who's repeat-counsel for the SEC vs. private
plaintiffs.

Initial scope: published opinions from the Ninth Circuit, 2020 forward.
Dissents and concurrences should be treated as separate documents from
the majority opinion. I care about author attribution per opinion.

I'd want to expand later to other federal circuits, and ideally to
filings (briefs, motions) — but I know PACER access is a real constraint,
so let's stick with CourtListener for v1.

For the citation graph: cite_to / cite_from edges between opinion
documents, with attributes for the citing court and the date of the
citing opinion.

For counsel-of-record: party representation as a relationship between
attorneys/firms and parties; pattern-detection for role overlap (the
same firm representing a defendant in case A and an amicus in case B
on the same legal question).
