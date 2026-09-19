import json, urllib.request, jsonschema
BASE="http://127.0.0.1:18431"
def post(path, body):
    req=urllib.request.Request(BASE+path, data=json.dumps(body).encode(), headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(req, timeout=120) as r: return r.status, json.loads(r.read())
schema={"type":"object","properties":{"city":{"type":"string","maxLength":40},"population":{"type":"integer","minimum":0},"is_capital":{"type":"boolean"},"tags":{"type":"array","items":{"type":"string","enum":["coastal","inland","historic"]},"maxItems":3}},"required":["city","population","is_capital","tags"],"additionalProperties":False}
cities=["Lisbon","Denver","Kyoto","Cairo","Oslo"]
tally={}
for label in ["response_format","top_level_json_schema"]:
    ok=0; n=0; bad=[]
    for c in cities:
        for temp in (0.0, 1.2):
            extra={"response_format":{"type":"json_schema","json_schema":{"name":"city","schema":schema}}} if label=="response_format" else {"json_schema":schema}
            st, js = post("/v1/chat/completions", {"messages":[{"role":"user","content":f"Describe {c} as JSON."}],"max_tokens":300,"temperature":temp, **extra})
            m=js["choices"][0]["message"]; content=m.get("content") or ""
            n+=1
            try: jsonschema.validate(json.loads(content), schema); ok+=1
            except Exception as e: bad.append({"city":c,"temp":temp,"content":content[:200],"reasoning":(m.get("reasoning_content") or "")[:100],"finish":js["choices"][0]["finish_reason"],"err":str(e).splitlines()[0][:120]})
    tally[label]={"valid":ok,"n":n,"failures":bad}
    print(label, f"{ok}/{n}", json.dumps(bad)[:800])
json.dump(tally, open("20b-4a-json-schema-repeats.json","w"), indent=1)
