"""Prompt A/B for phase-1 relation recall on ft-ans-dev-b, no state touched.

The prompt is exactly what `svrn enrich extract --dry-run` composes (system, user, response schema);
each arm applies one text transform and calls the daemon's primary with the extractor's parameters
(temperature 0.1, thinking off, the response schema as json_schema). Scored per run: holds_coins_of
relations emitted, distinct ATTESTED mints reached as the `to` end, and ends that are not a mint.

    python3 relation_ab.py --sections sec_00008,sec_00004,sec_00009 --arms A,B --runs 3
"""
import argparse, collections, json, pathlib, re, subprocess, sys, time, urllib.request

ANS = pathlib.Path("/Users/alexsbryan/dev/commonwealth-ai/research/ontology-retrieval/ontology-proof/ans")
sys.path.insert(0, str(ANS))
import k2_bank  # noqa: E402
from k2_t0 import IDX  # noqa: E402
from make_bank import fold, rx  # noqa: E402

OUT = pathlib.Path("/private/tmp") / "relation_ab"  # raw extractions quote the private text: never in git
CHAT = "http://127.0.0.1:9741/v1/chat/completions"
REL = "holds_coins_of"
RENAMED = "contains_coins_struck_at"

# Arm B: the shape example's single relation becomes the listing it would be in a list-dense text.
EXAMPLE_ONE = '''  "relations_introduced": [
    {
      "participants": ["the broad penny", "Offa of Mercia"],
      "label": "minted under the authority of",
      "anchor": "struck in the king's name"
    }
  ],'''
EXAMPLE_LIST = '''  "relations_introduced": [
    {
      "participants": ["the broad penny", "Offa of Mercia"],
      "label": "minted under the authority of",
      "anchor": "struck in the king's name"
    },
    {
      "participants": ["the broad penny", "Canterbury"],
      "label": "struck at",
      "anchor": "pennies of Canterbury, London and Ipswich"
    },
    {
      "participants": ["the broad penny", "London"],
      "label": "struck at",
      "anchor": "pennies of Canterbury, London and Ipswich"
    },
    {
      "participants": ["the broad penny", "Ipswich"],
      "label": "struck at",
      "anchor": "pennies of Canterbury, London and Ipswich"
    }
  ],'''


def dry_run(section):
    t = subprocess.run(["sovereign", "enrich", "extract", "ft-ans-dev-b", "--chapters", section, "--dry-run"],
                       capture_output=True, text=True, check=True).stdout
    system = t.split("· system ────", 1)[1].split("──── ", 1)[0].strip()
    user = t.split("· user ────", 1)[1].split("──── ", 1)[0].strip()
    schema, _ = json.JSONDecoder().raw_decode(t.split("· response schema ────", 1)[1].strip())
    return system, user, schema


def arm(name, system, user, schema):
    if name == "A":
        return system, user, schema
    if name == "B":
        assert EXAMPLE_ONE in system, "shape example changed; arm B no longer applies"
        return system.replace(EXAMPLE_ONE, EXAMPLE_LIST), user, schema
    if name == "C":
        s = json.loads(json.dumps(schema).replace(REL, RENAMED))
        return system.replace(REL, RENAMED), user, s
    raise ValueError(name)


def call(system, user, schema):
    body = {"model": "primary", "temperature": 0.1, "max_tokens": 16384, "think_budget": 0,
            "chat_template_kwargs": {"enable_thinking": False},
            "response_format": {"type": "json_schema", "json_schema": {"name": "section", "schema": schema, "strict": True}},
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}]}
    for attempt in range(4):
        try:
            req = urllib.request.Request(CHAT, data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
            d = json.load(urllib.request.urlopen(req, timeout=900))
            return d["choices"][0]["message"].get("content") or ""
        except Exception as e:  # noqa: BLE001 — a research harness; the error is printed and retried
            print(f"  retry {attempt + 1}: {e}", file=sys.stderr)
            time.sleep(5 * (attempt + 1))
    return ""


def attested_mints():
    fx = k2_bank.Fixture(); F = k2_bank.Facts(fx)
    c2s = {cid: c["id"] for c in json.loads((IDX / "ft-ans-dev-b" / "chapters.json").read_text())["chapters"]
           for cid in c["chunk_ids"]}
    att = collections.defaultdict(set)
    for k in fx.keys:
        for m in fx.vocab:
            a = F.contains(k, m)
            if a["text"] == "T":
                for ch in a["chunks"]:
                    if ch in c2s:
                        att[c2s[ch]].add(m)
    bank = __import__("tomllib").loads((ANS / "bank-k2-dev.toml").read_text())
    pats = {m["id"]: m["match"] for m in bank["mints"]}
    return att, pats


def score(raw, attested, pats):
    try:
        d = json.loads(raw)
    except json.JSONDecodeError:
        return {"parse": False}
    rels = [r for r in d.get("relations_introduced") or [] if r.get("relation_type") in (REL, RENAMED)]
    label = lambda name: {m for m, ps in pats.items() if any(rx(p).search(fold(name or "")) for p in ps)}  # noqa: E731
    to_ends = [label((r.get("participants") or [None, None])[1] if len(r.get("participants") or []) > 1 else "") for r in rels]
    reached = set().union(*to_ends) if to_ends else set()
    return {"parse": True, "emitted": len(rels), "attested_reached": len(reached & attested),
            "attested": len(attested), "non_mint_ends": sum(1 for e in to_ends if not e)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sections", required=True)
    ap.add_argument("--arms", default="A,B")
    ap.add_argument("--runs", type=int, default=3)
    a = ap.parse_args()
    OUT.mkdir(exist_ok=True)
    att, pats = attested_mints()
    rows = []
    for sec in a.sections.split(","):
        base = dry_run(sec)
        for name in a.arms.split(","):
            system, user, schema = arm(name, *base)
            for run in range(a.runs):
                t0 = time.time()
                raw = call(system, user, schema)
                (OUT / f"{sec}.{name}.{run}.json").write_text(raw)
                s = score(raw, att[sec], pats)
                rows.append({"section": sec, "arm": name, "run": run, "seconds": round(time.time() - t0, 1), **s})
                print(json.dumps(rows[-1]), flush=True)
    (OUT / "rows.json").write_text(json.dumps(rows, indent=1))
    print("\nsection   arm  emitted(mean)  attested_reached(mean)/attested  non_mint_ends(mean)")
    for (sec, name), rs in sorted(collections.defaultdict(list, {}).items()):
        pass
    groups = collections.defaultdict(list)
    for r in rows:
        if r.get("parse"):
            groups[(r["section"], r["arm"])].append(r)
    for (sec, name), rs in sorted(groups.items()):
        m = lambda k: round(sum(r[k] for r in rs) / len(rs), 2)  # noqa: E731
        print(f"{sec}  {name}    {m('emitted'):5}  {m('attested_reached'):5}/{rs[0]['attested']}   {m('non_mint_ends'):5}   runs={len(rs)}")


if __name__ == "__main__":
    main()
