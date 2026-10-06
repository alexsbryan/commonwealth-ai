#!/usr/bin/env python3
"""GVC and ECB+, the public cross-document event coreference corpora, as eval data for the ontology layer.

    prepare.py [--corpus gvc ecbplus] [--out ~/.svrnmesh/bench-corpora] [--src DIR] [--no-crosscheck]

Both are parsed from the original releases at pinned commits, stdlib only: GVC's verbose.conll
(Vossen et al. 2018) and ECB+'s XML (Cybulska & Vossen 2014). uCDCR (Zhukova et al., LREC 2026)
packages both with the standard splits, but its full text ships only as parquet, folds GVC's DCT and
title into the token stream, and its ECB+ token table labels 359 of 982 documents with two splits.
Its mention JSON is stdlib-readable and is used instead as an independent cross-check, reported per
split in gold/README.md: documents, mentions, and the chain partition.

Splits: ECB+ by topic, Cybulska & Vossen 2015; only mentions in its validated sentences
(ECBplus_coreference_sentences.csv) are `scored`, as in that setup. GVC by incident, Bugert et al.
2021, as distributed in UKPLab/cdcr-beyond-corpus-tailored (the files uCDCR reuses).

Writes, per corpus, outside git under <out>/<gvc|ecbplus>/:
  src/                 the release files (fetched when absent; --src reads a copy kept elsewhere)
  raw/documents.jsonl  id, title, created_at, body, split (+ topic, subtopic for ECB+); no gold
  gold/mentions.jsonl  one gold mention per line, character offsets into that document's body
  gold/README.md       sources, licences, split provenance, counts, the offset and cross-check verdicts

Body: tokens joined by one space, one sentence per line, so line i is the mention's `sentence` i.
GVC's title sentences come first, so title mentions get offsets too. Every mention is checked: the
body at its offsets, whitespace-folded, equals the annotation's own tokens joined by spaces, and
its start lies on line `sentence`. A discontinuous mention carries `segments`; the check joins those.
"""
import argparse, collections, csv, io, json, pathlib, sys, urllib.request, xml.etree.ElementTree as ET, zipfile

GVC_GIT = "https://raw.githubusercontent.com/cltl/GunViolenceCorpus/1d80c3a21bb896520dae9e047d70d598279c0df6"
UKP_GIT = ("https://raw.githubusercontent.com/UKPLab/cdcr-beyond-corpus-tailored/"
           "1f60cafbcf5eeb723c46ed68a1b3ee4fc0fcdc26/resources/data/gun_violence")
ECB_GIT = "https://raw.githubusercontent.com/cltl/ecbPlus/3aa071e12fd08fc0705b16bb1841e7a9c5c22420/ECB+_LREC2014"
UCDCR = "https://huggingface.co/datasets/AnZhu/uCDCR/resolve/6c13e7786272701159f7ccd638352b72c9990e01"
SOURCES = {
    "gvc": {"verbose.conll": f"{GVC_GIT}/verbose.conll", "LICENSE.md": f"{GVC_GIT}/LICENSE.md",
            **{f: f"{UKP_GIT}/{f}" for f in ("train.csv", "dev.csv", "test.csv", "gvc_doc_to_event.csv")}},
    "ecbplus": {f: f"{ECB_GIT}/{f}" for f in ("ECB+.zip", "ECBplus_coreference_sentences.csv", "LICENSEDATA.TXT")},
}
UCDCR_DIR = {"gvc": "GVC", "ecbplus": "ECBplus"}
SPLITS = ("train", "dev", "test")
KINDS = ("event", "entity")


def topics(spec):  # "1,3,6-8" -> {1, 3, 6, 7, 8}
    return {n for part in spec.split(",") for a, _, b in [part.partition("-")] for n in range(int(a), int(b or a) + 1)}


# Cybulska & Vossen 2015. Topics 15 and 17 are in the published list but absent from the release.
ECB_SPLIT = {s: topics(t) for s, t in (("train", "1,3,4,6-11,13-17,19,20,22,24-33"),
                                       ("dev", "2,5,12,18,21,23,34,35"), ("test", "36-45"))}


