"""Estimator replay (cast-probe/PREREG.md amendment 2): RESOLVE's weighing replayed from a leg's trace, first order,
with no model call, before and after correction C (a document-field source does not speak on a pair whose
statement's document the alternative record already holds).

The fit and the zone are ported line for line from resolve_records/estimate.rs (Estimate::fit, evidence, supports)
and weigh.rs (weigh, no bar). Gates G1 (every logged fit reproduced) and G2 (logged zones reproduced) run first.

    estimator_replay.py [leg ...]
"""
import collections
import json
import math
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import cast_probe as P  # noqa: E402  legs, gold particulars per statement, handles

ANSI = re.compile(r"\x1b\[[0-9;]*m")
WEIGHED = re.compile(r'atlas/resolve: the sources weighed document="([^"]+)" statement=(.*?) alternatives=(\[.*\]) '
                     r'comparisons=(\[.*\]) zone=(\w+)(?:\((\d+), [\d.e-]+\))?\s*$')
DECIDED = re.compile(r'atlas/resolve: decided document="([^"]+)" statement=(.*?) decision="(\w+)" record="((?:[^"\\]|\\.)*)"')
RESOLVED = re.compile(r'atlas/resolve: document resolved document="([^"]+)"')
FITTED = re.compile(r'atlas/resolve estimate: agreement weights fitted pairs=(\d+) prior=([\d.e-]+) .*sources=\{(.*)\}\s*$')
SOURCE = re.compile(r'"([^"]+)": SourceWeight \{ m: ([\d.e-]+), u: ([\d.e-]+),')

START_PRIOR, START_M, START_U, TOLERANCE, MAX_ITERATIONS, EDGE = 0.1, 0.9, 0.1, 1e-9, 500, 1e-6


def clamp(p):
    return min(max(p, EDGE), 1.0 - EDGE)


def identical(patterns):
    names = sorted({s for p in patterns for s, _ in p})
    say = lambda p, s: next((a for x, a in p if x == s), None)  # noqa: E731
    grouped = []
    for i, a in enumerate(names):
        if any(a in g for g in grouped):
            continue
        group = [a] + [b for b in names[i + 1:] if all(say(p, a) == say(p, b) for p in patterns)]
        if len(group) > 1:
            grouped.append(group)
    return grouped


def fit(patterns):
    """Estimate::fit. patterns: {tuple((source, agree), ...) sorted: count}. -> (prior, {source: (m, u)}, groups)."""
    groups = identical(patterns)
    folded = collections.Counter()
    for p, n in patterns.items():
        folded[tuple(x for x in p if not any(x[0] in g[1:] for g in groups))] += n
    total = sum(folded.values())
    rates = {s: (START_M, START_U) for p in folded for s, _ in p}
    prior = START_PRIOR
    if total > 0:
        for _ in range(MAX_ITERATIONS):
            matched, seen = 0.0, collections.defaultdict(lambda: [0.0, 0.0, 0.0, 0.0])
            for p, n in sorted(folded.items()):
                lm, lu = math.log(prior), math.log(1.0 - prior)
                for s, a in p:
                    m, u = rates[s]
                    lm += math.log(m) if a else math.log(1.0 - m)
                    lu += math.log(u) if a else math.log(1.0 - u)
                g = 1.0 / (1.0 + math.exp(lu - lm))
                matched += n * g
                for s, a in p:
                    e = seen[s]
                    e[0] += n * g * a
                    e[1] += n * g
                    e[2] += n * (1.0 - g) * a
                    e[3] += n * (1.0 - g)
            moved = 0.0
            nxt = clamp((matched + 1.0) / (total + 2.0))
            moved = max(moved, abs(nxt - prior))
            prior = nxt
            for s in sorted(rates):
                e = seen.get(s, [0.0, 0.0, 0.0, 0.0])
                m, u = clamp((e[0] + 1.0) / (e[1] + 2.0)), clamp((e[2] + 1.0) / (e[3] + 2.0))
                if m < u:
                    pooled = clamp((e[0] + e[2] + 1.0) / (e[1] + e[3] + 2.0))
                    m, u = pooled, pooled
                moved = max(moved, abs(m - rates[s][0]), abs(u - rates[s][1]))
                rates[s] = (m, u)
            if moved < TOLERANCE:
                break
        if sum(m - u for m, u in rates.values()) < 0.0:
            prior = 1.0 - prior
            rates = {s: (u, m) for s, (m, u) in rates.items()}
    else:
        prior = 0.5
        rates = {s: (0.5, 0.5) for s in rates}
    out = {}
    for s, mu in rates.items():
        for member in next((g for g in groups if g[0] == s), [s]):
            out[member] = mu
    with_ = {m: [x for x in g if x != m] for g in groups for m in g}
    return prior, out, with_


