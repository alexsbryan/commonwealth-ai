#!/usr/bin/env python3
"""A focused first-order pass for one declared claim type, one call per message, its output TYPED.

    focus.py --type deal_mention --folders tune --registry <composed atoms.json> --out <dir>

The joint Phase-1 prompt asks a section for every facet at once; corpus-engine's relation_focus pass
showed a focused call per declared type reaches what the joint pass lists short (relation_focus.rs).
This is that idea for a claim type, per MESSAGE (a deal is said in a message; one message can say
several), with the output schema generated from the declaration, its [shape] refinements in
compose.toml, and the entity registry, so a wrong value cannot be written rather than being asked
not to be:

  citation      indices of the message's numbered sentences (an enum): the quote is assembled by code
  registry ref  an enum of the registry members this message touches (a name or alias in its text, a
                key in its headers), the owner's family left out; "unlisted" + a name is the escape,
                and code resolves that name and refuses it when it is the owner
  month_range   first and last month, each from the closed list of months the archive can mean
  values        a closed set

Output replaces the base atlas's claims of the type in <out>/atoms.json (compose.py, score.py and
deals.py read it unchanged); <out>/focus_report.json carries cost and every refusal. Answers are cached
per message, keyed by the whole request, outside git (mailbox text).
"""
import argparse, collections, concurrent.futures as cf, email.utils, hashlib, json, pathlib, re, sys, time, tomllib

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import compose as K  # noqa: E402
import deals as D  # noqa: E402
import score as S  # noqa: E402

HOME = pathlib.Path.home()
UNLISTED, NONE, UNKNOWN = "unlisted", "none", "unknown"


def declaration(recipe, name):
    types = tomllib.loads(recipe.read_text())["enrichment"]["ontology"]["types"]
    return next(t for t in types if t["name"] == name)


def registry(path):
    """company id -> record, plus a folded-alias index; the owner's family is marked, never offered."""
    atoms = json.loads(path.read_text())["atoms"]
    comp = {x["data"]["id"]: x["data"] for x in atoms if x["atom_type"] == "Entity" and x["data"].get("entity_type") == "company"}
    alias = collections.defaultdict(set)
    for i, e in comp.items():
        for n in [e.get("canonical_name")] + list(e.get("aliases") or []):
            if n and len(K.fold(n)) >= 3:
                alias[K.fold(n)].add(i)
    return comp, alias


def months_between(lo, hi):
    out, (y, m) = [], lo
    while (y, m) <= hi:
        out.append(f"{y:04d}-{m:02d}"); y, m = (y + (m == 12), m % 12 + 1)
    return out


def sentences(text):
    out = []
    for line in text.splitlines():
        for s in re.split(r"(?<=[.!?])\s+", line.strip()):
            if s.strip():
                out.append(s.strip())
    return out[:150]


