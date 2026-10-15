"""Recording proxy: forwards every request to the daemon unchanged and saves each chat request body with its response.
    proxy.py PORT OUTDIR"""
import http.server, json, pathlib, sys, threading, urllib.request

PORT, OUT = int(sys.argv[1]), pathlib.Path(sys.argv[2])
OUT.mkdir(parents=True, exist_ok=True)
UP = "http://127.0.0.1:9741"
lock, n = threading.Lock(), [0]


class H(http.server.BaseHTTPRequestHandler):
    def _fwd(self, method):
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0)) if method == "POST" else None
        req = urllib.request.Request(UP + self.path, data=body, method=method,
                                     headers={k: v for k, v in self.headers.items() if k.lower() not in ("host", "content-length")})
        try:
            with urllib.request.urlopen(req, timeout=600) as r:
                status, data, ctype = r.status, r.read(), r.headers.get("Content-Type", "application/json")
        except urllib.error.HTTPError as e:
            status, data, ctype = e.code, e.read(), "application/json"
        if method == "POST" and "chat/completions" in self.path:
            with lock:
                n[0] += 1
                i = n[0]
            (OUT / f"{i:05d}.json").write_text(json.dumps({"request": json.loads(body), "status": status,
                                                           "response": json.loads(data or b"null")}, indent=1))
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        self._fwd("POST")

    def do_GET(self):
        self._fwd("GET")

    def log_message(self, *a):
        pass


http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
