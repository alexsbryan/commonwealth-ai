"""score.py: the register (ship S4), scored the same way for the system's records and the one-shot's.

    score.py <gold rules.json> <map.json> --atoms <atlas atoms.json> <out.json>
    score.py <gold rules.json> <map.json> --oneshot <oneshot records.json> <out.json>

A gold rule's records are the records of the mapped rule type that are that rule: the id (`can-` and 12 hex) a
record names most, anywhere in an attribute, its name or a cited quote (`rule_of`), so the scorer needs no
knowledge of the recipe's fields. The
map (frozen once the recipe is written, before any read) names the rule type, the attribute that holds a rule's
standing, and which of its values mean active, proposed, superseded and retracted.

Main count: rules whose records serve the gold status. Several records naming one rule must serve the same
status, or the rule counts as split, not right. A superseded rule also scores its replacement: a record naming the
gold `superseded_by` id.
"""
import collections, json, re, sys

CAN = re.compile(r"can-[0-9a-f]{12}")


def records_from_atoms(path, rtype):
    """A record and the claims about it (their `subject`): the id may be in either."""
    sys.path.insert(0, __file__.rsplit("/", 2)[0])
    import ladder as L
    atoms = json.load(open(path))["atoms"]
    about = collections.defaultdict(list)
    for a in atoms:
        if a.get("atom_type") == "Claim" and (a.get("data") or {}).get("subject"):
            about[a["data"]["subject"]].append(a)
    return [{"attributes": d.get("attributes") or {}, "name": d.get("canonical_name") or "",
             "blob": json.dumps([d] + about[i])} for i, d in L.records_of(atoms).items() if d["record_type"] == rtype]


def records_from_oneshot(path, rtype):
    recs = json.load(open(path))["records"]
    return [{"attributes": r.get("attributes") or {}, "name": r.get("name") or "", "blob": json.dumps(r)}
            for r in recs if r.get("type") == rtype]


def rule_of(rec):
    """The one rule a record is: the id it names most, ties to the id in its own name, then to the first named.
    A record also names other rules (the one that replaced it, a claim quoting both), so naming is not being."""
    found = CAN.findall(rec["blob"])
    if not found:
        return None
    n = collections.Counter(found)
    return max(n, key=lambda i: (n[i], i in rec["name"], -found.index(i)))


def status_of(rec, m):
    v = rec["attributes"].get(m["status_attribute"])
    if isinstance(v, dict):
        v = v.get("value", v.get("state"))
    return m["values"].get(str(v).strip().lower()) if v is not None else None


def main():
    gold_path, map_path, mode, rec_path, out_path = sys.argv[1:6]
    gold = json.load(open(gold_path))["rules"]
    m = json.load(open(map_path))
    m["values"] = {k.lower(): v for k, v in m["values"].items()}
    recs = (records_from_atoms if mode == "--atoms" else records_from_oneshot)(rec_path, m["rule_type"])
    for r in recs:
        r["rule"] = rule_of(r)
    out, rows = collections.Counter(), []
    for g in gold:
        mine = [r for r in recs if r["rule"] == g["id"]]
        served = {status_of(r, m) for r in mine}
        if not mine:
            verdict = "no record"
        elif len(served) > 1:
            verdict = "split"
        elif served == {g["status"]}:
            verdict = "right"
        else:
            verdict = "wrong"
        replaced = None
        if g["status"] == "superseded" and g.get("superseded_by"):
            replaced = any(g["superseded_by"] in r["blob"] for r in mine)
        out[verdict] += 1
        out[f"{g['status']}:{verdict}"] += 1
        if replaced is not None:
            out["replacement named"] += replaced
        rows.append({"id": g["id"], "project": g["project"], "gold": g["status"], "served": sorted(map(str, served)),
                     "records": len(mine), "verdict": verdict, "replacement_named": replaced})
    res = {"rules": len(gold), "right": out["right"], "counts": dict(out), "records_of_type": len(recs), "rows": rows}
    json.dump(res, open(out_path, "w"), indent=1)
    print(f"{rec_path}: {out['right']}/{len(gold)} rules at the right standing; {dict(out)}")


if __name__ == "__main__":
    main()
