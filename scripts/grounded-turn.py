#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""grounded-turn.py — one corpus-grounded retrieval (and optional synthesis) turn
against a `sovereign daemon` HTTP API, for the mesh-soak grounding oracle.

There is NO one-shot RAG endpoint on the daemon — a grounded turn is three calls
on the client port (the fused answer+provenance only exists in-process in the
desktop/CLI Runtime, never as a daemon route):

  A  POST /v1/embeddings        embed the question        -> query vector
  B  POST /v1/knowledge/search  retrieve evidence chunks  -> results[].content (TEXT)
  C  POST /v1/chat/completions  synthesize from ONLY      -> choices[0].message
                                those chunks  (optional, --synthesize)

knowledge/search REQUIRES the query vector (it does not embed for you), so A is
mandatory before B. Output is one line of JSON on stdout (diagnostics to stderr).
With --score-input it also writes the {question, answer, chunks} triple that
`sovereign bench chaos-monkey score-answer --input <file>` consumes, so the
grounding verdict is the bench's shared `assess_asserted_value` primitive — the
same judgment the live grounding gate makes — not a hand-rolled judge.
"""
import argparse
import json
import sys
import urllib.error
import urllib.request


def post(base, path, body, timeout):
    req = urllib.request.Request(
        base.rstrip("/") + path,
        data=json.dumps(body).encode(),
        headers={"content-type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--base-url", required=True, help="node client base, e.g. http://127.0.0.1:19741")
    ap.add_argument("--corpus", required=True, help="corpus id to scope retrieval to")
    ap.add_argument("--question", required=True)
    ap.add_argument("--limit", type=int, default=8, help="max chunks to retrieve")
    ap.add_argument("--embed-model", default="embed")
    ap.add_argument("--chat-model", default="primary")
    ap.add_argument("--synthesize", action="store_true", help="also generate a grounded answer (call C)")
    ap.add_argument("--max-tokens", type=int, default=160)
    ap.add_argument("--timeout", type=float, default=120.0)
    ap.add_argument("--score-input", help="write {question,answer,chunks} JSON here for score-answer")
    a = ap.parse_args()

    out = {
        "corpus": a.corpus,
        "n_chunks": 0,
        "corpora_searched": [],
        "corpora_unavailable": [],
        "has_answer": False,
        "answer": None,
        "error": None,
    }
    try:
        # A — embed the question (knowledge/search needs the vector; it won't embed for you).
        emb = post(a.base_url, "/v1/embeddings", {"model": a.embed_model, "input": a.question}, a.timeout)
        vec = emb["data"][0]["embedding"]

        # B — retrieve evidence chunks, scoped to the one corpus.
        ks = post(
            a.base_url,
            "/v1/knowledge/search",
            {"query_embedding": vec, "query_text": a.question, "corpora": [a.corpus], "limit": a.limit},
            a.timeout,
        )
        chunks = [r.get("content", "") for r in ks.get("results", []) if r.get("content")]
        out["n_chunks"] = len(chunks)
        out["corpora_searched"] = ks.get("corpora_searched") or []
        out["corpora_unavailable"] = ks.get("corpora_unavailable") or []

        answer = None
        # C — synthesize an answer grounded ONLY in those chunks (optional).
        if a.synthesize and chunks:
            ctx = "\n\n---\n\n".join(chunks)
            cc = post(
                a.base_url,
                "/v1/chat/completions",
                {
                    "model": a.chat_model,
                    "stream": False,
                    "max_tokens": a.max_tokens,
                    "messages": [
                        {
                            "role": "system",
                            "content": (
                                "Answer the question using ONLY the provided context. "
                                "If the context does not contain the answer, say you do not know."
                            ),
                        },
                        {"role": "user", "content": f"Context:\n{ctx}\n\nQuestion: {a.question}"},
                    ],
                },
                a.timeout,
            )
            answer = cc["choices"][0]["message"]["content"]
            out["answer"] = answer
            out["has_answer"] = bool(answer and answer.strip())

        # The score-answer triple — only meaningful once we actually synthesized.
        if a.score_input and answer is not None:
            with open(a.score_input, "w") as f:
                json.dump({"question": a.question, "answer": answer, "chunks": chunks}, f)
    except urllib.error.HTTPError as e:
        body = ""
        try:
            body = e.read()[:200].decode("utf-8", "replace")
        except Exception:
            pass
        out["error"] = f"http {e.code} on {e.url}: {body}"
    except Exception as e:  # noqa: BLE001 — the oracle must never crash the soak; surface, don't raise.
        out["error"] = f"{type(e).__name__}: {e}"

    print(json.dumps(out))
    return 0 if out["error"] is None else 1


if __name__ == "__main__":
    sys.exit(main())
