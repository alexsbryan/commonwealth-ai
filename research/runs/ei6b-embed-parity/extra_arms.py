import json,math,urllib.request,sys
D=json.load(open("extra_probe.json")); PORT=sys.argv[1]; TAG=sys.argv[2]
URL=f"http://127.0.0.1:{PORT}/v1/embeddings"; EOS="<|endoftext|>"
Q="Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: "
def emb(t,u=URL):
    r=urllib.request.Request(u,data=json.dumps({"input":t,"model":"m"}).encode(),headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(r,timeout=180) as f: return [float(x) for x in json.loads(f.read())["data"][0]["embedding"]]
def cos(a,b):
    d=sum(x*y for x,y in zip(a,b));na=math.sqrt(sum(x*x for x in a));nb=math.sqrt(sum(x*x for x in b))
    return d/(na*nb) if na and nb else float('nan')
w=[cos(emb(r["content"]+EOS),r["stored"]) for r in D["wiki"]]
print(f"  pooling={TAG:<5} wikipedia chunks (doc+EOS)      mean={sum(w)/len(w):.4f}  per={[round(x,4) for x in w]}")
s=[cos(emb(Q+f"{r['data']['canonical_name']}: {r['data']['description']}"+EOS),r["stored"]) for r in D["seplug"]]
print(f"  pooling={TAG:<5} sep-al-farabi seeds (query+EOS)  mean={sum(s)/len(s):.4f}  per={[round(x,4) for x in s]}")
