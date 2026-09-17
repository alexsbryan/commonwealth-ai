import json, urllib.request, math
TEXTS=["The quick brown fox jumps over the lazy dog.",
       "Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery: how do vaccines train the immune system",
       "fn main() { println!(\"hello, world\"); }"]
def post(url, body, timeout=120):
    req=urllib.request.Request(url, data=json.dumps(body).encode(), headers={"Content-Type":"application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r: return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        b=e.read(); 
        try: return e.code, json.loads(b)
        except Exception: return e.code, {"raw": b.decode(errors="replace")}
def vecs(js): return [d["embedding"] for d in sorted(js["data"], key=lambda d: d["index"])]
def norm(v): return math.sqrt(sum(x*x for x in v))
def cos(a,b): return sum(x*y for x,y in zip(a,b))/(norm(a)*norm(b))
def maxabs(a,b): return max(abs(x-y) for x,y in zip(a,b))
out={"texts":TEXTS}
sources={}
st,js=post("http://127.0.0.1:18433/v1/embeddings",{"input":TEXTS,"model":"x"}); out["ls_f16_default_pooling_http"]=st; sources["llama-server F16 (same file as daemon), default pooling, /v1/embeddings"]=vecs(js)
st,js=post("http://127.0.0.1:18434/v1/embeddings",{"input":TEXTS,"model":"x"}); out["ls_q8_http"]=st; sources["llama-server Q8_0, --pooling last, /v1/embeddings"]=vecs(js)
# native endpoint, unnormalized check
st,js=post("http://127.0.0.1:18433/embedding",{"content":TEXTS[0]}); out["ls_native_embedding_http"]=st
try:
    ne=js[0]["embedding"]; ne = ne[0] if isinstance(ne[0], list) else ne
    out["ls_native_embedding_norm"]=norm(ne); out["ls_native_embedding_dim"]=len(ne)
except Exception as e: out["ls_native_embedding_err"]=str(e)+" "+json.dumps(js)[:300]
st,js=post("http://127.0.0.1:9741/v1/embeddings",{"input":TEXTS,"model":"qwen-embedding-0.6b"}, timeout=180)
out["daemon_http"]=st
if st==200: sources["sovereign daemon :9741 /v1/embeddings (embedded engine, qwen-embedding-0.6b.gguf F16)"]=vecs(js); out["daemon_model_echo"]=js.get("model")
else: out["daemon_error"]=js
names=list(sources)
out["per_source"]={n:{"dims":[len(v) for v in sources[n]],"l2_norms":[round(norm(v),6) for v in sources[n]]} for n in names}
pairs={}
for i in range(len(names)):
    for j in range(i+1,len(names)):
        a,b=sources[names[i]],sources[names[j]]
        pairs[f"{names[i]}  VS  {names[j]}"]=[{"cos":round(cos(x,y),6),"max_abs_diff":round(maxabs(x,y),6) if len(x)==len(y) else None} for x,y in zip(a,b)]
out["pairs"]=pairs
# within-source sanity: cross-text cosine
out["cross_text_cos_daemon"]=[round(cos(sources[names[-1]][0],sources[names[-1]][k]),4) for k in (1,2)] if len(names)==3 else None
json.dump(out, open("62-4g-embeddings-compare.json","w"), indent=1)
json.dump({n:[v[:8] for v in sources[n]] for n in names}, open("62b-4g-embeddings-first8.json","w"), indent=1)
print(json.dumps({k:v for k,v in out.items() if k!="texts"}, indent=1))
