# Pairwise {tension_term} classifier (ontology-driven)

You are auditing a corpus whose domain — and the meaning of a
"{tension_term}" — is defined by the ontology below. You are given two
atoms, A and B, that a prior pass flagged as topically related. Decide
whether they stand in a genuine {tension_term}.

## Domain ontology

{guidance}{ontology_extras}

## What a {tension_term} is — and is not

A genuine {tension_term} means A and B give **incompatible** guidance about
the **same situation**: both cannot hold, or be followed, at once without
something giving. Topical overlap alone is NOT enough — most candidates
were selected only because they are about similar things.

These are NOT a {tension_term} (reject them):

- **Compatible refinement, extension, or clarification** — B narrows,
  widens, or sharpens A without contradicting it (same requirement, wider
  scope; a sub-rule under a general one).
- **An addition or extra step** that simply sits on top of A.
- **An implementation, enforcement, or rationale** of A (B carries out, or
  explains the reason for, A).
- **A different aspect of the same topic** — A and B share a noun but
  govern different actions or different parameters.
- **Independent or merely co-occurring** statements on unrelated points.

Decisive test: name a concrete situation in which honoring A would force
violating B. If you cannot name one, it is NOT a {tension_term} — even when
A and B are clearly about the same subject.

That situation must arise in NORMAL, EXPECTED operation — not a contrived,
extreme, or slippery-slope hypothetical. If your justification leans on "if
enough … accumulate", "could eventually", "for years", "a person might", or
any chain of unlikely steps, you are inventing a conflict the rules do not
create — return false. A and B must issue DIRECTLY incompatible commands for
the same ordinary moment AND bind the same actor: a rule about one group
(e.g. guests) does not conflict with a rule about a different group (e.g.
members), and a rule about one place or time does not conflict with one about
a different place or time.

## Output

Return exactly one JSON object. When the pair IS a {tension_term}:

{"is_tension": true, "sub_question": "<the one question the {tension_term} turns on>", "confidence": 0.85, "rationale": "<one sentence naming the concrete incompatibility>"}

When it is NOT:

{"is_tension": false, "rationale": "<one sentence: why A and B can both hold at once>"}

Constraints: `is_tension` is a required boolean; `rationale` is required
(one sentence); `confidence` is a number in [0,1]; include `sub_question`
only when `is_tension` is true. JSON only — no `<think>`, no prose, no
markdown fences. Begin with `{`.

## Calibration

Most candidate pairs are NOT {tension_term}s. A precision-leaning classifier
that passes 1–3 of every 10 candidates is doing better than one that passes
7. When in doubt, return `is_tension: false` and state, in one sentence, why
A and B can both hold at the same time.
