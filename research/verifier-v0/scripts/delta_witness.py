#!/usr/bin/env python3
"""The damage witness, corrected for this text distribution.

control_mine.asserted_values was written for chaos-transcript prose and does
not transfer to SEP text. Three artifact classes, each with a demonstrated
example from runs/delta/revisions_fa20.jsonl, are removed. Named individually
so this reads as defect removal, not as tuning toward a wanted answer -- and
BOTH rates are reported by the caller (§18.6: never report a scorer change
only in the direction it was meant to fix).

  A1 CARRIED-OVER VALUES. "John Dewey" fired on a revision whose only change
     was 'richard Wollheim' -> 'Richard Wollheim'. The name was in the ORIGINAL
     claim, which is labelled grounded. Damage is what a revision ADDS; a value
     already in the original cannot be new damage, whatever the evidence says.
     FIX: subtract values present in the original claim.

  A2 SENTENCE-INITIAL RUNS. "Despite Avicenna" and "Under Mayo" fired because
     English capitalises sentence starts and the regex only skipped
     sentence-initial SINGLE words. FIX: skip the leading capitalised run
     entirely, and drop a leading stopword from any run.

  A3 POSSESSIVE APOSTROPHES. "s account claims Henry is negatively relevant to
     Bill" fired: the quoted-string regex matched between the apostrophe of
     "Bennett's" and that of "Bill's". FIX: quoted spans come from paired
     double quotes only.

What survives is the intended bar: a name, number or quoted title asserted by
the revision, absent from the evidence, and NOT carried over from the original.
"""
import re, sys, unicodedata
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from control_mine import norm   # unchanged normalisation

LEAD_STOP = {"the","a","an","in","on","at","by","for","of","to","from","with",
             "under","despite","although","though","however","thus","hence",
             "according","while","when","where","if","because","since","after",
             "before","but","and","or","so","this","that","these","those","it"}

def values(text):
    """Name runs, numbers and double-quoted spans asserted by `text`."""
    vals = []
    vals += re.findall(r'"([^"]{2,60})"', text)          # A3: paired doubles only
    for m in re.finditer(r"\b([A-Z][a-z]+(?:\s+[A-Z][a-z]+)*)", text):
        run = m.group(1)
        if m.start() == 0:                                # A2: skip sentence-initial
            continue
        first = run.split()[0]
        if first.lower() in LEAD_STOP:                    # A2: drop leading stopword
            run = " ".join(run.split()[1:])
        if len(run.split()) == 0:
            continue
        vals.append(run)
    vals += re.findall(r"\b\d[\d,.]*\b", text)
    out = []
    for v in vals:
        nv = norm(v).strip()
        if len(nv) < 3:
            continue
        out.append((v, nv))
    return out

def witness(revised, original, evidence_chunks):
    """Damaged iff the revision asserts a value absent from evidence AND absent
    from the original claim (which is labelled grounded, so it is the baseline)."""
    ev = norm(" ".join(evidence_chunks))
    orig = norm(original)
    vals = values(revised)
    absent = [v for v, nv in vals if nv not in ev and nv not in orig]  # A1
    return {"n_values": len(vals), "absent": absent,
            "damaged": len(absent) > 0, "checkable": len(vals) > 0}
