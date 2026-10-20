"""score_bank.py <records.json> <bank.toml> <out.json>: does a record set carry a question bank's facts?

For a corpus whose gold is a question bank (saltgrass: bench/lanes/chaos_monkey/saltgrass.toml), not deal
records. Every question is asked of the records alone, by one fixed answerer for every arm (the daemon's
model, temperature 0, no reasoning, one prompt), over five presentations of the same records (record order and
each record's statement order shuffled by fixed seeds), and its verdict is the majority of the five. One
presentation moved one record set's score by 3 of 42 (23-26 over seeds 1-5, 2026-10-16) while re-asking one
presentation moved it by none (26, 26, 26), so the answerer reads order, and one ask is not a measurement.
The presentations are asked in seed order and a question stops once one verdict has three of them: the majority
of five can no longer change, so the verdict is the full majority's at about two thirds of the calls.
Each presentation is judged by the bank's own rule:
  present / partially_present / provenance_trap  right when every gold keyword is in the answer
  distractor                                     right when the gold keywords are in it and the distractor is not
  absent_adjacent / absent_out_of_domain         right when the answer is NOT IN RECORDS (an answer is invention)
A failed call is could_not_judge, never wrong; so is a question with fewer than three judged presentations or
a tie among them. <records.json> is {"records": [{type, name, attributes|fields,
claims: [{kind, quote, attributes}]}]}: a one-shot's file as runs/ceiling/oneshot.py writes it, or the system's
records as records_of.py dumps them through the public atlas-query. Writes per-question answers and verdicts.
"""
import json, os, random, sys, tomllib, urllib.request
from concurrent.futures import ThreadPoolExecutor
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import corpora

DAEMON = "http://localhost:9741/v1/chat/completions"
NONE = "NOT IN RECORDS"
PROMPT = ("Answer the question from these records only. They are all you know. Answer in one or two sentences "
          "and name the records you used by their number. If the records do not say, answer exactly: "
          f"{NONE}\n\nRecords:\n{{records}}\n\nQuestion: {{question}}")


PRESENTATIONS = range(1, 6)  # fixed seeds: the same records are always shown the same five ways


def presented(records, seed):
    """The same records, their order and each record's statement order shuffled by `seed`."""
    rng = random.Random(seed)
    shown = [dict(r, claims=rng.sample(r.get("claims", []), len(r.get("claims", [])))) for r in records]
    rng.shuffle(shown)
    return shown


def majority(verdicts):
    """A question's verdict over its presentations: what most judged ones gave; fewer than three judged, or a tie,
    is could_not_judge."""
    judged = [v for v in verdicts if v != "could_not_judge"]
    right = judged.count("right")
    if len(judged) < 3 or 2 * right == len(judged):
        return "could_not_judge"
    return "right" if 2 * right > len(judged) else "wrong"


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


def answerer():
    """The model behind the answerer's alias, as the daemon names it (/v1/models: owned_by "alias→<model>")."""
    try:
        ms = json.load(urllib.request.urlopen(DAEMON.replace("chat/completions", "models"), timeout=10))["data"]
        return next((m["owned_by"] for m in ms if m["id"] == "commonwealth/primary"), "not served")
    except Exception as e:
        return f"unknown ({e})"


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
    texts = {k: render(presented(records, k)) for k in PRESENTATIONS}

    def asked(q):
        per = []
        for k in PRESENTATIONS:
            try:
                ans = ask(texts[k], q["question"])
                v = judge(q, ans)
            except Exception as e:  # a failed call judges nothing
                ans, v = f"call failed: {e}", "could_not_judge"
            per.append(dict(seed=k, verdict=v, answer=ans))
            if max(sum(x["verdict"] == w for x in per) for w in ("right", "wrong")) >= 3:
                break  # decided: the presentations left cannot change the majority of five
        return per

    with ThreadPoolExecutor(8) as pool:
        pers = list(pool.map(asked, bank))
    rows = [dict(id=q["id"], qtype=q["qtype"], verdict=majority([x["verdict"] for x in per]), presentations=per)
            for q, per in zip(bank, pers)]
    by = {}
    for r in rows:
        by.setdefault(r["qtype"], {}).setdefault(r["verdict"], 0)
        by[r["qtype"]][r["verdict"]] += 1
    summary = dict(records=len(records), record_chars=len(texts[1]), presentations=len(PRESENTATIONS),
                   calls=sum(len(r["presentations"]) for r in rows),
                   right=sum(r["verdict"] == "right" for r in rows),
                   judged=sum(r["verdict"] != "could_not_judge" for r in rows), by_qtype=by)
    # This file, the model that answered and the bank it judged by: the stamp table.py checks before setting two
    # arms side by side.
    summary["answerer"] = answerer()
    summary["judge"] = corpora.judge_id([open(__file__).read(), summary["answerer"]], [sys.argv[2]])
    json.dump(dict(summary=summary, questions=rows), open(sys.argv[3], "w"), indent=1)
    print(json.dumps(summary, indent=1))
