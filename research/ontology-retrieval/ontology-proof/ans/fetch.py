#!/usr/bin/env python3
"""Polite cached fetch: fetch.py URL [URL...]. One request/second per process, every
response cached under raw/ by URL, never re-fetched, every request logged to
raw/requests.log. Generic User-Agent; no identity in any header."""
import hashlib, pathlib, re, sys, time, urllib.request, urllib.error

RAW = pathlib.Path(__file__).resolve().parent / "raw"
UA = "curl/8"


def path_for(url):
    name = re.sub(r"[^A-Za-z0-9._-]+", "_", url.split("://", 1)[1])[:150]
    return RAW / f"{name}.{hashlib.sha1(url.encode()).hexdigest()[:8]}"


def get(url, accept=None):
    p = path_for(url)
    if p.exists():
        return p.read_bytes()
    miss = p.with_suffix(p.suffix + ".miss")
    if miss.exists():
        return None
    RAW.mkdir(exist_ok=True)
    time.sleep(1.0)
    req = urllib.request.Request(url, headers={"User-Agent": UA, **({"Accept": accept} if accept else {})})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            body, code = r.read(), r.status
    except urllib.error.HTTPError as e:
        body, code = None, e.code
    with open(RAW / "requests.log", "a") as log:
        log.write(f"{code}\t{url}\n")
    if body is None:
        miss.write_text(str(code))
        return None
    p.write_bytes(body)
    return body


if __name__ == "__main__":
    for u in sys.argv[1:]:
        b = get(u)
        print(u, "->", "MISS" if b is None else f"{len(b)} bytes {path_for(u).name}")
