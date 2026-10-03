import json,sys,math,os,urllib.request
D=json.load(open("pool_sweep_texts.json")); PORT=sys.argv[1]; TAG=sys.argv[2]
URL=f"http://127.0.0.1:{PORT}/v1/embeddings"; EOS="<|endoftext|>"
def emb(t):
    r=urllib.request.Request(URL,data=json.dumps({"input":t,"model":"m"}).encode(),headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(r,timeout=180) as f: return [float(x) for x in json.loads(f.read())["data"][0]["embedding"]]
def cos(a,b):
    d=sum(x*y for x,y in zip(a,b));na=math.sqrt(sum(x*x for x in a));nb=math.sqrt(sum(x*x for x in b))
    return d/(na*nb) if na and nb else float('nan')
for c,rows in D.items():
    raw=[];eos=[]
    for r in rows:
        raw.append(cos(emb(r["content"]),r["stored"]))
        eos.append(cos(emb(r["content"]+EOS),r["stored"]))
    print(f"  pooling={TAG:<5} {c:<14} raw={sum(raw)/len(raw):.4f}  +EOS={sum(eos)/len(eos):.4f}   raw_per={[round(x,3) for x in raw]}")
