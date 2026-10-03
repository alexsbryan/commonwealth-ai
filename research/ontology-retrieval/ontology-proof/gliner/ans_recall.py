#!/usr/bin/env python3
"""G-T0 part 2: does GLiNER, given the recipe's declared entity types as labels, find the
mints the LLM extraction leaves without a mint entity? Over ft-ans-dev-b's cached chunks.

    GLINER_SCRATCH=<dir> python3 ans_recall.py [--atoms <atoms.json>] [--json ans_recall.json]

GOLD is the K2 bank's, not re-derived: k2_bank.Fixture / Facts. A mint is ATTESTED for a hoard
when Facts.contains says text T and truth T (the bank's gold `contains` fact); TEXT-ATTESTED
drops the truth half (text T in any truth hoard's sections). The unit is (chunk, mint): a chunk
holding a content or implied mention of a mint attested for the hoard that owns that span.
Per chunk because the seam keeps one mention per (text, label) per chunk (labeled.rs:59).

LLM COVERAGE: a mint is covered when some atoms.json Entity of type `mint` has a canonical
name or alias the bank's [[mints]] match table maps to it (k2_t0 Records.labels' rule).

GLINER: the seam-onnx arm of seam.py (reference pre/post-processing, the seam's ONNX graph
for the logits), max_length 512 to match gline-rs's cap, the seam's 300/30 windows and
per-chunk dedupe, labels = the recipe's `kind = "entity"` type names verbatim. A unit is
FOUND (span) when a `mint` mention overlaps an occurrence of that mint's name in that chunk,
inside the attesting hoard's span; FOUND (any label) relaxes the label. Thresholds 0.6 (the
seam's default, gliner_ner.rs:53) down to 0.3, from one 0.3 run: flat-NER greedy decoding
picks by descending score, so a span under t cannot have displaced one over t.
"""
import argparse, collections, json, pathlib, re, sys, time, tomllib

HERE = pathlib.Path(__file__).resolve().parent
ANS = HERE.parent / "ans"
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ANS))
import seam  # noqa: E402
import k2_bank as kb  # noqa: E402
from make_bank import fold, rx  # noqa: E402

RECIPE = ANS / "recipe-dev-b.toml"
BANK = ANS / "bank-k2-dev.toml"
THRESHOLDS = (0.6, 0.5, 0.4, 0.3)


