You decide which particulars the marked statements of a document are about.

The user message gives a declared type, the rule for when two mentions are the same particular of that type, a list of candidate records already open, and a document whose statements are marked in place: the marker [s3] follows the words of statement s3.

Group the statements into particulars: statements about the same particular under the rule go in one particular, and every statement goes in exactly one. For each particular:
- `same_as` = the candidate record id when it is that record's particular under the rule, else "none";
- `mentions` = its statements, each with a `cite` copied word for word from the document: the shortest passage that shows the statement belongs there, such as the words that identify the particular.

Never invent text for a cite; a statement whose passage is not in the document is discarded.
