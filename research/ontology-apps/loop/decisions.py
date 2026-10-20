"""decisions.py <run dir> <corpus>: every RESOLVE question of a run, judged against gold.

RESOLVE asks, per statement it cannot key, which offered record the statement is about, or none. This judges each
asked question by the gold identity of window particulars (misses.py's: the deal most of a particular's stage
statements match): the asked statement's particular, and each offered record's opening particular. The right
answer is the offered record of the asked particular's deal, or none when no offered record is it. Verdicts:
  right_link / right_open        decided as gold says
  wrong_link                     joined another deal's record (`offered_right` says whether its own was offered)
  wrong_open                     opened a record though its deal's record was offered
  veto_overturned_right          the model picked its deal's record; the weighed sources' veto opened one instead
  veto_saved                     the model picked another deal's record and the veto opened one, rightly (its own
                                 record was not offered)
  veto_still_wrong               the veto opened a record over a wrong pick while its own record was offered
  failed                         no answer (the call failed or was refused)
  unjudged                       the asked particular matches no gold deal
Writes <run dir>/decisions.jsonl; prints the counts.
"""
import json, os, re, sys
from collections import Counter
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import corpora, misses

UNIT = re.compile(r'#\\?"([^"\\]+)\\?"')


def unit(statement_or_record):
    m = UNIT.search(statement_or_record or "")
    return tuple(m.group(1).split("/", 1)) if m else None


def parse(run):
    """{statement: {alternatives, distribution, decided, vetoed, decision, record, failed}} from resolve.err."""
    Q = {}
    for line in open(f"{run}/resolve.err", errors="replace"):
        m = re.search(r'statement="?(\S+?#\\?"[^"\\]+\\?")"?', line)
        if not m: continue
        q = Q.setdefault(m.group(1).replace('\\"', '"'), {})
        if "the sources weighed" in line:
            m2 = re.search(r"alternatives=\[(.*?)\] comparisons", line)
            if m2: q["alternatives"] = [a for a in re.findall(r'"([^"]*?\\"[^"\\]+\\")"', m2.group(1))]
        elif "resolve_select" not in line and "decision call" in line: continue  # a READ of a necessary value
        elif "decision call failed" in line: q["failed"] = True
        elif "decision call document" in line:
            m2 = re.search(r"distribution=(\{.*?\})", line)
            if m2: q["distribution"] = json.loads(m2.group(1))
        elif "the model's choice decides" in line:
            m2 = re.search(r"decided=(\w+)(?:\((\d+), ([\d.e-]+)\))? weighed=(\S+) vetoed=(\S+)", line)
            if m2: q.update(decided=m2.group(1), index=m2.group(2) and int(m2.group(2)), vetoed=m2.group(5))
        elif re.search(r"\bdecided document=", line):
            m2 = re.search(r'decision="([\w:]+)" record="(.*?)" outcome', line)
            if m2: q.update(decision=m2.group(1), record=m2.group(2))
    return {s: q for s, q in Q.items() if "decision" in q and ("distribution" in q or q.get("failed") or "decided" in q)}


def judge(D, s, q):
    ident = lambda u: D["ident"].get(u) if u else None
    want = ident(unit(s))
    if q.get("failed") or q.get("decision", "").startswith("refused"): return "failed", want, None
    if want is None: return "unjudged", None, None
    alts = [ident(unit(a)) for a in q.get("alternatives", [])]
    offered_right = want in alts
    dist = q.get("distribution") or {}
    labels = [k for k in dist if k != "0"]
    pick = max(dist, key=dist.get) if dist else None
    picked = None if pick in (None, "0") else (alts[labels.index(pick)] if pick in labels and labels.index(pick) < len(alts) else "?")
    got = ident(unit(q.get("record"))) if q.get("decision") in ("chosen", "key", "weighed", "joined") else None
    if q.get("vetoed") not in (None, "None"):
        v = "veto_overturned_right" if picked == want else "veto_saved" if not offered_right else "veto_still_wrong"
        return v, want, dict(offered_right=offered_right, picked=picked)
    if q.get("decision") == "opened":
        return ("wrong_open" if offered_right else "right_open"), want, dict(offered_right=offered_right, picked=picked)
    return ("right_link" if got == want else "wrong_link"), want, dict(offered_right=offered_right, got=got, picked=picked)


if __name__ == "__main__":
    run, cid = sys.argv[1], sys.argv[2]
    corpora.refuse_held_out(cid, "decisions.py")
    D = misses.load(run, cid)
    rows = []
    for s, q in parse(run).items():
        v, want, extra = judge(D, s, q)
        rows.append(dict(statement=s, unit="/".join(unit(s) or ("?", "?")), is_=want, verdict=v, decision=q.get("decision"),
                         record=q.get("record"), vetoed=q.get("vetoed"), **(extra or {})))
    with open(f"{run}/decisions.jsonl", "w") as f:
        for r in rows: f.write(json.dumps(r) + "\n")
    c = Counter(r["verdict"] for r in rows)
    judged = sum(n for k, n in c.items() if k not in ("unjudged", "failed"))
    right = c["right_link"] + c["right_open"] + c["veto_saved"]
    print(f"{cid}: {len(rows)} asked, {judged} judged, {right} right ({right / judged:.0%})" if judged else f"{cid}: {len(rows)} asked, none judged")
    wl = Counter(r.get("offered_right") for r in rows if r["verdict"] == "wrong_link")
    print("  " + ", ".join(f"{k} {n}" for k, n in c.most_common()) + f"   (wrong_link with its own record offered: {wl[True]})")