def evidence(est, c):
    prior, rates, with_ = est
    total = 0.0
    for s, a in c.items():
        if any(x < s and x in c for x in with_.get(s, [])):
            continue
        if s in rates:
            m, u = rates[s]
            total += math.log(m / u) if a else math.log((1.0 - m) / (1.0 - u))
    return total


def supports(est, c):
    rates = est[1]
    return any(a and s in rates and math.log(rates[s][0] / rates[s][1]) > 0.0 for s, a in c.items())


def zone(est, comps):
    """weigh() with no bar: the index linked, or None."""
    if not comps:
        return None
    prior = est[0]
    plo = math.log(prior / (1.0 - prior))
    score = [evidence(est, c) + plo for c in comps]
    raised = [supports(est, c) for c in comps]
    top = max([0.0] + score)
    total = math.exp(-top) + sum(math.exp(s - top) for s in score)
    post = [math.exp(s - top) / total for s in score]
    none = math.exp(-top) / total
    if not any(raised):
        return None
    best = max(range(len(post)), key=lambda i: (post[i], -i))
    p = post[best]
    if raised[best] and p > none + 1e-12 and all(i == best or q + 1e-12 < p for i, q in enumerate(post)):
        return best
    return None


def key(c):
    return tuple(sorted(c.items()))


def replay(leg):
    system = P.W.LEGS[leg][0]
    docs = P.documents(leg, system)
    part = P.particulars(system, docs)
    events = []
    for raw in open(P.RUN / leg / "resolve.err.log", errors="replace"):
        line = ANSI.sub("", raw)
        if m := WEIGHED.search(line):
            events.append(("weighed", m[1], m[2], json.loads(m[3]), json.loads(m[4]),
                           int(m[6]) if m[5] == "Link" else None))
        elif m := DECIDED.search(line):
            events.append(("decided", m[1], m[2], m[3], json.loads('"' + m[4] + '"')))
        elif m := RESOLVED.search(line):
            events.append(("resolved", m[1]))
        elif m := FITTED.search(line):
            events.append(("fitted", int(m[1]), float(m[2]), {s: (float(a), float(b)) for s, a, b in SOURCE.findall(m[3])}))
    rec_docs = collections.defaultdict(set)
    rec_parts = collections.defaultdict(set)  # record -> gold particulars of the statements it holds so far
    arms = {"before": collections.Counter(), "after": collections.Counter()}
    est = {"before": fit({}), "after": fit({})}
    seen = {"before": [], "after": []}
    patterns = {"before": collections.Counter(), "after": collections.Counter()}
    g1 = [0, 0]  # fits reproduced, fits
    g2 = [0, 0]  # zones reproduced, weighed statements
    links = {"before": {}, "after": {}}
    for ev in events:
        if ev[0] == "weighed":
            _, doc, stmt, alts, comps, logged = ev
            sdoc = P.handle(stmt)[0]
            got = zone(est["before"], comps)
            g2[1] += 1
            g2[0] += got == logged
            fixed = [{k: v for k, v in c.items() if not (k.startswith("document_") and sdoc in rec_docs[r])}
                     for c, r in zip(comps, alts)]
            for arm, cs in (("before", comps), ("after", fixed)):
                z = zone(est[arm], cs)
                links[arm][stmt] = alts[z] if z is not None else None
                seen[arm] += [c for c in cs if c]
        elif ev[0] == "decided":
            _, doc, stmt, decision, record = ev
            ps = part(*P.handle(stmt))
            for arm in arms:
                linked = links[arm].pop(stmt, None)
                if linked is None:
                    continue
                c = arms[arm]
                c["links"] += 1
                if not ps:
                    c["unjudged"] += 1
                elif ps & rec_parts[linked]:
                    c["right"] += 1
                else:
                    c["wrong"] += 1
                arms[arm].setdefault("_right", set())
                if ps and ps & rec_parts[linked]:
                    arms[arm]["_right"].add(stmt)
                elif ps:
                    arms[arm].setdefault("_wrong", set())
                    arms[arm]["_wrong"].add(stmt)
            rec_docs[record].add(P.handle(stmt)[0])
            rec_parts[record] |= ps
        elif ev[0] == "fitted":
            _, pairs, prior, logged = ev
            if pairs == 0:  # a new type's resolver starts from no pairs
                for arm in arms:
                    patterns[arm], seen[arm], est[arm] = collections.Counter(), [], fit({})
            else:  # resolve_records.rs: the document's pairs are added and the estimate refit, then logged
                for arm in arms:
                    for c in seen[arm]:
                        patterns[arm][key(c)] += 1
                    est[arm] = fit(patterns[arm])
                    seen[arm] = []
            mine = est["before"]
            g1[1] += 1
            g1[0] += all(s in mine[1] and abs(mine[1][s][0] - m) < 1e-6 and abs(mine[1][s][1] - u) < 1e-6
                         for s, (m, u) in logged.items()) and abs(mine[0] - prior) < 1e-6
    out = {"G1_fits_reproduced": g1, "G2_zones_reproduced": g2,
           "G1_passed": g1[0] == g1[1], "G2_passed": g2[1] > 0 and g2[0] >= 0.99 * g2[1]}
    for arm in arms:
        c = arms[arm]
        out[arm] = {k: c[k] for k in ("links", "right", "wrong", "unjudged")}
    rb, ra = arms["before"].get("_right", set()), arms["after"].get("_right", set())
    wb, wa = arms["before"].get("_wrong", set()), arms["after"].get("_wrong", set())
    out["right_gained"], out["right_lost"] = len(ra - rb), len(rb - ra)
    out["wrong_gained"], out["wrong_removed"] = len(wa - wb), len(wb - wa)
    return out