def fetch(src, name, url):
    p = src / name
    if not p.exists():
        p.parent.mkdir(parents=True, exist_ok=True)
        with urllib.request.urlopen(url, timeout=300) as r:  # stdlib default User-Agent, nothing identifying
            p.with_name(p.name + ".part").write_bytes(r.read())
        p.with_name(p.name + ".part").rename(p)
    return p


def render(sentences):
    """Body and per-token (start, end): tokens joined by one space, one sentence per line."""
    lines, offsets, pos = [], [], 0
    for toks in sentences:
        spans = []
        for t in toks:
            spans.append((pos, pos + len(t)))
            pos += len(t) + 1
        pos += not toks
        offsets.append(spans)
        lines.append(" ".join(toks))
    return "\n".join(lines), offsets


def clean(tok):  # a token may not carry a line break into the body, or the line == sentence rule breaks
    return " ".join(tok.split())


class Doc:
    def __init__(self, doc_id, sentences):
        self.id, self.sentences = doc_id, sentences
        self.body, self.offsets = render(sentences)
        self.base = [0]
        for toks in sentences:
            self.base.append(self.base[-1] + len(toks))

    def mention(self, mention_id, kind, typ, toks, chain_id, annotated, **extra):
        """toks: sorted (sentence, index-in-sentence); annotated: the annotation's own token strings."""
        glob = [self.base[s] + k for s, k in toks]
        runs = []
        for (s, k), g in zip(toks, glob):
            if runs and g == runs[-1][1] + 1:
                runs[-1][1], runs[-1][3] = g, self.offsets[s][k][1]
            else:
                runs.append([g, g, self.offsets[s][k][0], self.offsets[s][k][1]])
        segments = [(a, b) for _, _, a, b in runs]
        s0, k0 = toks[0]
        m = {"doc_id": self.id, "mention_id": mention_id, "kind": kind, "type": typ, "sentence": s0,
             "token_span": [k0, k0 + glob[-1] - glob[0] + 1], "doc_token_span": [glob[0], glob[-1] + 1],
             "start": segments[0][0], "end": segments[-1][1],
             "text": " ".join(self.body[a:b] for a, b in segments), "chain_id": chain_id, **extra}
        if len(segments) > 1:
            m["segments"], m["doc_tokens"] = [list(x) for x in segments], glob
        m["_ok"] = (" ".join(m["text"].split()) == " ".join(" ".join(annotated).split())
                    and self.body.count("\n", 0, m["start"]) == s0)
        return m


def conll_docs(path):
    doc, rows = None, []
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#begin document"):
            doc, rows = line.split("(", 1)[1].split(")", 1)[0], []
        elif line.startswith("#end document"):
            yield doc, rows
        elif line.strip():
            rows.append(line.split("\t"))


