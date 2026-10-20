QUERY GRAMMAR (answer with exactly one JSON object):
- target_type: the kind of thing the question asks for.
- filters: conditions on the target's OWN attributes, or on `name` (the entity's own name).
  op `eq` = equals, `contains` = the attribute's text mentions the value, `lt` / `gt` =
  earlier/smaller or later/larger than the value. negate=true keeps the targets that do NOT
  meet the condition. A time is a date ("2026-10-20"), a month ("2026-10") or a year as a
  signed number (318 B.C. = -318, A.D. positive); `lt` / `gt` = wholly before / after it,
  `eq` = within it.
- relations: the target must (negate=false) or must not (negate=true) be linked by `relation`
  to an entity of `other_type` named `other_name` (null = to any such entity) that also meets
  `where` (null = no further condition). `where` holds that linked entity's own filters and
  its own relations to named entities, so a relation can reach entities that are described
  rather than named.
- aggregate: none = list every target that matches; count = how many targets match;
  argmax / argmin = the matching target with the largest / smallest `aggregate_over`, which is
  one of the target's time or quantity attributes, or a related type (= how many distinct
  linked entities of that type the target has, counting only those that meet the `where` of
  the query's relation to that type); sum = the total of `aggregate_over`, a quantity attribute
  of the target, over the matching targets, one total per value of `group_by`: one of the
  target's attributes, or the `attr` of a relation `<target>.<attr>` to total per linked entity
  (null = one total). Only sum takes `group_by`.

EXAMPLES (a different knowledge base: books, authors, libraries):
Q: Which books printed before 1600 are held by the Bodleian?
A: {"target_type": "book", "filters": [{"attribute": "printed", "op": "lt", "value": 1600, "negate": false}], "relations": [{"relation": "held_by", "other_type": "library", "other_name": "Bodleian", "negate": false, "where": null}], "aggregate": "none", "aggregate_over": null}
Q: Which author has written the most books?
A: {"target_type": "author", "filters": [], "relations": [], "aggregate": "argmax", "aggregate_over": "book"}
Q: Which authors wrote a book printed before 1500?
A: {"target_type": "author", "filters": [], "relations": [{"relation": "wrote", "other_type": "book", "other_name": null, "negate": false, "where": {"filters": [{"attribute": "printed", "op": "lt", "value": 1500, "negate": false}], "relations": []}}], "aggregate": "none", "aggregate_over": null}
Q: Which libraries hold a book written by Austen?
A: {"target_type": "library", "filters": [], "relations": [{"relation": "held_by", "other_type": "book", "other_name": null, "negate": false, "where": {"filters": [], "relations": [{"relation": "wrote", "other_type": "author", "other_name": "Austen", "negate": false}]}}], "aggregate": "none", "aggregate_over": null}
