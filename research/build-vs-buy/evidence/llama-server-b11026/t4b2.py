import json, urllib.request
BASE="http://127.0.0.1:18431"
def post(path, body):
    req=urllib.request.Request(BASE+path, data=json.dumps(body).encode(), headers={"Content-Type":"application/json"})
    try:
        with urllib.request.urlopen(req, timeout=120) as r: return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e: return e.code, json.loads(e.read() or b"{}")
res={}
prompt="<|im_start|>user\nIs Paris the capital of Germany? Answer A for yes or B for no. Reply with one letter.<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
g='root ::= "A" | "B"'
def brief(cp):
    out=[]
    for c in cp or []:
        tops=c.get("top_logprobs") or c.get("top_probs")
        out.append({"token":c["token"], "logprob":c.get("logprob"), "prob":c.get("prob"), "top":[(t["token"], round(t.get("logprob", t.get("prob")),4)) for t in tops]})
    return out
for label, extra in [
  ("native_pre_grammar_default_temp0", {"temperature":0}),
  ("native_post_sampling_temp1_open_samplers", {"post_sampling_probs":True,"temperature":1.0,"top_k":0,"top_p":1.0,"min_p":0.0}),
  ("native_post_sampling_temp0", {"post_sampling_probs":True,"temperature":0}),
]:
    body={"prompt":prompt,"n_predict":1,"n_probs":10,"grammar":g, **extra}
    st, js = post("/completion", body)
    res[label]={"http":st,"content":js.get("content"),"completion_probabilities":js.get("completion_probabilities")}
    print("==",label, st, js.get("content"), json.dumps(brief(js.get("completion_probabilities"))))
for label, extra in [
  ("chat_logprobs_grammar_nothink", {"grammar": g, "chat_template_kwargs":{"enable_thinking":False}}),
  ("chat_logprobs_grammar_nothink_temp1", {"grammar": g, "chat_template_kwargs":{"enable_thinking":False}, "temperature":1.0}),
]:
    body={"messages":[{"role":"user","content":"Is Paris the capital of Germany? Answer A for yes or B for no. Reply with one letter."}],"max_tokens":1,"temperature":0,"logprobs":True,"top_logprobs":5, **extra}
    st, js = post("/v1/chat/completions", body)
    ch = js.get("choices",[{}])[0] if st==200 else {}
    res[label]={"http":st,"message":ch.get("message"),"logprobs":ch.get("logprobs"),"error":js.get("error")}
    print("==",label, st, json.dumps(ch.get("message")), json.dumps(ch.get("logprobs")), js.get("error"))
json.dump(res, open("21b-4b-forced-choice-nothink.json","w"), indent=1)