class Shape:
    """The per-message output type: declaration attributes refined by [shape], bound to this message."""

    def __init__(self, decl, shape, months):
        self.decl, self.shape, self.months = decl, shape, months

    def schema(self, n_sent, labels):
        props = {"evidence": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": [f"s{i}" for i in range(n_sent)]}}}
        for a in self.decl.get("attributes", []):
            sh, name = self.shape.get(a["name"], {}), a["name"]
            if sh.get("kind") == "registry":
                props[name] = {"type": "string", "enum": labels + [NONE, UNLISTED]}
                props[f"{name}_unlisted"] = {"type": "string"}
            elif sh.get("kind") == "month_range":
                props[f"{name}_start"] = props[f"{name}_end"] = {"type": "string", "enum": self.months + [UNKNOWN]}
            elif sh.get("kind") == "values" or a.get("values"):
                vals = list(sh.get("values") or a["values"])
                props[name] = {"type": "string", "enum": vals + ([UNKNOWN] if UNKNOWN not in vals and NONE not in vals else [])}
            else:
                props[name] = {"type": "string"}
        item = {"type": "object", "required": list(props), "properties": props}
        return {"type": "object", "required": ["items"], "properties": {"items": {"type": "array", "items": item}}}

    def guide(self):
        lines = ["- evidence: the numbers of the sentences that show it"]
        for a in self.decl.get("attributes", []):
            sh, d = self.shape.get(a["name"], {}), a.get("description") or a["name"]
            if sh.get("kind") == "registry":
                lines.append(f"- {a['name']}: {d}. Pick it from the list; when it is not listed pick {UNLISTED} and "
                             f"write its name in {a['name']}_unlisted; pick {NONE} when the email does not say")
            elif sh.get("kind") == "month_range":
                lines.append(f"- {a['name']}_start, {a['name']}_end: the first and last month of {d}, read against the "
                             f"email's date; {UNKNOWN} when it does not say")
            else:
                lines.append(f"- {a['name']}: {d}")
        return "\n".join(lines)


def messages(corpus, folders):
    """message id -> {section, path, meta, head, body}: headers from chunk metadata, body = filtered chunks."""
    import lance  # noqa: PLC0415
    idx = HOME / ".svrnmesh/indexes" / corpus
    rows = lance.dataset(str(idx / "chunks.lance")).to_table(columns=["id", "metadata", "content"]).to_pylist()
    manifest = json.loads((S.WARD / "manifest.json").read_text())
    path_of = {m: e["path"] for e in manifest for m in e["message_ids"]}
    sec_of = {str(i): ch["id"] for ch in json.loads((idx / "chapters.json").read_text())["chapters"] for i in ch["chunk_ids"]}
    out = {}
    for r in rows:
        m = json.loads(r["metadata"] or "{}"); mid = m.get("message_id")
        path = path_of.get(mid)
        if not path or path.split("/", 1)[0] not in folders or str(r["id"]) not in sec_of:
            continue
        d = out.setdefault(mid, {"section": sec_of[str(r["id"])], "path": path, "meta": m, "body": []})
        d["body"].append(r["content"] or "")
    for d in out.values():
        m = d["meta"]
        d["head"] = [f"{k.capitalize()}: {K.meta_value(m.get(k)) or ''}" for k in ("from", "to", "cc", "date", "subject")]
        d["body"] = "\n".join(d["body"])[:12000]
    return out


def touched(msg, comp, alias, owners):
    """Registry members this message touches: a key in its headers or a name/alias in its words."""
    doms = {a.split("@", 1)[1] for k in ("from", "to", "cc") for a in K.addresses(K.meta_value(msg["meta"].get(k)))}
    words = f" {K.fold(' '.join(msg['head']) + ' ' + msg['body'])} "
    hit = {i for i, e in comp.items() if (e.get("attributes") or {}).get("domain") in doms}
    hit |= {i for n, ids in alias.items() if f" {n} " in words for i in ids}
    return sorted(hit - owners)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", default="crm-ward-acts")
    ap.add_argument("--recipe", type=pathlib.Path, default=HERE / "recipe-acts.toml")
    ap.add_argument("--facets", type=pathlib.Path, default=HERE / "compose.toml")
    ap.add_argument("--type", default="deal_mention")
    ap.add_argument("--folders", default="tune", help="tune | read | all (deals.py folds)")
    ap.add_argument("--registry", type=pathlib.Path, required=True, help="atoms.json whose company atoms are resolved (compose.py output)")
    ap.add_argument("--model", default="commonwealth/primary")
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--atoms", type=pathlib.Path, help="base atoms (default: the corpus atlas)")
    ap.add_argument("--cache", type=pathlib.Path, default=S.WARD / "cache/focus")
    ap.add_argument("--out", type=pathlib.Path, required=True)
    a = ap.parse_args()
    folders = D.FOLDS["tune"] | D.FOLDS["read"] if a.folders == "all" else D.FOLDS[a.folders]
    facets = tomllib.loads(a.facets.read_text())
    decl = declaration(a.recipe, a.type)
    own = next(s.get("own") for s in facets["source"] if s.get("own"))
    comp, alias = registry(a.registry)
    owners = {i for i, e in comp.items() if e.get("own")}
    msgs = messages(a.corpus, folders)
    dates = [email.utils.parsedate_to_datetime(K.meta_value(m["meta"].get("date"))) for m in msgs.values() if K.meta_value(m["meta"].get("date"))]
    shape = Shape(decl, facets.get("shape", {}).get(a.type, {}),
                  months_between((min(dates).year - 1, 1), (max(dates).year + 3, 12)))
    report = collections.Counter(); walls = []

    def one(mid):
        msg = msgs[mid]
        cand = touched(msg, comp, alias, owners)
        labels, label_id = [], {}
        for i in cand:
            lab = comp[i].get("canonical_name") or i
            lab = lab if lab not in label_id else f"{lab} ({(comp[i].get('attributes') or {}).get('domain', i)})"
            labels.append(lab); label_id[lab] = i
        sents = msg["head"] + sentences(msg["body"])
        numbered = "\n".join(f"[s{i}] {s}" for i, s in enumerate(sents))
        system = f"You extract records from one email in the mailbox of a person at {own}. Answer only from the email."
        user = (f"List every {decl['name']} in this email: {decl.get('description', '')}\n"
                f"An email can hold several, for different counterparties or different deals, or none (an empty list).\n"
                f"For each give:\n{shape.guide()}\n\nORGANIZATIONS IN THIS EMAIL: {', '.join(labels) or '(none listed)'}\n\n"
                f"EMAIL (numbered sentences):\n{numbered}")
        t0 = time.time()
        ans, cached = K.ask(a.cache, system, user, shape.schema(len(sents), labels), model=a.model)
        return mid, ans, cached, time.time() - t0, sents, label_id

    def ref(i):
        """A registry member as the next identity pass reads it: its key when it has one, else its name. A
        name-derived id is minted from the canonical the model chose in the registry's run, so it is no
        key once it leaves that run (51 of 131 tune mentions became junk companies, 2026-10-04)."""
        e = comp[i]
        return i if (e.get("provenance") or {}).get("signal_kind") == "metadata_projection" else e.get("canonical_name") or i

    def resolve(item, name, label_id, rep, words):
        v = item.get(name)
        if v in label_id:
            return ref(label_id[v])
        if v == UNLISTED:
            raw = (item.get(f"{name}_unlisted") or "").strip()
            ids = alias.get(K.fold(raw), set())
            if ids & owners:
                rep[f"{name}: unlisted name is the owner's family, refused"] += 1
                return None
            if len(ids) == 1:
                rep[f"{name}: unlisted name resolved through the registry"] += 1
                return ref(next(iter(ids)))
            if not raw or f" {K.fold(raw)} " not in words:  # a party the message never names cannot be its party
                rep[f"{name}: unlisted name not said in the message, refused"] += 1
                return None
            rep[f"{name}: unlisted name kept raw"] += 1
            return raw
        return None
    claims, t_start = [], time.time()
    with cf.ThreadPoolExecutor(a.workers) as ex:
        for mid, ans, cached, wall, sents, label_id in ex.map(one, sorted(msgs)):
            words = f" {K.fold(' '.join(sents))} "
            report["answers replayed from cache" if cached else "answers asked"] += 1
            if not cached:
                walls.append(wall)
            for it in ans.get("items", []):
                ev = sorted({int(s[1:]) for s in it.get("evidence", []) if s[1:].isdigit() and int(s[1:]) < len(sents)})
                quote = " ".join(sents[i] for i in ev)
                attrs = {}
                for at in decl.get("attributes", []):
                    nm, sh = at["name"], shape.shape.get(at["name"], {})
                    if sh.get("kind") == "registry":
                        attrs[nm] = resolve(it, nm, label_id, report, words)
                    elif sh.get("kind") == "month_range":
                        for side in ("start", "end"):
                            v = it.get(f"{nm}_{side}")
                            attrs[f"{nm}_{side}"] = None if v in (UNKNOWN, None) else v
                    else:
                        v = it.get(nm)
                        attrs[nm] = None if v in ("", None, UNKNOWN, NONE) else v
                if attrs.get("via") and attrs.get("via") == attrs.get("counterparty"):
                    attrs["via"] = None; report["via equal to counterparty, dropped"] += 1
                if attrs.get("period_start") and attrs.get("period_end") and attrs["period_end"] < attrs["period_start"]:
                    attrs["period_start"], attrs["period_end"] = attrs["period_end"], attrs["period_start"]
                    report["period bounds reversed, swapped"] += 1
                cid = f"focus:{hashlib.sha1('|'.join([mid, quote, json.dumps(attrs, sort_keys=True)]).encode()).hexdigest()[:12]}"
                claims.append({"atom_type": "Claim", "data": {
                    "id": cid, "claim_kind": a.type, "attributes": attrs, "anchor": quote, "evidence_sentences": ev,
                    "evidence": [{"chunk_id": msgs[mid]["section"]}], "message_id": mid,
                    "provenance": {"signal_kind": "focused_pass", "model": a.model}}})
                report["items"] += 1
                report[f"items with a counterparty"] += bool(attrs.get("counterparty"))
    elapsed = time.time() - t_start
    base = json.loads((a.atoms or HOME / ".svrnmesh/indexes" / a.corpus / "atlas/atoms.json").read_text())["atoms"]
    kept = [x for x in base if not (x["atom_type"] == "Claim" and x["data"].get("claim_kind") == a.type)]
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "atoms.json").write_text(json.dumps({"schema_version": "prototype", "atoms": kept + claims}))
    cost = {"model": a.model, "messages": len(msgs), "asked": len(walls), "workers": a.workers,
            "elapsed_s": round(elapsed, 1), "s_per_message_wall": round(elapsed / len(msgs), 2) if msgs else None,
            "mean_call_s": round(sum(walls) / len(walls), 2) if walls else None}
    (a.out / "focus_report.json").write_text(json.dumps({"cost": cost, "counts": dict(report)}, indent=1) + "\n")
    print(json.dumps({"cost": cost, "counts": dict(report)}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
