import json, urllib.request, math
exec(open("t4g.py").read().split("out={")[0])  # reuse TEXTS, post, vecs, norm, cos, maxabs
QI="Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: "
EOS="<|endoftext|>"
st,js=post("http://127.0.0.1:9741/v1/embeddings",{"input":TEXTS,"model":"qwen-embedding-0.6b"}); D=vecs(js)
variants={"raw":TEXTS,"text+EOS_literal":[t+EOS for t in TEXTS],"QI+text":[QI+t for t in TEXTS],"QI+text+EOS_literal":[QI+t+EOS for t in TEXTS]}
res={}
for name,inp in variants.items():
    st,js=post("http://127.0.0.1:18433/v1/embeddings",{"input":inp,"model":"x"})
    V=vecs(js)
    tok=post("http://127.0.0.1:18433/tokenize",{"content":inp[0],"add_special":True,"with_pieces":True})[1]["tokens"][-3:]
    res[name]={"cos_vs_daemon":[round(cos(a,b),6) for a,b in zip(V,D)],"max_abs_diff":[round(maxabs(a,b),6) for a,b in zip(V,D)],"last3_tokens_text0":[t["piece"] for t in tok]}
    print(name, res[name])
# daemon determinism: second call
st,js=post("http://127.0.0.1:9741/v1/embeddings",{"input":TEXTS,"model":"qwen-embedding-0.6b"}); D2=vecs(js)
res["daemon_repeat_cos"]=[round(cos(a,b),8) for a,b in zip(D,D2)]
# daemon single-input vs batch
st,js=post("http://127.0.0.1:9741/v1/embeddings",{"input":TEXTS[0],"model":"qwen-embedding-0.6b"}); S=vecs(js)[0]
res["daemon_single_vs_batch_cos_text0"]=round(cos(S,D[0]),8)
st,js=post("http://127.0.0.1:18433/v1/embeddings",{"input":TEXTS[0],"model":"x"}); S2=vecs(js)[0]
st,js=post("http://127.0.0.1:18433/v1/embeddings",{"input":TEXTS,"model":"x"}); B2=vecs(js)[0]
res["ls_single_vs_batch_cos_text0"]=round(cos(S2,B2),8)
print(json.dumps({k:v for k,v in res.items() if k.startswith(("daemon_","ls_"))}))
json.dump(res, open("63-4g-embeddings-variants.json","w"), indent=1)
