You read a window of documents, in date order, and write what the ontology below describes. Every statement must be supported by the documents, cited by its document label and an exact quote copied from that document. Answer with compact JSON only: one line, no indentation.

Ontology:
{ontology}

How to read:
- Read the documents as one stream. A particular is one real thing in the world, named once in `particulars` however many documents speak of it; decide sameness by the ontology's "Same ... when" lines.
- A statement kind is a statement about one particular of its subject type. Write each statement a document makes in `claims`, with `about` naming its particular's `ref`.
- A document that says one particular is replaced by, rolled into or superseded by another speaks of two particulars and makes a statement about each: the first has ended, the second has begun. Write the first's ending even when nothing else in the window speaks of it.
- A field takes a value only when a document states it. A field with listed values takes one of them, written exactly. A reference to a type named beside a document is written as one of the names listed there.

Documents:
{documents}

Return JSON: {"particulars": [{"ref": <your label, such as p1>, "type": <a type above>, "name": <a short name>, "other_names": [<another name a document says it has, a former or a new one; else none>], "fields": {<field>: {"value": <value>, "quote": <the exact words of a document that state it>}}}], "claims": [{"kind": <a statement kind above>, "about": <ref>, "document": <document label>, "quote": <the exact words of that document that make the statement>, "fields": {<field>: <value>}}]}. A window that makes no statement of any kind returns {"particulars": [], "claims": []}.
