#!/usr/bin/env python3
"""Generator-familiarity control: mine REAL model-produced claims from chaos
transcripts into verifier cases with MECHANICAL labels.

Why this exists (kill-condition guard, note 648e47be): the headroom study's
corruptions come from our own generator, which rung-1000 trained against.
This bank's fabrications were produced by live models answering bank
questions — no corruption generator involved — so an edge that survives here
is not generator-signature memorization.

Label rule (mechanical, judge-free, evidence-relative — the gate's own job
definition):
  1. Claims are extracted from (question, answer) by the PRODUCTION
     `extract_claim_list` register via the daemon (local_only pinned).
  2. A claim's "asserted values" = capitalized name runs, quoted strings,
     and numbers appearing in the claim but NOT in the question (the new
     information the claim asserts).
  3. label = grounded iff EVERY asserted value appears (normalized) in the
     turn's retrieved chunks; ungrounded iff AT LEAST ONE is absent.
     Claims with no asserted values are DROPPED (nothing checkable), as are
     claims whose values all appear in the QUESTION only.
  4. Dedupe on normalized claim text across runs of the same bank.

Bias stated openly: this restricts to value-anchored claims (names, numbers,
quoted titles). Relational corruptions (negation flips) are not measurable
here — the control speaks to entity/value fabrication, which is both the
dominant production failure mode and the incumbent's measured blind spot.

Output rows are headroom_study.py-compatible: {id, kind, label, claim,
evidence_chunks, provenance}. kind = real_fab | real_grounded.
"""
import argparse
import glob
import json
import re
import sys
import unicodedata
import urllib.request

EXTRACT_SYSTEM = "You extract claims precisely. Reply with up to {n} lines, or NO_CLAIM."
# `grounding/judge.rs:397-411` (extract_claim_list), verbatim across the
# language boundary — same citation discipline as headroom_study.py.
EXTRACT_PROMPT = (
    "A user asked: {question}\n\nAn assistant wrote this long answer:\n\"\"\"\n{answer}\n\"\"\"\n\n"
    "List the SPECIFIC factual claims the answer asserts — concrete who/what/when "
    "relations a passage could confirm or refute (names, identifications, events, "
    "attributions). One claim per line, each a short standalone sentence naming "
    "both sides of the relation. At most {n} lines; pick the most load-bearing "
    "claims, and when the answer is long, sample across ALL of it — include "
    "specific claims from the later sections, not only the opening. Skip "
    "opinions, summaries of the question, and anything the answer itself flags "
    "as not from the sources.\n"
    "Reply with exactly NO_CLAIM if there are no such checkable claims."
)
MAX_CLAIMS = 4


def norm(s):
    s = unicodedata.normalize("NFKD", s)
    s = "".join(c for c in s if not unicodedata.combining(c))
    return re.sub(r"[^a-z0-9 ]", " ", s.lower())


def asserted_values(claim, question):
    """Name runs, quoted strings, numbers in the claim but not the question."""
    vals = []
    vals += re.findall(r'"([^"]{2,60})"', claim)
    vals += re.findall(r"[“‘']([^”’']{2,60})[”’']", claim)
    # capitalized runs (skip sentence-initial single word)
    for m in re.finditer(r"\b([A-Z][a-z]+(?:\s+[A-Z][a-z]+)+|\b[A-Z][a-z]{2,}\b)", claim):
        if m.start() == 0 and " " not in m.group(1):
            continue
        vals.append(m.group(1))
    vals += re.findall(r"\b\d[\d,.]*\b", claim)
    qn = norm(question)
    out = []
    for v in vals:
        nv = norm(v).strip()
        if len(nv) < 3 or nv in qn:
            continue
        out.append((v, nv))
    return out


def extract_claims(daemon_url, question, answer, timeout=300):
    body = json.dumps({
        "model": "primary",
        "oicp": {"oicp_version": "0.4.0",
                 "privacy": {"sharding": "local_only"}},
        "messages": [
            {"role": "system", "content": EXTRACT_SYSTEM.format(n=MAX_CLAIMS)},
            {"role": "user", "content": EXTRACT_PROMPT.format(
                question=question[:400], answer=answer[:14000], n=MAX_CLAIMS)},
        ],
        "max_tokens": 300,
        "temperature": 0.0,
        "think_budget": 0,
        "chat_template_kwargs": {"enable_thinking": False},
    }).encode()
    req = urllib.request.Request(f"{daemon_url}/chat/completions", data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        out = json.load(r)
    text = out["choices"][0]["message"]["content"].strip()
    if "NO_CLAIM" in text:
        return []
    return [l.strip("-• \t") for l in text.splitlines()
            if l.strip() and len(l.strip()) > 15][:MAX_CLAIMS]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--transcripts", nargs="+", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--daemon-url", default="http://127.0.0.1:9741/v1")
    args = ap.parse_args()

    files = []
    for pat in args.transcripts:
        files += glob.glob(pat)
    seen = set()
    cases, dropped_no_value, extract_fail = [], 0, 0
    for f in sorted(files):
        for li, line in enumerate(open(f)):
            r = json.loads(line)
            qt, aa = r.get("qtype"), r.get("agent_action")
            q, ans = r.get("question"), r.get("answer")
            chunks = r.get("retrieved_chunks") or []
            if not (qt and aa == "Answered" and q and ans and chunks):
                continue
            try:
                claims = extract_claims(args.daemon_url, q, ans)
            except Exception as e:
                extract_fail += 1
                print(f"  extract failed {f}:{li}: {str(e)[:80]}", file=sys.stderr)
                continue
            ev_norm = norm("\n".join(chunks))
            for ci, claim in enumerate(claims):
                key = norm(claim).strip()
                if key in seen:
                    continue
                vals = asserted_values(claim, q)
                if not vals:
                    dropped_no_value += 1
                    continue
                missing = [v for v, nv in vals if nv not in ev_norm]
                label = "ungrounded" if missing else "grounded"
                seen.add(key)
                cases.append({
                    "id": f"ctl-{len(cases):04d}",
                    "kind": "real_fab" if missing else "real_grounded",
                    "label": label,
                    "claim": claim,
                    "evidence_chunks": chunks,
                    "provenance": {"file": f.rsplit('/', 1)[-1], "row": li,
                                   "qtype": qt, "asserted_values": [v for v, _ in vals],
                                   "missing_values": missing},
                })
    with open(args.out, "w") as f:
        for c in cases:
            f.write(json.dumps(c, ensure_ascii=False) + "\n")
    import collections
    print(json.dumps({
        "cases": len(cases),
        "by_kind": dict(collections.Counter(c["kind"] for c in cases)),
        "by_qtype": dict(collections.Counter(
            (c["provenance"]["qtype"], c["kind"]) and
            f"{c['provenance']['qtype']}/{c['kind']}" for c in cases)),
        "dropped_no_value": dropped_no_value,
        "extract_failures": extract_fail,
    }, indent=1))


if __name__ == "__main__":
    main()