def declared_labels():
    types = tomllib.load(open(RECIPE, "rb"))["enrichment"]["ontology"]["types"]
    return [t["name"] for t in types if t.get("kind") == "entity"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--atoms", default=str(kb.IDX / "atlas/atoms.json"))
    ap.add_argument("--json", default=str(HERE / "ans_recall.json"))
    a = ap.parse_args()
    t0 = time.time()
    fx = kb.Fixture()
    F = kb.Facts(fx)
    mints = tomllib.load(open(BANK, "rb"))["mints"]
    hoards = tomllib.load(open(BANK, "rb"))["hoards"]

    def to_mints(name):
        n = fold(name or "")
        return {m["id"] for m in mints if any(rx(p).search(n) for p in m["match"])}

    # ── gold units ──
    units = {}                     # (cid, label) -> {gold, roles, occurrences [(s, e)]}
    for k in fx.keys:
        if isinstance(k, str):
            continue
        for lab in fx.vocab:
            c = F.contains(k, lab)
            if c["text"] != "T":
                continue
            gold = c["truth"] == "T"
            for cid, s, e in fx.spans[k]:
                if cid not in c["chunks"]:
                    continue
                text = kb.deaccent(fx.chunk[cid][s:e])
                for b in kb.BIBLIO:
                    text = re.sub(b, lambda m: " " * len(m.group(0)), text)
                occ = [(s + m.start(), s + m.end()) for nm in kb.names_for(lab) for m in kb.crx(nm).finditer(text)]
                u = units.setdefault((cid, lab), {"gold": False, "roles": set(), "occ": set()})
                u["gold"] |= gold
                u["roles"].add(fx.role[k])
                u["occ"].update(occ)
    gold_mints = {lab for (_, lab), u in units.items() if u["gold"]}
    text_mints = {lab for _, lab in units}

    # ── LLM coverage ──
    atoms = json.load(open(a.atoms))["atoms"]
    ents = [x["data"] for x in atoms if x["atom_type"] == "Entity"]
    cov = set()
    for e in ents:
        if e.get("entity_type") == "mint":
            for n in [e.get("canonical_name")] + list(e.get("aliases") or []):
                cov |= to_mints(n)
    no_entity_gold = sorted(gold_mints - cov)
    no_entity_text = sorted(text_mints - cov)

    # ── GLiNER ──
    labels = declared_labels()
    model = seam.load("seam-onnx", max_length=seam.GLINE_MAX_LENGTH)
    cids = sorted(fx.chunk)
    texts = [fx.chunk[c] for c in cids]
    runs = {"declared": seam.infer(model, texts, labels, min(THRESHOLDS), windowed=True, dedupe=True)}
    runs["seam-default-labels"] = seam.infer(model, texts, seam.DEFAULT_LABELS, min(THRESHOLDS), windowed=True, dedupe=True)
    by_chunk = {name: dict(zip(cids, out)) for name, out in runs.items()}

    def found(run, thr, cid, lab, want_label):
        for m in by_chunk[run][cid]:
            if m["score"] < thr or (want_label and m["label"] != want_label):
                continue
            if lab in to_mints(m["text"]) and any(m["start"] < e and s < m["end"] for s, e in units[(cid, lab)]["occ"]):
                return True
        return False

    def recall(run, thr, want_label, keep):
        us = [k for k, u in units.items() if keep(k, u)]
        hit = [k for k in us if found(run, thr, *k, want_label)]
        per_mint = collections.defaultdict(lambda: [0, 0])
        for k in us:
            per_mint[k[1]][1] += 1
            per_mint[k[1]][0] += k in hit
        return {"units": len(us), "found": len(hit), "recall": round(len(hit) / len(us), 3) if us else None,
                "mints": len(per_mint), "mints_found": sum(1 for f, n in per_mint.values() if f),
                "per_mint": {m: f"{f}/{n}" for m, (f, n) in sorted(per_mint.items())}}

    subsets = {
        "gold": lambda k, u: u["gold"],
        "gold_no_llm_mint_entity": lambda k, u: u["gold"] and k[1] in no_entity_gold,
        "text_attested": lambda k, u: True,
        "text_attested_no_llm_mint_entity": lambda k, u: k[1] in no_entity_text,
    }
    res = {"atoms": a.atoms, "atoms_mtime": time.ctime(pathlib.Path(a.atoms).stat().st_mtime),
           "labels": labels, "seam_default_labels": seam.DEFAULT_LABELS,
           "gold_mints": sorted(gold_mints), "text_attested_mints": sorted(text_mints),
           "llm_mint_entities": sorted(e["canonical_name"] for e in ents if e.get("entity_type") == "mint"),
           "no_llm_mint_entity": {"gold": no_entity_gold, "text_attested": no_entity_text},
           "recall": {}}
    for thr in THRESHOLDS:
        for name, keep in subsets.items():
            res["recall"][f"declared mint-label t={thr} {name}"] = recall("declared", thr, "mint", keep)
            res["recall"][f"declared any-label t={thr} {name}"] = recall("declared", thr, None, keep)
            res["recall"][f"seam-default-labels any-label t={thr} {name}"] = recall("seam-default-labels", thr, None, keep)

    # what the mint label also says: of `mint` mentions at the seam threshold, how many name a bank mint
    mm = [m for c in cids for m in by_chunk["declared"][c] if m["label"] == "mint" and m["score"] >= seam.DEFAULT_THRESHOLD]
    off = collections.Counter(m["text"] for m in mm if not to_mints(m["text"]))
    res["mint_mentions_t0.6"] = {"total": len(mm), "name_a_bank_mint": sum(1 for m in mm if to_mints(m["text"])),
                                 "top_not_a_bank_mint": off.most_common(15)}
    res["label_counts_t0.6"] = dict(collections.Counter(m["label"] for c in cids for m in by_chunk["declared"][c]
                                                        if m["score"] >= seam.DEFAULT_THRESHOLD))

    # hoards: a bank hoard is found when a `hoard` mention in one of its chunks matches its match terms
    hr = {}
    for thr in THRESHOLDS:
        for want in ("hoard", None):
            hit = [h["id"] for h in hoards if any(
                (want is None or m["label"] == want) and m["score"] >= thr and any(rx(p).search(fold(m["text"])) for p in h["match"])
                for c in h["chunks"] for m in by_chunk["declared"][c])]
            dev = [h["id"] for h in hoards if h["role"] == "dev"]
            hr[f"t={thr} {want or 'any'}-label"] = {"found": len(hit), "of": len(hoards),
                                                     "dev_found": len([x for x in hit if x in dev]), "dev_of": len(dev),
                                                     "missed": sorted(set(h["id"] for h in hoards) - set(hit))}
    res["hoards"] = hr

    # the seam reports gline-rs BYTE offsets as char offsets (gliner_ner.rs:370-389 vs ner.rs:22-31)
    allm = [(c, m) for c in cids for m in by_chunk["declared"][c] if m["score"] >= seam.DEFAULT_THRESHOLD]
    res["offset_exposure_t0.6"] = {"mentions": len(allm), "byte_offset_differs_from_char": sum(
        1 for c, m in allm if len(fx.chunk[c][:m["start"]].encode()) != m["start"])}
    # which label carried each found unit (gold, seam threshold), per run
    carried = {}
    for run in by_chunk:
        cnt = collections.Counter()
        for (cid, lab), u in units.items():
            if not u["gold"]:
                continue
            for m in by_chunk[run][cid]:
                if m["score"] >= seam.DEFAULT_THRESHOLD and lab in to_mints(m["text"]) and any(
                        m["start"] < e and s < m["end"] for s, e in u["occ"]):
                    cnt[m["label"]] += 1
        carried[run] = dict(cnt)
    res["gold_units_found_by_label_t0.6"] = carried
    res["onnx_vs_torch_max_abs_logit_diff"] = model.model.max_diff
    # every mention at >= 0.3, so any threshold or matcher can be re-scored without the model
    res["mentions"] = {run: {str(c): ms for c, ms in out.items()} for run, out in by_chunk.items()}
    res["seconds"] = round(time.time() - t0)
    json.dump(res, open(a.json, "w"), indent=1, default=sorted)
    for k, v in res["recall"].items():
        if "t=0.6" in k or "t=0.3" in k:
            print(k, {x: v[x] for x in ("units", "found", "recall", "mints", "mints_found")})
    print("no LLM mint entity:", res["no_llm_mint_entity"])
    print("mint mentions t0.6:", res["mint_mentions_t0.6"])
    print("hoards:", {k: (v["found"], v["of"], v["dev_found"], v["dev_of"]) for k, v in hr.items()})
    print("found by label:", carried)
    print("offsets:", res["offset_exposure_t0.6"], "logit diff", res["onnx_vs_torch_max_abs_logit_diff"])


if __name__ == "__main__":
    main()
