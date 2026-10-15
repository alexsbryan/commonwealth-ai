"""score_bank.py <records.json> <bank.toml> <out.json>: does a record set carry a question bank's facts?

For a corpus whose gold is a question bank (saltgrass: bench/lanes/chaos_monkey/saltgrass.toml), not deal
records. Every question is asked of the records alone, by one fixed answerer for every arm (the daemon's
model, temperature 0, no reasoning, one prompt), and judged by the bank's own rule:
  present / partially_present / provenance_trap  right when every gold keyword is in the answer
  distractor                                     right when the gold keywords are in it and the distractor is not
  absent_adjacent / absent_out_of_domain         right when the answer is NOT IN RECORDS (an answer is invention)
A failed call is could_not_judge, never wrong. <records.json> is {"records": [{type, name, attributes|fields,
claims: [{kind, quote, attributes}]}]}: a one-shot's file as runs/ceiling/oneshot.py writes it, or the system's
records as records_of.py dumps them through the public atlas-query. Writes per-question answers and verdicts.
"""
import json, sys, tomllib, urllib.request

DAEMON = "http://localhost:9741/v1/chat/completions"
NONE = "NOT IN RECORDS"
PROMPT = ("Answer the question from these records only. They are all you know. Answer in one or two sentences "
          "and name the records you used by their number. If the records do not say, answer exactly: "
          f"{NONE}\n\nRecords:\n{{records}}\n\nQuestion: {{question}}")


def render(records):
    out = []
    for n, r in enumerate(records, 1):
        attrs = r.get("attributes") or r.get("fields") or {}
        head = f"[{n}] {r.get('type')}: {r.get('name')}" + (f" ({json.dumps(attrs, ensure_ascii=False)})" if attrs else "")
        lines = [head] + [f"    {c.get('kind')}: {c.get('quote')!r}"
                          + (f" {json.dumps(c.get('attributes'), ensure_ascii=False)}" if c.get("attributes") else "")
                          for c in r.get("claims", [])]
        out.append("\n".join(lines))
    return "\n".join(out)


def ask(text, question):
    body = {"model": "commonwealth/primary", "temperature": 0, "max_tokens": 400, "thinking": {"type": "disabled"},
            "think_budget": 0, "messages": [{"role": "user", "content": PROMPT.format(records=text, question=question)}]}
    req = urllib.request.Request(DAEMON, json.dumps(body).encode(), {"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=300))["choices"][0]["message"]["content"].strip()


def judge(q, answer):
    a = answer.lower()
    has = lambda ks: all(k.lower() in a for k in ks)
    t = q["qtype"]
    if t.startswith("absent"):
        return "right" if NONE.lower() in a else "wrong"
    if t == "distractor":
        return "right" if has(q["gold_keywords"]) and q["distractor_quote"].lower() not in a else "wrong"
    return "right" if has(q["gold_keywords"]) else "wrong"


if __name__ == "__main__":
    records = json.load(open(sys.argv[1]))["records"]
    bank = tomllib.load(open(sys.argv[2], "rb"))["questions"]
    text = render(records)
    rows = []
    for q in bank:
        try:
            ans = ask(text, q["question"])
            v = judge(q, ans)
        except Exception as e:  # a failed call judges nothing
            ans, v = f"call failed: {e}", "could_not_judge"
        rows.append(dict(id=q["id"], qtype=q["qtype"], verdict=v, answer=ans))
    by = {}
    for r in rows:
        by.setdefault(r["qtype"], {}).setdefault(r["verdict"], 0)
        by[r["qtype"]][r["verdict"]] += 1
    summary = dict(records=len(records), record_chars=len(text), right=sum(r["verdict"] == "right" for r in rows),
                   judged=sum(r["verdict"] != "could_not_judge" for r in rows), by_qtype=by)
    json.dump(dict(summary=summary, questions=rows), open(sys.argv[3], "w"), indent=1)
    print(json.dumps(summary, indent=1))