def parse_gvc(src, problems):
    split_of_incident = {}
    for s in SPLITS:
        for row in csv.reader(fetch(src, f"{s}.csv", SOURCES["gvc"][f"{s}.csv"]).open()):
            split_of_incident[row[1]] = s
    rows = csv.DictReader(fetch(src, "gvc_doc_to_event.csv", SOURCES["gvc"]["gvc_doc_to_event.csv"]).open())
    incident_of_doc = {r["doc-id"]: r["event-id"] for r in rows}
    docs, mentions = [], []
    for doc_id, rows in conll_docs(fetch(src, "verbose.conll", SOURCES["gvc"]["verbose.conll"])):
        dct = [r[1] for r in rows if r[2] == "DCT"]
        toks = [r for r in rows if r[2] != "DCT"]  # id, token, TITLE|BODY, verbose, chain
        keys = sorted({r[0].split(".")[1] for r in toks}, key=lambda k: (k[0] != "t", int(k[1:])))
        sidx = {k: i for i, k in enumerate(keys)}
        sentences, where = [[] for _ in keys], []
        for r in toks:
            s = sidx[r[0].split(".")[1]]
            where.append((s, len(sentences[s])))
            sentences[s].append(clean(r[1]))
        d = Doc(doc_id, sentences)
        open_, spans = collections.defaultdict(list), []
        for i, r in enumerate(toks):
            for part in ([] if r[4] == "-" else r[4].split("|")):
                cid = part.strip("()")
                if part.startswith("(") and part.endswith(")"):
                    spans.append((i, i, cid))
                elif part.startswith("("):
                    open_[cid].append(i)
                elif part.endswith(")"):
                    if open_[cid]:
                        spans.append((open_[cid].pop(), i, cid))
                    else:
                        problems["gvc: close bracket with no open"] += 1
                elif not open_[cid]:
                    problems["gvc: continuation token outside its mention"] += 1
        problems["gvc: unclosed mention"] += sum(len(v) for v in open_.values())
        opens = sum(p.startswith("(") for r in toks if r[4] != "-" for p in r[4].split("|"))
        problems["gvc: opening bracket not turned into a mention"] += opens - len(spans)
        incident = incident_of_doc.get(doc_id)
        for a, b, cid in sorted(spans):
            tw = sorted(where[a:b + 1])
            s, k = tw[0]
            mid = f"{doc_id}/s{s}t{k}-{k + b - a + 1}"
            verbose = toks[a][3].split(".")
            if cid != "0" and verbose[0] != incident:
                problems["gvc: mention incident differs from the document's"] += 1
            m = d.mention(mid, "event", None if cid == "0" else verbose[1], tw,
                          f"singleton:{mid}" if cid == "0" else cid, [r[1] for r in toks[a:b + 1]],
                          incident=None if cid == "0" else verbose[0])
            if cid == "0":
                m["unlinked"] = True  # GVC chain 0: not linked to the incident; a singleton, as uCDCR does
            mentions.append(m)
        ntitle = sum(k[0] == "t" for k in keys)
        docs.append({"id": doc_id, "title": " ".join(" ".join(x) for x in sentences[:ntitle]) or None,
                     "created_at": dct[0] if dct else None, "body": d.body,
                     "split": split_of_incident.get(incident)})
    return docs, mentions


def parse_ecbplus(src, problems):
    rows = csv.DictReader(fetch(src, "ECBplus_coreference_sentences.csv",
                                SOURCES["ecbplus"]["ECBplus_coreference_sentences.csv"]).open())
    validated = {(f"{r['Topic']}_{r['File']}", int(r["Sentence Number"])) for r in rows}
    split_of_topic = {t: s for s, ts in ECB_SPLIT.items() for t in ts}
    z = zipfile.ZipFile(fetch(src, "ECB+.zip", SOURCES["ecbplus"]["ECB+.zip"]))
    docs, mentions = [], []
    for name in sorted(n for n in z.namelist() if n.endswith(".xml") and not n.startswith("__MACOSX")):
        root = ET.fromstring(z.read(name))
        doc_id = pathlib.PurePosixPath(name).stem
        topic, num = doc_id.split("_")
        sub = "ecbplus" if num.endswith("ecbplus") else "ecb"
        nsent = 1 + max(int(t.get("sentence")) for t in root.iter("token"))
        sentences, where, raw = [[] for _ in range(nsent)], {}, {}
        for t in root.iter("token"):
            s = int(t.get("sentence"))
            if int(t.get("number")) != len(sentences[s]):
                problems["ecbplus: token number differs from its position"] += 1
            where[t.get("t_id")], raw[t.get("t_id")] = (s, len(sentences[s])), t.text or ""
            sentences[s].append(clean(t.text or ""))
        problems["ecbplus: sentence numbers skipped (empty body line)"] += sum(not x for x in sentences)
        d = Doc(doc_id, sentences)
        marks = list(root.find("Markables"))
        descriptor = {m.get("m_id"): m for m in marks if m.find("token_anchor") is None}
        chain, label = {}, {}
        for rel in root.find("Relations") if root.find("Relations") is not None else []:
            target = descriptor.get(rel.find("target").get("m_id"))
            if rel.tag == "CROSS_DOC_COREF":
                cid = rel.get("note") or target.get("instance_id")
                if target is not None and target.get("instance_id") not in (None, cid):
                    problems["ecbplus: relation note differs from its target's instance_id"] += 1
            else:
                cid = f"intra:{doc_id}/r{rel.get('r_id')}"
            for s in rel.findall("source"):
                if s.get("m_id") in chain:
                    problems["ecbplus: mention in two relations"] += 1
                chain[s.get("m_id")] = cid
                label[s.get("m_id")] = (target.get("TAG_DESCRIPTOR") if target is not None else "") or None
        for m in marks:
            if m.find("token_anchor") is None:
                continue
            tids = [a.get("t_id") for a in m.findall("token_anchor")]
            tw = sorted(where[t] for t in tids)
            mid = f"{doc_id}/m{m.get('m_id')}"
            cid = chain.get(m.get("m_id"), f"singleton:{mid}")
            kind = "event" if m.tag.startswith(("ACTION", "NEG_ACTION")) else "entity"
            scope = {"intra": "intra_doc", "singleton": "singleton"}.get(cid.split(":")[0], "cross_doc")
            mentions.append(d.mention(mid, kind, m.tag, tw, cid, [raw[t] for t in sorted(tids, key=lambda t: where[t])],
                                      chain_scope=scope, chain_label=label.get(m.get("m_id")),
                                      scored=(doc_id, tw[0][0]) in validated))
        docs.append({"id": doc_id, "title": None, "created_at": None, "body": d.body,
                     "split": split_of_topic.get(int(topic)), "topic": topic, "subtopic": f"{topic}{sub}"})
    return docs, mentions


