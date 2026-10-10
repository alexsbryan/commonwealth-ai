#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Probe what the quality check did not reach, on the daemon's engine (L)
against llama-server (built with llguidance at the vendored commit).

  grammar  The three production Lark grammars, used verbatim:
           - raptor_atlas.rs:1283, the summary JSON with capitalised entities;
           - skeleton.rs:225, the window entity list;
           - skeleton.rs:607, segment naming with n = 3.
           Each grammar is sent to L as `lark_grammar` (the field the remote
           client writes) and to llama-server as `grammar: "%llguidance {}\\n"
           + lark`, over the same 5 passages. Per side it records: did the
           request succeed, and does every output match the grammar?
  mask     The two decode-time allow-lists, which have no llama-server
           counterpart.
           - L gets `url_allowlist` or `evidence_id_allowlist`; llama-server
             gets the same prompt unconstrained.
           - The probe counts outputs that name a URL or `ev-` handle outside
             the allow-list. That is what an outside-the-model check would
             have to catch.
  rerank   One query against 4 relevant and 4 irrelevant documents, taken
           from `examples/rerank_pairs_probe.rs`. It compares llama-server's
           `/v1/rerank` (qwen3-reranker-0.6b, `--reranking`) with the
           in-process scores that example prints on its sanity line.

    probes.py grammar --daemon http://127.0.0.1:9741 --server http://127.0.0.1:18320
    probes.py mask    --daemon http://127.0.0.1:9741 --server http://127.0.0.1:18320
    probes.py rerank  --server http://127.0.0.1:18321 --inproc "<scores list from the example>"
