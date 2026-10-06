#!/usr/bin/env python3
"""The precision of an evidential document field under gold, no model (resolve-prereg.md, Ring 1a).

The rule measured is the one RESOLVE applies (resolve_records/fields.rs): documents in the order the
statements file first names them; a statement's document holds a value of the field; if exactly one gold
chain has a statement in an EARLIER document holding that value, the field links the statement to it.
precision = links whose chain is the statement's own / links; coverage = links / statements. Values are read
as `read_stamp` reads them: a thread verbatim, a date as ISO 8601 (an instant in UTC to the second, or a day).

  field_precision.py DOCUMENTS STATEMENTS GOLD --stamp thread --field thread
"""
import argparse, datetime, email.utils, json


def stamp_value(raw, stamp):
    if raw is None or isinstance(raw, (list, dict, bool)):
        return None
    s = str(raw).strip()
    if not s:
        return None
    if stamp != "date":
        return s
    utc = lambda d: d.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")  # noqa: E731
    try:
        d = datetime.datetime.fromisoformat(s.replace("Z", "+00:00"))
        if d.tzinfo:
            return utc(d)
        if len(s) == 10:
            return d.strftime("%Y-%m-%d")
        return d.strftime("%Y-%m-%dT%H:%M:%S")
    except ValueError:
        pass
    try:
        return utc(email.utils.parsedate_to_datetime(s))
    except (TypeError, ValueError):
        return None


def main():
    a = argparse.ArgumentParser()
    a.add_argument("documents"); a.add_argument("statements"); a.add_argument("gold")
    a.add_argument("--stamp", choices=["thread", "date"], required=True)
    a.add_argument("--field", required=True, help="the metadata field change.document names for the stamp")
    a = a.parse_args()
    gold = json.load(open(a.gold))
    stmts, order = {}, []
    for line in open(a.statements):
        s = json.loads(line)
        if s["document"] not in stmts:
            order.append(s["document"]); stmts[s["document"]] = []
        stmts[s["document"]].append(s["id"])
    want = set(order)
    value = {}
    for line in open(a.documents):
        d = json.loads(line)
        if d["id"] in want:
            value[d["id"]] = stamp_value(d.get(a.field), a.stamp)
    held = {}  # value -> gold chains with a statement in an earlier document holding it
    n = links = right = unread = 0
    for doc in order:
        v = value.get(doc)
        unread += v is None
        chains = held.get(v, set()) if v is not None else set()
        for sid in stmts[doc]:
            n += 1
            if len(chains) == 1:
                links += 1
                right += next(iter(chains)) == gold.get(sid)
        if v is not None:
            held.setdefault(v, set()).update(gold[s] for s in stmts[doc] if gold.get(s) is not None)
    print(json.dumps({"stamp": a.stamp, "field": a.field, "documents": len(order), "unread": unread,
                      "statements": n, "links": links, "right": right,
                      "precision": round(right / links, 3) if links else None,
                      "coverage": round(links / n, 3) if n else None}))


if __name__ == "__main__":
    main()