def scored(m):
    return m.get("scored", True)


def counts(docs, mentions):
    """Per split, over scored mentions. A chain whose mentions mix kinds counts once under each kind."""
    split = {d["id"]: d["split"] for d in docs}
    out = {}
    for s in SPLITS:
        ms = [m for m in mentions if split[m["doc_id"]] == s and scored(m)]
        size = collections.Counter(m["chain_id"] for m in ms)
        kinds = collections.defaultdict(set)
        for m in ms:
            kinds[m["chain_id"]].add(m["kind"])
        out[s] = {"documents": sum(d["split"] == s for d in docs),
                  "documents_with_mentions": len({m["doc_id"] for m in ms}),
                  "created_at": sum(d["split"] == s and d["created_at"] is not None for d in docs),
                  **{f"{k}_mentions": sum(m["kind"] == k for m in ms) for k in KINDS},
                  **{f"{k}_chains": sum(k in v for v in kinds.values()) for k in KINDS},
                  **{f"{k}_singletons": sum(k in kinds[c] and n == 1 for c, n in size.items()) for k in KINDS}}
        if any("incident" in m for m in ms):
            out[s]["incidents"] = len({m["incident"] for m in ms if m.get("incident")})
        if any(len(v) > 1 for v in kinds.values()):
            out[s]["mixed_kind_chains"] = sum(len(v) > 1 for v in kinds.values())
        if any(not scored(m) for m in mentions):
            out[s]["unscored_mentions"] = sum(split[m["doc_id"]] == s and not scored(m) for m in mentions)
    return out


def flat(text):  # uCDCR re-tokenizes (2-year -> 2 - year) and writes `` and '' as ", so compare without either
    return "".join(text.replace("``", '"').replace("''", '"').split())


def crosscheck(corpus, src, docs, mentions):
    """uCDCR's mention JSON against ours, per split: documents, mentions, and the chain partition.

    A mention is keyed on (document, sentence, its full extent's text, whitespace removed). Chains are
    compared over the mentions whose key is unique on both sides: a disagreement is a mention not on
    the counterpart its chain mostly maps to, counted in whichever direction finds more.
    """
    split = {d["id"]: d["split"] for d in docs}
    body = {d["id"]: d["body"] for d in docs}

    def key_theirs(m):  # uCDCR puts GVC's DCT at sentence 0
        t, sub, n = m["conll_doc_key"].split("/")
        doc = n if corpus == "gvc" else f"{t}_{n}{sub[len(t):]}"
        return doc, m["sent_id"] - (corpus == "gvc"), flat(m["tokens_str"])

    def key_ours(m):
        return m["doc_id"], m["sentence"], flat(body[m["doc_id"]][m["start"]:m["end"]])

    out = {}
    for s, theirs_name in zip(SPLITS, ("train", "val", "test")):
        theirs = []
        for k in KINDS:
            path = fetch(src / "ucdcr", f"{theirs_name}/{k}_mentions.json",
                         f"{UCDCR}/{UCDCR_DIR[corpus]}/{theirs_name}/{k}_mentions.json")
            theirs += json.loads(path.read_text(encoding="utf-8"))
        mine = [m for m in mentions if split[m["doc_id"]] == s and scored(m)]
        a = collections.Counter(key_ours(m) for m in mine)
        b = collections.Counter(key_theirs(m) for m in theirs)
        ca, cb = {key_ours(m): m["chain_id"] for m in mine}, {key_theirs(m): m["coref_chain"] for m in theirs}
        pairs = [(ca[k], cb[k]) for k in a if a[k] == 1 and b.get(k) == 1]

        def disagree(pairs):
            n = 0
            for i in (0, 1):
                by = collections.defaultdict(collections.Counter)
                for p in pairs:
                    by[p[i]][p[1 - i]] += 1
                n = max(n, sum(sum(c.values()) - max(c.values()) for c in by.values()))
            return n

        out[s] = {"ours": sum(a.values()), "ucdcr": sum(b.values()), "matched": sum((a & b).values()),
                  "docs_only_ours": len({k[0] for k in a} - {k[0] for k in b}),
                  "docs_only_ucdcr": len({k[0] for k in b} - {k[0] for k in a}),
                  "chain_pairs_compared": len(pairs), "chain_disagreements": disagree(pairs),
                  "chain_disagreements_outside_intra_doc": disagree([p for p in pairs if not p[0].startswith("intra:")])}
    return out


