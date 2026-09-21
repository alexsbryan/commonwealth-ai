"""Polite cached HTTP: <=2 req/s, counts every network request in raw/_requests.json.
UA is a fixed generic string; nothing identifying is ever sent."""
import json, os, time, hashlib, urllib.request, urllib.error

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, "raw")
COUNT = os.path.join(RAW, "_requests.json")
UA = "curl/8.7.1"
BUDGET = 400
GAP = 1.6  # api.tosdr.org 429s at ~2 req/s (observed after 6 rapid search calls)
_last = [0.0]


def _count():
    try:
        return json.load(open(COUNT))
    except Exception:
        return {"n": 0, "log": []}


def get(url, name=None, as_json=True, _retry=0):
    """Fetch url, cache under raw/<name>. Returns parsed JSON (or text)."""
    name = name or hashlib.sha1(url.encode()).hexdigest()[:16] + ".json"
    path = os.path.join(RAW, name)
    if os.path.exists(path):
        data = open(path).read()
        return json.loads(data) if as_json else data
    c = _count()
    if c["n"] >= BUDGET:
        raise SystemExit(f"request budget {BUDGET} exhausted")
    wait = GAP - (time.time() - _last[0])
    if wait > 0:
        time.sleep(wait)
    req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
    status = None
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            status = r.status
            data = r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        status = e.code
        data = e.read().decode("utf-8", "replace")
    finally:
        _last[0] = time.time()
        c["n"] += 1
        c["log"].append([url, status])
        json.dump(c, open(COUNT, "w"))
    if status == 429 and _retry < 3:
        time.sleep(45 * (_retry + 1))
        return get(url, name, as_json, _retry + 1)
    if status != 200:
        open(path + ".err", "w").write(f"{status}\n{data[:2000]}")
        raise RuntimeError(f"HTTP {status} for {url}: {data[:300]}")
    open(path, "w").write(data)
    return json.loads(data) if as_json else data
