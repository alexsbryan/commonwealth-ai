import json, urllib.request, jsonschema, sys
BASE="http://127.0.0.1:18431"
def post(path, body):
    req=urllib.request.Request(BASE+path, data=json.dumps(body).encode(), headers={"Content-Type":"application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r: return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e: return e.code, json.loads(e.read() or b"{}")
schema={"type":"object","properties":{"city":{"type":"string"},"population":{"type":"integer","minimum":0},"is_capital":{"type":"boolean"},"tags":{"type":"array","items":{"type":"string","enum":["coastal","inland","historic"]},"maxItems":3}},"required":["city","population","is_capital","tags"],"additionalProperties":False}
msgs=[{"role":"user","content":"Describe Lisbon as a JSON object."}]
out={}
for label, extra in [
  ("response_format.json_schema", {"response_format":{"type":"json_schema","json_schema":{"name":"city","strict":True,"schema":schema}}}),
  ("top_level_json_schema", {"json_schema":schema}),
  ("response_format.json_object+schema", {"response_format":{"type":"json_object","schema":schema}}),
  ("no_constraint_control", {}),
]:
    body={"messages":msgs,"max_tokens":200,"temperature":0.7, **extra}
    st, js = post("/v1/chat/completions", body)
    content = js.get("choices",[{}])[0].get("message",{}).get("content") if st==200 else None
    valid=None; err=None
    if content is not None:
        try:
            jsonschema.validate(json.loads(content), schema); valid=True
        except Exception as e: valid=False; err=str(e).splitlines()[0][:160]
    out[label]={"http":st,"content":content,"schema_valid":valid,"validation_error":err, "error": js.get("error") if st!=200 else None, "finish_reason": js.get("choices",[{}])[0].get("finish_reason") if st==200 else None}
    print(label, st, valid, err, repr(content)[:300])
json.dump(out, open("20-4a-json-schema.json","w"), indent=1)