README = """# {name} — gold for cross-document coreference

Prepared by `research/ontology-apps/cdcr/prepare.py` from the original release. `raw/documents.jsonl`
holds text only; everything here is gold and must not reach a system under test.

## Sources
{sources}

## Licence and attribution
{licence}

Changes made here: tokens re-joined into a body (one space between tokens, one sentence per line);
{changes} Otherwise no token, mention or chain was added, removed or relabelled{relabel}.

## Split
{split}

## Counts (scored mentions)
| split | docs | docs with mentions | docs with created_at | event mentions | entity mentions | event chains | entity chains | event singletons | entity singletons |
|---|---|---|---|---|---|---|---|---|---|
{rows}

A chain is counted in each split it has scored mentions in; a singleton is a chain of one scored mention.
{extra}
## Files
`gold/mentions.jsonl`: doc_id, mention_id, kind (event|entity), type (the corpus's finer tag), sentence
(= line of the body), token_span ([start, end) within that sentence), doc_token_span ([start, end) over
the document), start/end (character offsets into the body), text, chain_id; discontinuous mentions add
`segments` and `doc_tokens`, and `text` is their segments joined by one space. {fields}

## Verification
Offsets: {offsets}
Cross-check against uCDCR (CC BY-SA 4.0; Zhukova, Ruas, Wahle & Gipp, LREC 2026,
https://huggingface.co/datasets/AnZhu/uCDCR at 6c13e77) mention JSON, keyed on (document, sentence,
surface): {cross}
"""

