import json, sys, urllib.request, math, os

TEXTS = json.load(open(sys.argv[1]))
PORT  = sys.argv[2]
BARE  = f"http://127.0.0.1:{PORT}/v1/embeddings"
DAEMON= "http://127.0.0.1:9741/v1/embeddings"
EOS   = "<|endoftext|>"
QINSTR= "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: "

def post(url, body, timeout=180):
    req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())

def embed(url, text, model):
    b = post(url, {"input": text, "model": model})
    return [float(x) for x in b["data"][0]["embedding"]]

def cos(a, b):
    if len(a) != len(b): return float("nan")
    d = sum(x*y for x, y in zip(a, b))
    na = math.sqrt(sum(x*x for x in a)); nb = math.sqrt(sum(x*x for x in b))
    return d/(na*nb) if na and nb else float("nan")

BARE_MODEL   = os.environ.get("BARE_MODEL", "Qwen3-Embedding-0.6B-Q8_0.gguf")
DAEMON_MODEL = os.environ.get("DAEMON_MODEL", "embed:default")

rows = []
for t in TEXTS:
    S = t["stored"]; T = t["content"]
    r = {"id": t["id"], "title": t["title"], "chars": len(T)}
    try:    A = embed(DAEMON, T, DAEMON_MODEL); r["A_daemon_vs_stored"] = cos(A, S)
    except Exception as e: A = None; r["A_daemon_vs_stored"] = f"ERR {e}"
    B = embed(BARE, T, BARE_MODEL);                r["B_bare_raw_vs_stored"]   = cos(B, S)
    C = embed(BARE, T + EOS, BARE_MODEL);          r["C_bare_eos_vs_stored"]   = cos(C, S)
    D = embed(BARE, QINSTR + T + EOS, BARE_MODEL); r["D_bare_query_vs_stored"] = cos(D, S)
    if A: r["C_vs_A"] = cos(C, A); r["B_vs_A"] = cos(B, A)
    r["_A8"] = A[:8] if A else None
    r["_C8"] = C[:8]
    r["_B8"] = B[:8]
    rows.append(r)

json.dump(rows, open(os.environ.get("OUT", "arms-out.json"), "w"), indent=2)
def fmt(v): return f"{v:.4f}" if isinstance(v, float) else str(v)
print(f"{'chunk':<12}{'chars':>6}  {'A dmn/stored':>13}{'B raw/stored':>14}{'C +EOS/stored':>15}{'D query/stored':>16}{'B vs A':>9}{'C vs A':>9}")
for r in rows:
    print(f"{str(r['id']):<12}{r['chars']:>6}  {fmt(r['A_daemon_vs_stored']):>13}{fmt(r['B_bare_raw_vs_stored']):>14}"
          f"{fmt(r['C_bare_eos_vs_stored']):>15}{fmt(r['D_bare_query_vs_stored']):>16}"
          f"{fmt(r.get('B_vs_A','-')):>9}{fmt(r.get('C_vs_A','-')):>9}")
def mean(k):
    vs=[r[k] for r in rows if isinstance(r[k], float)]
    return sum(vs)/len(vs) if vs else float('nan')
print(f"\nmean n={len(rows)}:  A={mean('A_daemon_vs_stored'):.4f}  B={mean('B_bare_raw_vs_stored'):.4f}  "
      f"C={mean('C_bare_eos_vs_stored'):.4f}  D={mean('D_bare_query_vs_stored'):.4f}   threshold=0.92")
