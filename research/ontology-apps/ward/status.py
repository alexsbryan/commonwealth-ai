#!/usr/bin/env python3
"""A member's information status: is the composed particular it is about NEW at this passage, or GIVEN (already
under way)? The model observes, ledger.py decides (a new member starts an instance; a given one joins).

    status.py --atoms <stage out>/atoms.json --registry <composed atoms.json> --out <dir> [--folders tune]

For every [[compose]] spec that declares a `status` attribute, each member claim gets new | given, one call per
message with its members as fixed keys in email order, so none can be skipped or invented. Nothing here names a
type: the question is built from the spec's type, the same for a deal, a support case or a message-built
ticket. A research pass, measured apart; adopted, it rides stage.py's call (crm-cost counts calls). Answers are
cached outside git (mailbox text).
"""
import argparse, collections, concurrent.futures as cf, json, pathlib, sys, time, tomllib

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compose as K  # noqa: E402
import deals as D  # noqa: E402
import focus as F  # noqa: E402
import score as S  # noqa: E402

STATUS = ("new", "given")


def question(t):
    return (f"For each passage listed, say whether the {t} it is about is new or given at that passage.\n"
            f"- new: the {t} starts at this passage: it is asked for, offered or raised for the first time, and nothing in "
            f"this email, its quoted mail included, shows it was already under way.\n"
            f"- given: the email treats the {t} as already under way: a follow-up, revision, answer, confirmation or "
            f"reminder about it, or another passage of this email introduces it. When several passages are about one {t} "
            f"that starts in this email, the first in email order is new and the rest are given.")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", default="crm-ward-acts")
    ap.add_argument("--facets", type=pathlib.Path, default=HERE / "compose.toml")
    ap.add_argument("--folders", default="tune", help="tune | read | all (deals.py folds)")
    ap.add_argument("--atoms", type=pathlib.Path, required=True, help="stage.py (or focus.py) output")
    ap.add_argument("--registry", type=pathlib.Path, required=True, help="atoms.json whose company atoms name the parties")
    ap.add_argument("--model", default="commonwealth/primary")
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--cache", type=pathlib.Path, default=S.WARD / "cache/status")
    ap.add_argument("--out", type=pathlib.Path, required=True)
    a = ap.parse_args()
    folders = D.FOLDS["tune"] | D.FOLDS["read"] if a.folders == "all" else D.FOLDS[a.folders]
    facets = tomllib.loads(a.facets.read_text())
    specs = [s for s in facets.get("compose", []) if s.get("status")]
    if not specs:
        raise SystemExit("no [[compose]] spec declares a `status` attribute")
    own = next(s.get("own") for s in facets["source"] if s.get("own"))
    comp, _ = F.registry(a.registry)
    atoms = json.loads(a.atoms.read_text())["atoms"]
    msgs = F.messages(a.corpus, folders)
    report, walls = collections.Counter(), []
    for spec in specs:
        by_msg = collections.defaultdict(list)
        for x in atoms:
            d = x["data"]
            if x["atom_type"] == "Claim" and d.get("claim_kind") in spec["of"] and d.get("message_id") in msgs:
                by_msg[d["message_id"]].append(d)
        for ms in by_msg.values():  # email order: the first sentence each member cites
            ms.sort(key=lambda c: (min(c.get("evidence_sentences") or [10 ** 6]), c.get("unit") or "", c["id"]))
        ask_text = question(spec["type"])

        def one(mid, spec=spec, by_msg=by_msg, ask_text=ask_text):
            msg, ms = msgs[mid], by_msg[mid]
            sents = msg["head"] + F.sentences(msg["body"])
            rows = []
            for i, c in enumerate(ms):
                bv = (c.get("attributes") or {}).get(spec["block"][0])
                name = (comp.get(bv) or {}).get("canonical_name") or bv or f"{spec['block'][0]} not named"
                cites = ", ".join(f"s{j}" for j in c.get("evidence_sentences") or [])
                rows.append(f"p{i}: with {name} ({cites}): \"{c.get('anchor') or ''}\"")
            system = f"You read one email in the mailbox of a person at {own}. Answer only from the email."
            user = (f"{ask_text}\n\nPASSAGES (in email order):\n" + "\n".join(rows) +
                    "\n\nEMAIL (numbered sentences):\n" + "\n".join(f"[s{i}] {s}" for i, s in enumerate(sents)))
            schema = {"type": "object", "required": [f"p{i}" for i in range(len(ms))],
                      "properties": {f"p{i}": {"type": "string", "enum": list(STATUS)} for i in range(len(ms))}}
            t0 = time.time()
            ans, cached = K.ask(a.cache, system, user, schema, model=a.model)
            return mid, ans, cached, time.time() - t0

        with cf.ThreadPoolExecutor(a.workers) as ex:
            for mid, ans, cached, wall in ex.map(one, sorted(by_msg)):
                report["answers replayed from cache" if cached else "answers asked"] += 1
                if not cached:
                    walls.append(wall)
                for i, c in enumerate(by_msg[mid]):
                    v = ans.get(f"p{i}")
                    c.setdefault("attributes", {})[spec["status"]] = v if v in STATUS else None
                    report[f"{spec['type']}: status {v}"] += 1
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "atoms.json").write_text(json.dumps({"schema_version": "prototype", "atoms": atoms}))
    out = {"cost": {"model": a.model, "asked": len(walls), "workers": a.workers,
                    "mean_call_s": round(sum(walls) / len(walls), 2) if walls else None},
           "counts": dict(sorted(report.items()))}
    (a.out / "status_report.json").write_text(json.dumps(out, indent=1) + "\n")
    print(json.dumps(out, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
