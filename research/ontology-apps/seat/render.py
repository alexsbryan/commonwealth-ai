"""The model's seat: print captured requests (proxy.py's REQUESTS_DIR) as the model received them, grouped by question
type (the system prompt's first line), with the answer distribution. Filters: a word the user message must contain,
and how many per type.
    render.py REQUESTS_DIR [--grep WORD] [--per-type N] [--full]"""
import argparse, collections, json, pathlib

a = argparse.ArgumentParser()
a.add_argument("requests")
a.add_argument("--grep")
a.add_argument("--per-type", type=int, default=1)
a.add_argument("--full", action="store_true", help="print the numbered document too (it is elided by default)")
o = a.parse_args()
shown = collections.Counter()
for p in sorted(pathlib.Path(o.requests).glob("*.json")):
    d = json.loads(p.read_text())
    msgs = d["request"]["messages"]
    system = next((m["content"] for m in msgs if m["role"] == "system"), "")
    user = "\n".join(m["content"] for m in msgs if m["role"] == "user")
    kind = system.splitlines()[0] if system else "(no system prompt)"
    if (o.grep and o.grep not in user) or shown[kind] >= o.per_type:
        continue
    shown[kind] += 1
    if not o.full and "<<<\n" in user:
        head, rest = user.split("<<<\n", 1)
        doc, tail = rest.split("\n>>>", 1)
        user = f"{head}<<<\n[{doc.count(chr(10)) + 1} numbered lines elided; --full shows them]\n>>>{tail}"
    answer = ((d.get("response") or {}).get("choices") or [{}])[0].get("message", {}).get("content")
    print(f"===== {p.name}  [{kind}]\n--- system\n{system}\n--- user\n{user}\n--- answer\n{answer}\n")
print("question types:", dict(shown))
