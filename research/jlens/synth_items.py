"""Seeded synthetic distractor-bearing evidence-QA items.

Fictional entities (no parametric priors), one witness passage + confusable
distractor passages (shared name fragments, same attribute type, nearby
values). Every value is controlled, so scoring is exact string matching —
no judge. Absent-witness items ask about an attribute deliberately omitted
from the entity's passage.

Deterministic: everything derives from a passed-in seed via random.Random.
"""

import random

FIRST = ["Calloway", "Ashgrove", "Merton", "Halloran", "Winslow", "Bram",
         "Ostrander", "Fenwick", "Quill", "Marchbank", "Tullis", "Verne",
         "Ridley", "Sablewood", "Corvin", "Ellsmere"]
KIND = ["Institute", "Foundation", "Collective", "Society", "Laboratory",
        "Consortium", "Trust", "Bureau"]
CITIES = ["Dunmore", "Fallbright", "Kestrel Bay", "Northgate", "Veldt City",
          "Harrowfield", "Silverline", "Marrow Point", "Eastonville", "Cragmuir"]
SURNAMES = ["Trask", "Mellor", "Quintrell", "Ashby", "Drummond", "Voss",
            "Pellworth", "Ingram", "Sorrel", "Blythe", "Cardew", "Hale"]
GIVEN = ["Marlowe", "Edwina", "Casper", "Ione", "Bartholomew", "Sylvie",
         "Ferdinand", "Petra", "Lionel", "Maren", "Osric", "Delia"]

ATTRS = ["founded", "city", "director", "staff"]


def _passage(r, name, facts, omit=None):
    """Render a short prose passage for an org; omit one attribute if asked."""
    parts = [f"The {name} "]
    order = [a for a in ATTRS if a != omit]
    r.shuffle(order)
    clauses = []
    for a in order:
        if a == "founded":
            clauses.append(f"was established in {facts['founded']}")
        elif a == "city":
            clauses.append(f"operates out of {facts['city']}")
        elif a == "director":
            clauses.append(f"is directed by {facts['director']}")
        elif a == "staff":
            clauses.append(f"employs roughly {facts['staff']} people")
    parts.append(", ".join(clauses[:-1]) + f", and {clauses[-1]}.")
    return "".join(parts)


def _facts(r, base_year=None):
    year = (base_year or r.randint(1921, 2009))
    return {
        "founded": year,
        "city": r.choice(CITIES),
        "director": f"{r.choice(GIVEN)} {r.choice(SURNAMES)}",
        "staff": r.choice([40, 65, 90, 120, 210, 340, 480]),
    }


QUESTION = {
    "founded": "According to the passages, in what year was the {name} established?",
    "city": "According to the passages, in what city does the {name} operate?",
    "director": "According to the passages, who directs the {name}?",
    "staff": "According to the passages, roughly how many people does the {name} employ?",
}

# Two-hop variant: the entity is referenced by a property stated only in
# the witness passage (its director, or its city when asking about the
# director). Distractor directors share surnames, so the hop must resolve
# an exact full name under heavy interference — the chaos-bank failure
# shape, not a string lookup.
QUESTION_2HOP = {
    "founded": "According to the passages, in what year was the organization directed by {hook} established?",
    "city": "According to the passages, in what city does the organization directed by {hook} operate?",
    "staff": "According to the passages, roughly how many people does the organization directed by {hook} employ?",
    "director": "According to the passages, who directs the organization that operates out of {hook}?",
}


def make_item(seed, n_distractors=3, confusable=True, absent=False,
              two_hop=False):
    """One item: passages (shuffled), question, gold value, wrong values.

    confusable=True gives distractor orgs the SAME leading name word as the
    witness (Calloway Institute vs Calloway Trust) and founded-years within
    a few years of the witness — the confusion the chaos bank showed the
    model failing on.
    """
    r = random.Random(seed)
    first = r.choice(FIRST)
    kinds = r.sample(KIND, n_distractors + 1)
    witness_name = f"{first} {kinds[0]}"
    attr = ATTRS[seed % len(ATTRS)]
    wfacts = _facts(r)

    hook_attr = "director" if attr != "director" else "city"

    passages, wrong_values = [], []
    passages.append(_passage(r, witness_name, wfacts,
                             omit=attr if absent else None))
    seen_hooks = {wfacts[hook_attr]}
    for i in range(n_distractors):
        dfirst = first if confusable else r.choice([f for f in FIRST if f != first])
        dname = f"{dfirst} {kinds[i + 1]}"
        base_year = wfacts["founded"] + r.choice([-4, -3, 3, 4, 6]) if confusable else None
        dfacts = _facts(r, base_year=base_year)
        # the asked value must differ, and the hook property must be unique
        # per passage or the two-hop reference is ambiguous
        while dfacts[attr] == wfacts[attr] or dfacts[hook_attr] in seen_hooks:
            dfacts = _facts(r)
        seen_hooks.add(dfacts[hook_attr])
        passages.append(_passage(r, dname, dfacts))
        wrong_values.append(str(dfacts[attr]))
    r.shuffle(passages)

    if two_hop:
        question = QUESTION_2HOP[attr].format(hook=wfacts[hook_attr])
    else:
        question = QUESTION[attr].format(name=witness_name)

    return {
        "id": f"synth-{seed}{'-absent' if absent else ''}{'-2h' if two_hop else ''}",
        "passages": passages,
        "question": question,
        "gold": None if absent else str(wfacts[attr]),
        "wrong": wrong_values,
        "attr": attr,
        "absent": absent,
    }


def render_user(item):
    body = "\n\n".join(f"Passage {i + 1}: {p}" for i, p in enumerate(item["passages"]))
    return f"{body}\n\nQuestion: {item['question']}\nAnswer briefly."


# Stems cover do/does/don't/doesn't variants ("do not say", "doesn't say"...)
ABSTAIN_MARKERS = ["not in the passage", "not mention", "not contain",
                   "not say", "no information", "not provide", "not state",
                   "not specif", "not appear", "not include", "not given",
                   "cannot", "can't", "don't know", "unknown", "unclear",
                   "no relevant"]


def _has(value, text):
    """Word-boundary match so '90' never matches inside '1990'."""
    import re
    return re.search(rf"(?<![\w]){re.escape(value.lower())}(?![\w])", text) is not None


def score(item, reply):
    """'correct' | 'wrong' (distractor value or abstain-on-present) | 'other'."""
    rl = reply.lower()
    abstained = any(m in rl for m in ABSTAIN_MARKERS)
    if item["absent"]:
        if abstained:
            return "correct"
        return "wrong" if any(_has(w, rl) for w in item["wrong"]) else "other"
    if _has(item["gold"], rl) and not any(_has(w, rl) for w in item["wrong"]):
        return "correct"
    if abstained or any(_has(w, rl) for w in item["wrong"]):
        return "wrong"
    return "other"
