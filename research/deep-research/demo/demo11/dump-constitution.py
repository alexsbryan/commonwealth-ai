import json, pathlib, re

run = pathlib.Path("research/deep-research/demo/demo11/runs/compounding/run-a/dr-1786978547")

# The failing claims from gap-list-1.json
gl = json.load(open(run / "gap-list-1.json"))
print("gap-list-1 top keys:", sorted(gl.keys()))
claims = gl.get("claims", [])
print("gap-list-1 claims:", len(claims))
for c in claims:
    v = (c.get("verdict") or c.get("status") or "").lower()
    if v == "passed":
        print("---")
        print("id:", c.get("id"))
        print("verdict:", v)
        print("text:", (c.get("text") or c.get("claim") or "")[:220])

# The evidence window
for w in sorted(run.glob("evidence-window-*.json")):
    d = json.load(open(w))
    print("\n== ", w.name, "==")
    for c in d.get("chunks", []):
        content = c["content"]
        for probe in ["214", "1901", "34", "electrif"]:
            if probe in content:
                idx = content.find(probe)
                print(f"[{probe}] ...{content[max(0,idx-60):idx+60]}...")
