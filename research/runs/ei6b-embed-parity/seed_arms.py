import json,math,urllib.request,sys
S=json.load(open("seed_probe.json")); PORT=sys.argv[1]
URL=f"http://127.0.0.1:{PORT}/v1/embeddings"; EOS="<|endoftext|>"
Q="Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: "
def emb(t):
    r=urllib.request.Request(URL,data=json.dumps({"input":t,"model":"m"}).encode(),headers={"Content-Type":"application/json"})
    with urllib.request.urlopen(r,timeout=180) as f: return [float(x) for x in json.loads(f.read())["data"][0]["embedding"]]
def cos(a,b):
    d=sum(x*y for x,y in zip(a,b));na=math.sqrt(sum(x*x for x in a));nb=math.sqrt(sum(x*x for x in b))
    return d/(na*nb) if na and nb else float('nan')
def cands(d):
    n=d.get("canonical_name",""); desc=d.get("description","") or ""
    return {"name":n, "name+desc":f"{n}: {desc}" if desc else n}
arms={"raw":lambda t:t, "raw+EOS":lambda t:t+EOS, "Q+t":lambda t:Q+t, "Q+t+EOS":lambda t:Q+t+EOS}
print(f"{'text form':<12}{'arm':<10}{'mean cos vs stored seed':>26}   per-atom")
for form in ["name","name+desc"]:
    for an,fn in arms.items():
        sims=[cos(emb(fn(cands(o['data'])[form])), o["stored"]) for o in S]
        print(f"{form:<12}{an:<10}{sum(sims)/len(sims):>26.4f}   {[round(x,4) for x in sims]}")
