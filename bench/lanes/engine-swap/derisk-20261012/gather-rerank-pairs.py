"""The rerank spike's input: the first 8 questions of bench/lanes/sep/questions.toml,
each with its top 40 SEP search hits from the running daemon. Not committed (SEP text).
Usage: python3 gather-rerank-pairs.py <out-dir>   (from the repo root)"""
import json, subprocess, sys, tomllib
bank = tomllib.load(open("bench/lanes/sep/questions.toml", "rb"))
rows = bank.get("questions") or next(v for v in bank.values() if isinstance(v, list))
out = []
for q in [(r.get("question") or r.get("text")).strip() for r in rows][:8]:
    r = subprocess.run(["sovereign", "corpus", "search", "sep", q, "--limit", "40", "--json"],
                       capture_output=True, text=True, timeout=300, check=True)
    out.append({"query": q, "documents": [h["content"] for h in json.loads(r.stdout)]})
json.dump(out, open(f"{sys.argv[1]}/rerank-pairs.json", "w"))
