#!/usr/bin/env python3
"""A mention's stage as a fold over the events its message shows — the model observes, code decides.

    stage.py --atoms <focus out>/atoms.json --registry <composed atoms.json> --out <dir> [--folders tune]

Asked for one lifecycle state out of six overlapping ones, the first-order pass reads "negotiating" for
most live deals (tune: 25 of 36 stage disagreements with gold). Here the model answers, per mention, a
yes/no for each event declared in compose.toml [events.<type>]; the stage is the furthest event in the
declared order. One call per message, its mentions as fixed keys, so none can be skipped or invented.
Mentions keep their id and every other attribute; the events land on the claim beside the stage, the
first-order stage under stage_first_order. Answers are cached outside git (mailbox text).
"""
import argparse, collections, concurrent.futures as cf, json, pathlib, sys, time, tomllib

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compose as K  # noqa: E402
import deals as D  # noqa: E402
import focus as F  # noqa: E402
import score as S  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", default="crm-ward-acts")
    ap.add_argument("--facets", type=pathlib.Path, default=HERE / "compose.toml")
    ap.add_argument("--type", default="deal_mention")
    ap.add_argument("--folders", default="tune", help="tune | read | all (deals.py folds)")
    ap.add_argument("--atoms", type=pathlib.Path, required=True, help="focus.py output")
    ap.add_argument("--registry", type=pathlib.Path, required=True, help="atoms.json whose company atoms name the counterparties")
    ap.add_argument("--model", default="commonwealth/primary")
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--cache", type=pathlib.Path, default=S.WARD / "cache/stage")
    ap.add_argument("--out", type=pathlib.Path, required=True)
    a = ap.parse_args()
    folders = D.FOLDS["tune"] | D.FOLDS["read"] if a.folders == "all" else D.FOLDS[a.folders]
    facets = tomllib.loads(a.facets.read_text())
    ev = facets["events"][a.type]
    order, to_stage, say = ev["order"], ev["stage"], ev["say"]
    own = next(s.get("own") for s in facets["source"] if s.get("own"))
    comp, _ = F.registry(a.registry)
    atoms = json.loads(a.atoms.read_text())["atoms"]
    msgs = F.messages(a.corpus, folders)
    by_msg = collections.defaultdict(list)
    for x in atoms:
        d = x["data"]
        if x["atom_type"] == "Claim" and d.get("claim_kind") == a.type and d.get("message_id") in msgs:
            by_msg[d["message_id"]].append(d)
    report = collections.Counter(); walls = []
    questions = "\n".join(f"- {e}: {say[e]}" for e in order)

    def one(mid):
        msg, ms = msgs[mid], by_msg[mid]
        sents = msg["head"] + F.sentences(msg["body"])
        rows = []
        for i, c in enumerate(ms):
            cp = (c.get("attributes") or {}).get("counterparty")
            name = (comp.get(cp) or {}).get("canonical_name") or cp or "counterparty not named"
            cites = ", ".join(f"s{j}" for j in c.get("evidence_sentences") or [])
            rows.append(f"d{i}: with {name} ({cites}): \"{c.get('anchor') or ''}\"")
        system = f"You read one email in the mailbox of a person at {own} and report what it shows happening in each deal listed. Answer only from the email."
        user = (f"For each deal, answer each question true only when this email says it happened for that deal:\n{questions}\n\n"
                f"DEALS:\n" + "\n".join(rows) + "\n\nEMAIL (numbered sentences):\n" + "\n".join(f"[s{i}] {s}" for i, s in enumerate(sents)))
        item = {"type": "object", "required": order, "properties": {e: {"type": "boolean"} for e in order}}
        schema = {"type": "object", "required": [f"d{i}" for i in range(len(ms))],
                  "properties": {f"d{i}": item for i in range(len(ms))}}
        t0 = time.time()
        ans, cached = K.ask(a.cache, system, user, schema, model=a.model)
        return mid, ans, cached, time.time() - t0

    t_start = time.time()
    with cf.ThreadPoolExecutor(a.workers) as ex:
        for mid, ans, cached, wall in ex.map(one, sorted(by_msg)):
            report["answers replayed from cache" if cached else "answers asked"] += 1
            if not cached:
                walls.append(wall)
            for i, c in enumerate(by_msg[mid]):
                got = [e for e in order if (ans.get(f"d{i}") or {}).get(e) is True]
                at = c.setdefault("attributes", {})
                c["stage_first_order"] = at.get(ev["attribute"])
                c["stage_events"] = got
                at[ev["attribute"]] = to_stage[got[-1]] if got else None
                report[f"mentions with {len(got)} events"] += 1
                report[f"stage {at[ev['attribute']]} (first order {c['stage_first_order']})"] += 1
    elapsed = time.time() - t_start
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "atoms.json").write_text(json.dumps({"schema_version": "prototype", "atoms": atoms}))
    cost = {"model": a.model, "messages": len(by_msg), "asked": len(walls), "workers": a.workers, "elapsed_s": round(elapsed, 1),
            "mean_call_s": round(sum(walls) / len(walls), 2) if walls else None}
    out = {"cost": cost, "counts": dict(sorted(report.items()))}
    (a.out / "stage_report.json").write_text(json.dumps(out, indent=1) + "\n")
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