META = {
    "gvc": dict(
        name="GVC (Gun Violence Corpus)",
        sources=f"- {GVC_GIT}/verbose.conll (cltl/GunViolenceCorpus @ 1d80c3a)\n"
                f"- {UKP_GIT}/{{train,dev,test,gvc_doc_to_event}}.csv (UKPLab/cdcr-beyond-corpus-tailored @ 1f60caf)",
        licence="Gun Violence Corpus, CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/). Cite: Piek Vossen,\n"
                "Filip Ilievski, Marten Postma and Roxane Segers. 2018. Don't Annotate, but Validate: a Data-to-Text\n"
                "Method for Capturing Event Data. In Proceedings of LREC 2018. Split files: Apache-2.0, UKP Lab, TU\n"
                "Darmstadt; cite Michael Bugert, Nils Reimers and Iryna Gurevych. 2021. Generalizing Cross-Document\n"
                "Event Coreference Resolution Across Multiple Corpora. Computational Linguistics 47(3).",
        changes="the title sentences open the body (and are also given as `title`); the DCT line became\n"
                "`created_at`; chain 0 (event mentions not linked to the incident) became one singleton chain per\n"
                "mention, `unlinked: true`, as uCDCR does.",
        relabel="",
        split="Bugert et al. 2021, by incident: train.csv/dev.csv/test.csv list the incidents of each split and\n"
              "gvc_doc_to_event.csv maps each document to its incident, as distributed in UKPLab/\n"
              "cdcr-beyond-corpus-tailored (the same files uCDCR reuses). Published: 358/78/74 documents,\n"
              "170/37/34 incidents.",
        fields="GVC adds `incident` (the gold incident id, also the subtopic; null when unlinked) and\n"
               "`unlinked: true` on chain-0 mentions.",
        cross_note="Read at the pinned commits on 2026-10-05: uCDCR lacks a few mentions the CoNLL release has\n"
                   "(e.g. the title's 'dies' in 3ff14dbe98c6b7d81d908f118d5b75c7; ours turn every opening bracket of\n"
                   "verbose.conll into a mention, which the parse-anomaly line would report otherwise), and it\n"
                   "re-tokenizes (2-year -> 2 - year), which the whitespace-free key absorbs.",
    ),
    "ecbplus": dict(
        name="ECB+",
        sources=f"- {ECB_GIT}/ECB+.zip and ECBplus_coreference_sentences.csv (cltl/ecbPlus @ 3aa071e)",
        licence="ECB+ annotation, CC BY 3.0 Unported (http://creativecommons.org/licenses/by/3.0/legalcode),\n"
                "copyright Agata Cybulska, Piek Vossen and the VU University of Amsterdam; the release notes the\n"
                "news texts themselves are not copyrighted. Cite: Agata Cybulska and Piek Vossen. 2014. Using a\n"
                "sledgehammer to crack a nut? Lexical diversity and event coreference resolution. In Proceedings of\n"
                "LREC 2014.",
        changes="sentence 0 (the headline in ECB documents, the source URL in ECB+ ones) is kept as\n"
                "line 0; ECB+ marks no title, so `title` is null, and no creation date, so `created_at` is null.\n"
                "Chains: CROSS_DOC_COREF instance id; INTRA_DOC_COREF `intra:<doc>/r<id>`; unlinked mentions\n"
                "`singleton:<mention>`.",
        relabel="; the tags are the release's (whose own clean-up already gave a linked mention its instance's tag)",
        split="Cybulska & Vossen 2015 by topic: train 1,3,4,6-11,13-17,19,20,22,24-33; dev 2,5,12,18,21,23,34,35;\n"
              "test 36-45 (topics 15 and 17 do not exist in the release). Only mentions in the validated sentences\n"
              "of ECBplus_coreference_sentences.csv are `scored: true` and counted below, as in that setup; the\n"
              "rest are kept with `scored: false`. `topic`/`subtopic` (ecb vs ecbplus, the two seminal events per\n"
              "topic) are in the raw file as the corpus gives them; evaluating with gold subtopics is the easier,\n"
              "commonly reported setting.",
        fields="ECB+ adds `chain_scope` (cross_doc|intra_doc|singleton), `chain_label` (the coder's instance\n"
               "name) and `scored`.",
        cross_note="Read at the pinned commits on 2026-10-05: uCDCR lacks some mentions the XML has (where a\n"
                   "sentence holds two mentions with one surface it can keep one, e.g. 1_14ecb sentence 2 'her'; others\n"
                   "such as 1_7ecb 'rep' and 11_2ecbplus '27/09/2013' are absent); it gives a discontinuous mention its\n"
                   "full extent (ours keep `segments`; the key uses the extent); and it rebuilds INTRA_DOC_COREF chains\n"
                   "(`<TAG>-<topic>ecb-regener-...`), merging some across documents and splitting mixed-kind ones\n"
                   "by tag. The release scopes INTRA_DOC_COREF to one document, as ours do.",
    ),
}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", nargs="+", choices=["gvc", "ecbplus"], default=["gvc", "ecbplus"])
    ap.add_argument("--out", type=pathlib.Path, default=pathlib.Path.home() / ".svrnmesh/bench-corpora")
    ap.add_argument("--src", type=pathlib.Path, help="read release files from DIR/<corpus>/ instead of <out>/<corpus>/src")
    ap.add_argument("--no-crosscheck", action="store_true")
    a = ap.parse_args()
    summary = {}
    for corpus in a.corpus:
        out = a.out / corpus
        src = (a.src / corpus) if a.src else out / "src"
        problems = collections.Counter()
        docs, mentions = (parse_gvc if corpus == "gvc" else parse_ecbplus)(src, problems)
        split, topic = {d["id"]: d["split"] for d in docs}, {d["id"]: d.get("topic") for d in docs}
        spans = collections.defaultdict(lambda: (set(), set(), set()))  # chain -> splits, topics, labels
        for m in mentions:
            for got, v in zip(spans[m["chain_id"]], (split[m["doc_id"]], topic[m["doc_id"]], m.get("chain_label"))):
                got.add(v)
        cross_split = sorted(c for c, (ss, _, _) in spans.items() if len(ss) > 1)
        cross_topic = sorted(c for c, (_, ts, _) in spans.items() if len(ts) > 1)
        problems["document with no split"] += sum(d["split"] is None for d in docs)
        problems = {k: v for k, v in problems.items() if v}
        bad = [m["mention_id"] for m in mentions if not m.pop("_ok")]
        disc = sum("segments" in m for m in mentions)
        stats = counts(docs, mentions)
        cross = None if a.no_crosscheck else crosscheck(corpus, src, docs, mentions)
        for d in ("raw", "gold"):
            (out / d).mkdir(parents=True, exist_ok=True)
        with (out / "raw/documents.jsonl").open("w", encoding="utf-8") as f:
            f.writelines(json.dumps(d, ensure_ascii=False) + "\n" for d in docs)
        with (out / "gold/mentions.jsonl").open("w", encoding="utf-8") as f:
            f.writelines(json.dumps(m, ensure_ascii=False) + "\n" for m in mentions)
        rows = "\n".join(f"| {s} | " + " | ".join(str(c[k]) for k in (
            "documents", "documents_with_mentions", "created_at", "event_mentions", "entity_mentions",
            "event_chains", "entity_chains", "event_singletons", "entity_singletons")) + " |" for s, c in stats.items())
        notes = {"unscored_mentions": "further mentions outside the validated sentences (`scored: false`)",
                 "mixed_kind_chains": "chain(s) mixing event and entity mentions (as released; counted under both)",
                 "incidents": "incidents"}
        extra = "".join(f"\n{s}: {c[k]} {v}." for s, c in stats.items() for k, v in notes.items() if k in c)
        if cross_split or cross_topic:
            eg = ", ".join(sorted({x for c in cross_split for x in spans[c][2] if x}))
            extra += (f"\n\n{len(cross_split)} chain(s) have mentions in two splits and {len(cross_topic)} in two topics, "
                      f"as released (an entity instance reused across topics: {eg or ', '.join(cross_split)}). "
                      "Each split's counts take the chain's mentions in that split.")
        extra += f"\n\nParse anomalies: {json.dumps(problems) if problems else 'none'}.\n"
        offsets = (f"{len(mentions) - len(bad)}/{len(mentions)} mentions pass (body at offsets, whitespace-folded, "
                   f"equals the annotation's tokens; start on line `sentence`); {len(bad)} mismatches"
                   f"{' (' + ', '.join(bad[:5]) + ')' if bad else ''}. {disc} discontinuous mentions are checked "
                   "on their `segments`.")
        cross_txt = ("not run (--no-crosscheck)." if cross is None else "\n\n" + "\n".join(
            f"- {s}: ours {c['ours']}, uCDCR {c['ucdcr']}, matched {c['matched']}; documents only ours "
            f"{c['docs_only_ours']}, only uCDCR {c['docs_only_ucdcr']}; chain partition over "
            f"{c['chain_pairs_compared']} one-to-one mentions, {c['chain_disagreements']} off their chain's counterpart "
            f"({c['chain_disagreements_outside_intra_doc']} outside ECB+ intra-doc chains)"
            for s, c in cross.items()) + "\n\n" + META[corpus]["cross_note"])
        (out / "gold/README.md").write_text(README.format(rows=rows, extra=extra, offsets=offsets, cross=cross_txt,
                                                          **META[corpus]), encoding="utf-8")
        summary[corpus] = {"out": str(out), "documents": len(docs), "mentions": len(mentions),
                           "offset_mismatches": len(bad), "discontinuous": disc, "problems": problems,
                           "chains_in_two_splits": len(cross_split), "chains_in_two_topics": len(cross_topic),
                           "splits": stats, "crosscheck": cross}
    json.dump(summary, sys.stdout, indent=1)
    print()


if __name__ == "__main__":
    main()