# ---------------------------------------------------------------- diagnostics added after C was read (no bar)
def calibration(leg):
    """Per source, the agreement rate on gold matches and non-matches among judged pairs beside EM's last fit, and
    the decider's judged links under the logged weights and under weights fitted on gold (an oracle ceiling, never
    a production candidate: it reads labels)."""
    system = P.W.LEGS[leg][0]
    part = P.particulars(system, P.documents(leg, system))
    lines = [ANSI.sub("", raw) for raw in open(P.RUN / leg / "resolve.err.log", errors="replace")]
    rec_parts, t, nm, nn, em = collections.defaultdict(set), collections.defaultdict(lambda: [0, 0, 0, 0]), 0, 0, {}
    for line in lines:
        if m := WEIGHED.search(line):
            ps = part(*P.handle(m[2]))
            for comp, r in zip(json.loads(m[4]), json.loads(m[3])):
                if ps and rec_parts[r]:
                    match = bool(ps & rec_parts[r])
                    nm, nn = nm + match, nn + (not match)
                    for src, a in comp.items():
                        x = t[src]
                        x[0 if match else 2] += a
                        x[1 if match else 3] += 1
        elif m := DECIDED.search(line):
            rec_parts[json.loads('"' + m[4] + '"')] |= part(*P.handle(m[2]))
        elif (m := FITTED.search(line)) and int(m[1]) > 0:
            em = {src: (float(a), float(b)) for src, a, b in SOURCE.findall(m[3])}
    rates = {src: (clamp((a + 1) / (b + 2)), clamp((c + 1) / (d + 2))) for src, (a, b, c, d) in t.items()}
    rates = {src: (mu if mu[0] >= mu[1] else ((mu[0] + mu[1]) / 2,) * 2) for src, mu in rates.items()}
    oracle = ((nm + 1) / (nm + nn + 2), rates, {})
    rec_parts, pend, links = collections.defaultdict(set), {}, {"logged": collections.Counter(), "oracle": collections.Counter()}
    for line in lines:
        if m := WEIGHED.search(line):
            alts, comps = json.loads(m[3]), json.loads(m[4])
            z = zone(oracle, comps)
            pend[m[2]] = {"logged": alts[int(m[6])] if m[5] == "Link" else None, "oracle": alts[z] if z is not None else None}
        elif m := DECIDED.search(line):
            ps = part(*P.handle(m[2]))
            for arm, linked in pend.pop(m[2], {}).items():
                if linked is not None and ps:
                    links[arm]["right" if ps & rec_parts[linked] else "wrong"] += 1
            rec_parts[json.loads('"' + m[4] + '"')] |= ps
    return {"rates": {src: {"gold_m": [a, b], "gold_u": [c, d], "em_m_u": [round(x, 3) for x in em.get(src, (0, 0))]}
                      for src, (a, b, c, d) in sorted(t.items())},
            "judged_links_logged_weights": dict(links["logged"]), "judged_links_gold_weights_oracle": dict(links["oracle"])}


def main():
    legs = sys.argv[1:] or list(P.W.LEGS)
    report = {}
    for leg in legs:
        report[leg] = replay(leg) | {"calibration_diagnostic": calibration(leg)}
        print(json.dumps({leg: report[leg]}), flush=True)
    (P.DATA / "estimator-replay.json").write_text(json.dumps(report, indent=1) + "\n")


if __name__ == "__main__":
    main()