"""
import argparse
import json
import re
import sys
import urllib.error
import urllib.request

CAP = r"[A-Z][A-Za-z'.]*(?: [A-Z][A-Za-z'.]*)*"
FUNC = "Introduces|Develops|Complicates|Resolves|Transitions|Evidences"

GRAMMARS = {
    "raptor_summary": (
        r'''
start: "{\"summary\": \"" summary "\", \"primary_entities\": [" entities "]}"
summary: NOQUOTE+
entities: (entity (", " entity)*)?
entity: "\"" CAP_NAME "\""
NOQUOTE: /[^"\\]/
CAP_NAME: /[A-Z][A-Za-z'.]*( [A-Z][A-Za-z'.]*)*/
''',
        re.compile(r'\{"summary": "[^"\\]+", "primary_entities": \[(?:"' + CAP + r'"(?:, "' + CAP + r'")*)?\]\}'),
        "Summarise the passage in one sentence and list its primary named entities as JSON "
        '{{"summary": ..., "primary_entities": [...]}}.\n\nPassage:\n{p}',
    ),
    "skeleton_entities": (
        "start: line\nline: (entity (\",\" \" \"? entity)*)?\nentity: /[A-Z][A-Za-z'.]*( [A-Z][A-Za-z'.]*)*/\n",
        re.compile(r"(?:" + CAP + r"(?:, ?" + CAP + r")*)?"),
        "List the named entities in the passage, canonical names exactly as they appear, "
        "ONE comma-separated list, each name once.\n\nPassage:\n{p}\n\nAnswer (one line):",
    ),
    "skeleton_segments_n3": (
        'start: line "\\n" line "\\n" line\n'
        'line: /[0-9]+/ "|" /[^|\\n]{1,80}/ "|" func\n'
        f'func: {" | ".join(chr(34) + f + chr(34) for f in FUNC.split("|"))}\n',
        re.compile(r"(?:[0-9]+\|[^|\n]{1,80}\|(?:" + FUNC + r"))(?:\n[0-9]+\|[^|\n]{1,80}\|(?:" + FUNC + r")){2}"),
        "Name 3 segments of the passage, one line each as index|title|function, where "
        f"function is one of {FUNC.replace('|', ', ')}.\n\nPassage:\n{{p}}\n\nAnswer (3 lines):",
    ),
}

PASSAGES = [
    "Immanuel Kant published the Groundwork of the Metaphysics of Morals in Riga in 1785. "
    "There he introduced the Categorical Imperative, which Arthur Schopenhauer later attacked.",
    "The Treaty of Westphalia, signed in Osnabrück and Münster in 1648, ended the Thirty Years' War "
    "and shaped the sovereignty of the Holy Roman Empire's states.",
    "Marie Curie and Pierre Curie discovered polonium and radium in Paris; she later won a second "
    "Nobel Prize, in Chemistry, in 1911.",
    "The Apollo 11 mission, commanded by Neil Armstrong with Buzz Aldrin and Michael Collins, "
    "landed in the Sea of Tranquility on July 20, 1969.",
    "Ada Lovelace wrote notes on Charles Babbage's Analytical Engine, including what is often "
    "called the first computer program, published in Taylor's Scientific Memoirs.",
]


def post(url: str, body: dict, timeout: int = 300):
    req = urllib.request.Request(url, json.dumps(body).encode(), {"content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        return e.code, {"error": e.read().decode("utf-8", "replace")[:300]}


def content(answer: dict) -> str:
    return ((answer.get("choices") or [{}])[0].get("message") or {}).get("content") or ""


def chat(base, model, prompt, extra):
    body = {"model": model, "messages": [{"role": "user", "content": prompt}], "max_tokens": 300,
            "temperature": 0.1, "chat_template_kwargs": {"enable_thinking": False}, **extra}
    return post(f"{base}/v1/chat/completions", body)


def grammar(a) -> int:
    for name, (lark, check, template) in GRAMMARS.items():
        for side, base, model, extra in (
            ("L", a.daemon, a.daemon_model, {"lark_grammar": lark}),
            ("llama-server", a.server, a.server_model, {"grammar": "%llguidance {}\n" + lark}),
        ):
            ok = valid = 0
            sample = ""
            for p in PASSAGES:
                status, answer = chat(base, model, template.format(p=p), extra)
                text = content(answer).strip()
                ok += status == 200
                valid += status == 200 and bool(check.fullmatch(text))
                sample = sample or (text[:90] if status == 200 else str(answer)[:120])
            print(f"{name:22} {side:13} succeeded {ok}/5  matches grammar {valid}/5  e.g. {sample!r}")
    return 0


def mask(a) -> int:
    allowed_urls = ["https://plato.stanford.edu/entries/kant-moral/",
                    "https://en.wikipedia.org/wiki/Categorical_imperative"]
    url_prompt = ("Give three web links where a student can read about Kant's categorical imperative, "
                  "one per line, then one sentence on each.")
    ev_ids = ["ev-T1-0001", "ev-T1-0002"]
    ev_prompt = ("Evidence:\n[ev-T1-0001] Kant published the Groundwork in 1785.\n"
                 "[ev-T1-0002] The categorical imperative binds unconditionally.\n\n"
                 "Write four sentences about Kant's ethics. Cite evidence handles in brackets after "
                 "each claim, like [ev-T1-0001].")
    url_re = re.compile(r"https?://[^\s)\]>\"']+")
    ev_re = re.compile(r"ev-T\d+-\d{4}")
    for kind, prompt, field, allowed, find in (
        ("url", url_prompt, "url_allowlist", allowed_urls, url_re),
        ("evidence-id", ev_prompt, "evidence_id_allowlist", ev_ids, ev_re),
    ):
        for side, base, model, extra in (
            ("L, masked", a.daemon, a.daemon_model, {field: allowed}),
            ("llama-server", a.server, a.server_model, {}),
        ):
            outside = total = 0
            seen = []
            for _ in range(5):
                status, answer = chat(base, model, prompt, extra)
                found = [m.rstrip(".,;") for m in find.findall(content(answer))]
                total += len(found)
                bad = [f for f in found if f not in allowed]
                outside += len(bad)
                seen += bad[:2]
            print(f"{kind:12} {side:13} cited {total:3}  outside the allow-list {outside:3}  e.g. {seen[:3]}")
    return 0


def rerank(a) -> int:
    sys.path.insert(0, "")
    src = open("serve/crates/sovereign-inference/examples/rerank_pairs_probe.rs").read()
    query = re.search(r'const QUERY: &str = "(.*?)";', src, re.S).group(1)
    docs = re.findall(r'^\s+"(.*?)",$', src[src.index("const RELEVANT"):src.index("fn main")], re.S | re.M)
    clean = lambda s: re.sub(r"\\\n\s*", "", s)
    query, docs = clean(query), [clean(d) for d in docs]
    status, answer = post(f"{a.server}/v1/rerank", {"model": "reranker", "query": query, "documents": docs})
    if status != 200:
        print(f"llama-server /v1/rerank: HTTP {status} {answer}")
        return 1
    scores = [0.0] * len(docs)
    for r in answer["results"]:
        scores[r["index"]] = r["relevance_score"]
    inproc = json.loads(a.inproc)
    order = lambda xs: sorted(range(len(xs)), key=lambda i: -xs[i])
    print(f"docs {len(docs)} (first 4 relevant)")
    print(f"llama-server scores {[round(s, 4) for s in scores]}")
    print(f"in-process   scores {[round(s, 4) for s in inproc]}")
    print(f"llama-server order {order(scores)}  relevant mean {sum(scores[:4]) / 4:+.4f}  "
          f"irrelevant mean {sum(scores[4:]) / 4:+.4f}  max |score| {max(abs(s) for s in scores):.3e}")
    print(f"in-process   order {order(inproc)}")
    top4 = lambda xs: set(order(xs)[:4])
    print(f"top-4 sets agree: {top4(scores) == top4(inproc)}; both rank all relevant first: "
          f"{top4(scores) == {0, 1, 2, 3} and top4(inproc) == {0, 1, 2, 3}}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("probe", choices=["grammar", "mask", "rerank"])
    ap.add_argument("--daemon")
    ap.add_argument("--daemon-model", default="fast")
    ap.add_argument("--server")
    ap.add_argument("--server-model", default="Qwen3.5-4B-UD-MTP-Q6_K_XL")
    ap.add_argument("--inproc", help="rerank: the in-process scores as a JSON list")
    a = ap.parse_args()
    return {"grammar": grammar, "mask": mask, "rerank": rerank}[a.probe](a)


if __name__ == "__main__":
    sys.exit(main())
