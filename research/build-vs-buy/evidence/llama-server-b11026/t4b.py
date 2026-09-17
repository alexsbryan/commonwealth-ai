import json, urllib.request
BASE="http://127.0.0.1:18431"
def post(path, body):
    req=urllib.request.Request(BASE+path, data=json.dumps(body).encode(), headers={"Content-Type":"application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r: return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e: return e.code, json.loads(e.read() or b"{}")
res={}
prompt="<|im_start|>user\nIs the Earth round? Answer A for yes or B for no. Reply with one letter.<|im_end|>\n<|im_start|>assistant\n"
g='root ::= "A" | "B"'
for label, extra in [("native_default", {}), ("native_post_sampling_probs", {"post_sampling_probs": True}), ("native_no_grammar", {"grammar": None})]:
    body={"prompt":prompt,"n_predict":1,"n_probs":10,"temperature":0,"grammar":g, **extra}
    if body.get("grammar") is None: body.pop("grammar")
    st, js = post("/completion", body)
    res[label]={"http":st,"content":js.get("content"),"completion_probabilities":js.get("completion_probabilities"),"top_level_keys":sorted(js.keys())}
    print("==",label, st, "keys:", sorted(js.keys()))
    print(json.dumps(js.get("completion_probabilities"), indent=0)[:1500])
# chat with logprobs + grammar
for label, extra in [("chat_logprobs_grammar", {"grammar": g}), ("chat_logprobs_no_grammar", {})]:
    body={"messages":[{"role":"user","content":"Is the Earth round? Answer A for yes or B for no. Reply with one letter."}],"max_tokens":1,"temperature":0,"logprobs":True,"top_logprobs":5, **extra}
    st, js = post("/v1/chat/completions", body)
    ch = js.get("choices",[{}])[0] if st==200 else {}
    res[label]={"http":st,"message":ch.get("message"),"logprobs":ch.get("logprobs"),"error":js.get("error")}
    print("==",label, st, json.dumps(ch.get("message")), "\n", json.dumps(ch.get("logprobs"))[:1500], js.get("error"))
json.dump(res, open("21-4b-forced-choice.json","w"), indent=1)
