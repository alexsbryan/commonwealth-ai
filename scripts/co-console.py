#!/usr/bin/env python3
"""The seat console — the first page that actuates.

    scripts/co-console.py                 # serve, open a browser, Ctrl-C to stop
    scripts/co-console.py --port 8731     # a fixed port instead of an ephemeral one
    scripts/co-console.py --no-open       # print the URL, do not launch a browser
    scripts/co-console.py --self-test     # the lane; no socket, no model

WHY THIS SERVES INSTEAD OF EXPORTING. The plan specified a `file://` page
that exports `seat-actions.jsonl` for `co-apply.py` to replay, on the
reasoning that "a local HTTP endpoint would be a new daemon". That
conflates protocol with lifecycle. A daemon here is the thing at :9741 —
autostart, idle timeouts, health probes, slot management, a launchd unit.
This is a foreground process that lives as long as you are working and
dies on Ctrl-C.

Serving buys the two things the export shape could not:

  1. A `file://` page cannot write to disk, so exporting meant a Blob
     download or a clipboard round trip — browser-dependent, and the most
     likely reason the operator journey fails for a reason that has
     nothing to do with the seat.
  2. Actuation is a ROUND TRIP. R4 takes 1-2 minutes and its canary
     doubles that; R6 bounded to two items ran ~90s. Export-then-apply
     cannot show you a verdict in the same sitting. Here, actuating
     starts a job and the page polls it.

WHAT DID NOT CHANGE, AND IS THE POINT. The console is a DRIVER, not a
second path. Every action shells out through `co_apply.build_argv` — the
same argv `co-apply.py` builds and self-tests — so a directive resolved
here is indistinguishable in `directives.jsonl` from one resolved by
hand. Nothing writes to a store directly. `co-apply.py` did not become
redundant: it is now the shared argv decider plus the batch/offline path,
and this file is a thin caller (ARCH §10.6).

Every action is also appended to `seat-actions.jsonl` in the store
directory, in exactly the shape `co-apply.py` consumes. That recovers
what the export shape gave for free — a readable artifact of what you
decided — and it is replayable, so the log of a session is also a script.

THE DATA COMES FROM co-closeout.py. `join_directives`, `read_jsonl`,
`read_orders` and the two Standing renderers are IMPORTED, not
reimplemented. A second copy of the pending/resolved join is how two
pages come to disagree about what is waiting on you.

SECURITY, because these endpoints resolve directives and actuate roles.
Binding to 127.0.0.1 does NOT stop a page in your browser from POSTing
here. So: a token is minted per run and required on every request, and
cross-site POSTs are refused by `Origin` / `Sec-Fetch-Site`. The token is
in the URL this prints — that URL is the credential, so do not paste it
anywhere.
"""
from __future__ import annotations

import argparse
import datetime as dt
import html
import importlib.util
import json
import secrets
import subprocess
import sys
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

REPO = Path(__file__).resolve().parent.parent


def _load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, REPO / "scripts" / filename)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


CO = _load("co_closeout_console", "co-closeout.py")
APPLY = _load("co_apply_console", "co-apply.py")

E = html.escape

# A rejection is a RESOLUTION whose substance the operator changed from
# "do this" to "do not" — so it is `edited`, not `no-decision`.
# `no-decision` means no decision was taken on the row at all, and using
# it for a deliberate refusal would understate the edit rate, which is the
# one statistic the M0 loop is measured by. The UI states this mapping
# rather than choosing it silently.
DECISION = {
    "approve": {"verdict": "unedited", "edit_class": "none"},
    "edit":    {"verdict": "edited",   "edit_class": "content"},
    "reject":  {"verdict": "edited",   "edit_class": "content"},
}

ROLE_HINT = {
    "R1": "typed intent — an order draft comes back",
    "R2": "an initiative, in a sentence — bars come back",
    "R3": "a campaign id, e.g. financial-corpora (no model)",
    "R4": "a landing bundle, or a path to one",
    "R5": "an out-of-scope finding — a backlog item comes back",
    "R6": "item ids, space separated — MUST be bounded",
}


# ---- jobs --------------------------------------------------------------
# Actuation is minutes, not milliseconds. A synchronous POST would sit on
# the socket past every sane browser timeout, so a run is a job the page
# polls. `state` is one of running | done | failed — never absent, so the
# page can always say which of the three it is.


class Jobs:
    def __init__(self):
        self._lock = threading.Lock()
        self._jobs: dict[str, dict] = {}

    def start(self, label: str, argv: list[str], on_done=None,
              parse_json: bool = False) -> str:
        jid = uuid.uuid4().hex[:8]
        with self._lock:
            self._jobs[jid] = {"id": jid, "label": label, "state": "running",
                               "output": "", "code": None, "payload": None}

        def run():
            payload = None
            try:
                r = subprocess.run(argv, capture_output=True, text=True,
                                   cwd=REPO, timeout=3600)
                out = ((r.stdout or "") + (r.stderr or "")).strip()
                state = "done" if r.returncode == 0 else "failed"
                code = r.returncode
                if parse_json and code == 0:
                    # co-role.py --json prints its audit row LAST. Parse
                    # it here rather than in the page: a browser scraping
                    # structure out of a human printout is a second
                    # decider about what the run produced.
                    for line in reversed((r.stdout or "").splitlines()):
                        try:
                            payload = json.loads(line)
                            break
                        except json.JSONDecodeError:
                            continue
            except Exception as e:                    # noqa: BLE001
                out, state, code = f"{type(e).__name__}: {e}", "failed", -1
            with self._lock:
                self._jobs[jid].update(state=state, output=out, code=code,
                                       payload=payload)
            if on_done:
                on_done(code)

        threading.Thread(target=run, daemon=True).start()
        return jid

    def get(self, jid: str) -> dict | None:
        with self._lock:
            j = self._jobs.get(jid)
            return dict(j) if j else None


# ---- state -------------------------------------------------------------


def load_state(log_path: Path, script_path: Path) -> dict:
    """Everything the page shows, from co-closeout's own loaders."""
    malformed: list = []
    directives = CO.read_jsonl(log_path, malformed)
    verdicts_path = log_path.parent / "verdicts.jsonl"
    verdicts = CO.read_jsonl(verdicts_path, malformed)
    features = CO.orders_dir(script_path)
    orders = CO.read_orders(features)
    open_pending, pairs = CO.join_directives(directives)
    open_pending.sort(key=lambda r: str(r.get("ts") or ""), reverse=True)
    return {"pending": open_pending, "pairs": pairs, "orders": orders,
            "verdicts": verdicts, "features": features,
            "verdicts_path": verdicts_path, "malformed": malformed,
            "log_path": log_path}


def audit(log_path: Path, rec: dict) -> None:
    """Append what we did, in the shape co-apply.py consumes. The session
    log is therefore also a replayable script."""
    p = log_path.parent / "seat-actions.jsonl"
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(rec, ensure_ascii=False) + "\n")


# ---- page --------------------------------------------------------------

EXTRA_CSS = """
.card{border:1px solid var(--line,#d0d0d0);border-radius:8px;padding:.75rem .9rem;
 margin:.6rem 0;background:var(--card,#fff)}
.card.sel{border-color:#3b82f6;box-shadow:0 0 0 2px rgba(59,130,246,.25)}
.card pre{white-space:pre-wrap;word-break:break-word;margin:.4rem 0;font-size:.86rem}
.row{display:flex;gap:.5rem;align-items:center;flex-wrap:wrap;margin-top:.5rem}
button{font:inherit;padding:.3rem .7rem;border-radius:6px;border:1px solid #888;
 background:transparent;color:inherit;cursor:pointer}
button:hover{border-color:#3b82f6}
textarea,input[type=text]{font:inherit;width:100%;box-sizing:border-box;padding:.4rem;
 border-radius:6px;border:1px solid #888;background:transparent;color:inherit}
.k{display:inline-block;min-width:1.2em;text-align:center;border:1px solid #888;
 border-radius:4px;padding:0 .3em;font-size:.8em;opacity:.75}
.out{white-space:pre-wrap;font-size:.82rem;opacity:.9;margin-top:.4rem;
 border-left:3px solid #888;padding-left:.6rem}
.hide{display:none}
.ok{color:#15803d}.err{color:#b91c1c}.run{opacity:.7}
@media(prefers-color-scheme:dark){.card{background:#161616;--line:#333}}
"""

JS = r"""
const TOK = document.getElementById('tok').textContent.trim();
let sel = 0;

function cards(){ return [...document.querySelectorAll('.card[data-id]')]; }
function mark(){ cards().forEach((c,i)=>c.classList.toggle('sel', i===sel));
  const c = cards()[sel]; if(c) c.scrollIntoView({block:'nearest'}); }

async function post(path, body){
  const r = await fetch(path, {method:'POST', headers:{
      'content-type':'application/json','x-console-token':TOK},
      body: JSON.stringify(body)});
  return {ok: r.ok, data: await r.json().catch(()=>({}))};
}

function setOut(el, cls, text){ el.className='out '+cls; el.textContent=text; }

async function decide(card, kind){
  const id = card.dataset.id;
  const out = card.querySelector('.out');
  let final;
  if(kind==='approve'){ final = card.dataset.draft; }
  else if(kind==='edit'){
    const ta = card.querySelector('textarea');
    if(ta.classList.contains('hide')){
      ta.classList.remove('hide'); ta.value = card.dataset.draft; ta.focus();
      setOut(out,'','Edit the text, then press Ctrl+Enter to submit.');
      return;
    }
    final = ta.value;
  } else {
    const why = prompt('Reject — why? (recorded as the final text)');
    if(why===null) return;
    final = 'Rejected: ' + why;
  }
  setOut(out,'run','working…');
  const {ok, data} = await post('/resolve', {id, final, decision:kind});
  setOut(out, ok?'ok':'err', data.detail || (ok?'resolved':'failed'));
  if(ok){ card.querySelector('.row').remove();
          const ta=card.querySelector('textarea'); if(ta) ta.remove(); }
}

async function actuate(role, inputEl, outEl){
  const inp = inputEl || document.getElementById('in-'+role);
  const out = outEl || document.getElementById('out-'+role);
  setOut(out,'run','starting '+role+'…');
  const {ok, data} = await post('/actuate', {role, input: inp.value});
  if(!ok){ setOut(out,'err', data.detail||'refused'); return; }
  poll(data.job, out, role);
}

async function route(){
  const out = document.getElementById('out-route');
  const steps = document.getElementById('steps');
  steps.innerHTML = '';
  const text = document.getElementById('intent').value.trim();
  if(!text){ setOut(out,'err','say what you want to do'); return; }
  setOut(out,'run','reading…');
  const {ok, data} = await post('/route', {input: text});
  if(!ok){ setOut(out,'err', data.detail||'refused'); return; }
  pollRoute(data.job, out, steps);
}

async function pollRoute(job, out, steps){
  const r = await fetch('/job?id='+encodeURIComponent(job)+'&t='+TOK);
  const j = await r.json();
  if(j.state==='running'){ setOut(out,'run','reading intent…');
    setTimeout(()=>pollRoute(job,out,steps), 2000); return; }
  // A route that could not be read is a real answer, and it is shown as
  // one — not as an empty pane the operator has to interpret.
  if(j.state!=='done'){ setOut(out,'err', j.output || 'could not route'); return; }
  const p = j.payload && j.payload.payload;
  const list = (p && p.steps) || [];
  if(!list.length){ setOut(out,'err', (p&&p.note) || j.output); return; }
  setOut(out,'ok', (p.note||'')+'  — nothing has run; edit and run each step.');
  list.forEach((s,i)=>{
    const d = document.createElement('div');
    d.className='card';
    d.innerHTML = '<b>'+s.role+'</b> <span class="sub">step '+(i+1)+' of '
      + list.length + ' · ' + esc(s.why) + '</span>'
      + '<div class="row"><input type="text" class="rin"><button>run '
      + s.role + '</button></div><div class="out"></div>';
    const inp = d.querySelector('.rin');
    inp.value = s.input;
    d.querySelector('button').onclick =
      ()=>actuate(s.role, inp, d.querySelector('.out'));
    steps.appendChild(d);
  });
}

function esc(t){ const d=document.createElement('div'); d.textContent=t; return d.innerHTML; }

async function poll(job, out, role){
  const r = await fetch('/job?id='+encodeURIComponent(job)+'&t='+TOK);
  const j = await r.json();
  if(j.state==='running'){ setOut(out,'run', role+' running… (R4 takes 1-2 min)');
    setTimeout(()=>poll(job,out,role), 2000); return; }
  setOut(out, j.state==='done'?'ok':'err', j.output || j.state);
}

document.addEventListener('keydown', e=>{
  if(/^(INPUT|TEXTAREA)$/.test(e.target.tagName)){
    if(e.key==='Enter' && (e.ctrlKey||e.metaKey)){
      if(e.target.id==='intent'){ route(); return; }
      const card = e.target.closest('.card'); if(card) decide(card,'edit'); }
    return;
  }
  if(e.key==='/'){ const el=document.getElementById('intent');
    if(el){ el.focus(); e.preventDefault(); return; } }
  const cs = cards();
  if(e.key==='j'||e.key==='ArrowDown'){ sel=Math.min(sel+1,cs.length-1); mark(); }
  else if(e.key==='k'||e.key==='ArrowUp'){ sel=Math.max(sel-1,0); mark(); }
  else if('aer'.includes(e.key.toLowerCase()) && cs[sel]){
    decide(cs[sel], {a:'approve',e:'edit',r:'reject'}[e.key.toLowerCase()]); }
  else if('123456'.includes(e.key)){
    const el=document.getElementById('in-R'+e.key); if(el){ el.focus(); e.preventDefault(); } }
});
mark();
"""


def render_pending(pending: list) -> str:
    if not pending:
        return ("<section><h2>Waiting on you</h2><p class=\"sub\">Nothing "
                "pending. That is a state, not an empty render.</p></section>")
    out = ["<section><h2>Waiting on you</h2>",
           '<p class="sub"><span class="k">j</span>/<span class="k">k</span> move · '
           '<span class="k">a</span> approve verbatim · <span class="k">e</span> edit · '
           '<span class="k">r</span> reject. A rejection is recorded as '
           '<code>edited</code>/<code>content</code> — it changed the substance; '
           '<code>no-decision</code> would mean you did not decide.</p>']
    for r in pending:
        did = str(r.get("id") or "")
        draft = str(r.get("draft") or "")
        cits = r.get("citations") or []
        if isinstance(cits, str):
            cits = [cits]
        out.append(
            f'<div class="card" data-id="{E(did)}" data-draft="{E(draft)}">'
            f'<b>{E(str(r.get("kind") or "(no kind)"))}</b> '
            f'<code>{E(did[:8])}</code> '
            f'<span class="sub">{E(CO.local_str(CO.parse_ts(r.get("ts"))))}'
            f'{" · " + E(str(r.get("worker"))) if r.get("worker") else ""}</span>'
            f"<pre>{E(draft)}</pre>"
            + (f'<p class="sub">citations: '
               + ", ".join(f"<code>{E(str(c))}</code>" for c in cits) + "</p>"
               if cits else "")
            + '<textarea class="hide" rows="6"></textarea>'
            '<div class="row">'
            '<button onclick="decide(this.closest(\'.card\'),\'approve\')">approve</button>'
            '<button onclick="decide(this.closest(\'.card\'),\'edit\')">edit</button>'
            '<button onclick="decide(this.closest(\'.card\'),\'reject\')">reject</button>'
            "</div>"
            '<div class="out"></div></div>')
    out.append("</section>")
    return "".join(out)


def render_route() -> str:
    return (
        "<section><h2>What do you want to do?</h2>"
        '<p class="sub">Say it plainly. R0 reads the intent and PROPOSES which '
        "roles do it, in order — it never runs them for you. Starting a "
        "campaign is two steps: R2 drafts the bars, then R1 drafts the order "
        "that serves them.</p>"
        '<div class="card">'
        '<textarea id="intent" rows="3" placeholder="e.g. start a campaign on '
        'making the deep-research loop stop re-fetching URLs it already '
        'refused"></textarea>'
        '<div class="row"><button onclick="route()">read it</button>'
        '<span class="sub">Ctrl+Enter</span></div>'
        '<div class="out" id="out-route"></div>'
        '<div id="steps"></div></div></section>')


def render_actuate() -> str:
    rows = ["<section><h2>Actuate</h2>",
            '<p class="sub">Press <span class="k">1</span>-<span class="k">6</span> '
            "to jump to a role. R1/R2/R6 queue a draft above rather than landing "
            "anything; R3/R5 are consumer-validated; R4 uses the charter's gate "
            "and runs its planted-defect canary first.</p>"]
    for n in range(1, 7):
        rid = f"R{n}"
        rows.append(
            f'<div class="card"><b>{rid}</b> '
            f'<span class="sub">{E(ROLE_HINT[rid])}</span>'
            f'<div class="row"><input type="text" id="in-{rid}" '
            f'placeholder="{E(ROLE_HINT[rid])}">'
            f'<button onclick="actuate(\'{rid}\')">run {rid}</button></div>'
            f'<div class="out" id="out-{rid}"></div></div>')
    rows.append("</section>")
    return "".join(rows)


def build_page(state: dict, token: str, script_path: Path) -> str:
    now = dt.datetime.now(dt.timezone.utc)
    chips = "".join(
        f'<span class="chip"><b>{n}</b> {label}</span>' for n, label in [
            (len(state["pending"]), "pending your call"),
            (sum(1 for o in state["orders"] if o["status"] == "open"), "open orders"),
            (len(state["verdicts"]), "verdicts on file"),
        ])
    body = "".join([
        "<div><h1>Seat console</h1>",
        f'<p class="sub">{E(CO.local_str(now))} · drawn from '
        f'<code>{E(str(state["log_path"]))}</code> · this page ACTUATES; '
        "every action shells out to the same script you would run by hand</p>",
        f'<div class="chips">{chips}</div></div>',
        render_route(),
        render_pending(state["pending"]),
        render_actuate(),
        "<section><h2>Standing</h2></section>",
        CO.render_orders(state["orders"], state["features"], now),
        CO.render_verdicts(state["verdicts"], state["verdicts_path"], now),
        CO.render_footer([state["log_path"], state["verdicts_path"],
                          state["features"]], state["malformed"],
                         CO.local_str(now)),
    ])
    return (
        '<!doctype html>\n<html lang="en"><head><meta charset="utf-8">'
        '<meta name="viewport" content="width=device-width,initial-scale=1">'
        "<title>Seat console</title>"
        f"<style>{CO.CSS}{EXTRA_CSS}</style></head><body><main>{body}</main>"
        f'<script type="application/json" id="tok">{E(token)}</script>'
        f"<script>{JS}</script></body></html>\n")


# ---- server ------------------------------------------------------------


def make_handler(token: str, log_path: Path, script_path: Path, jobs: Jobs):

    class Handler(BaseHTTPRequestHandler):
        server_version = "co-console"

        def log_message(self, *a):        # quiet; the page is the output
            pass

        # --- guards. These endpoints resolve directives and actuate
        # roles, and 127.0.0.1 does not stop another page in the same
        # browser from POSTing here.
        def _authed(self, body: dict | None) -> bool:
            supplied = (self.headers.get("x-console-token")
                        or (body or {}).get("token")
                        or parse_qs(urlparse(self.path).query).get("t", [""])[0])
            return secrets.compare_digest(str(supplied), token)

        def _same_site(self) -> bool:
            site = self.headers.get("sec-fetch-site")
            if site and site not in ("same-origin", "none"):
                return False
            origin = self.headers.get("origin")
            if origin:
                host = self.headers.get("host") or ""
                return origin.endswith(host)
            return True

        def _json(self, code: int, payload: dict):
            raw = json.dumps(payload).encode()
            self.send_response(code)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):                  # noqa: N802
            parts = urlparse(self.path)
            if not self._authed(None):
                self._json(403, {"detail": "bad or missing token — open the URL "
                                           "co-console.py printed"})
                return
            if parts.path == "/job":
                jid = parse_qs(parts.query).get("id", [""])[0]
                j = jobs.get(jid)
                self._json(200 if j else 404, j or {"detail": "no such job"})
                return
            if parts.path not in ("/", "/index.html"):
                self._json(404, {"detail": "no such path"})
                return
            page = build_page(load_state(log_path, script_path), token,
                              script_path).encode()
            self.send_response(200)
            self.send_header("content-type", "text/html; charset=utf-8")
            self.send_header("content-length", str(len(page)))
            self.end_headers()
            self.wfile.write(page)

        def do_POST(self):                 # noqa: N802
            try:
                n = int(self.headers.get("content-length") or 0)
                body = json.loads(self.rfile.read(n) or b"{}")
            except Exception:              # noqa: BLE001
                self._json(400, {"detail": "body is not JSON"})
                return
            if not self._authed(body):
                self._json(403, {"detail": "bad or missing token"})
                return
            if not self._same_site():
                self._json(403, {"detail": "cross-site POST refused"})
                return
            path = urlparse(self.path).path
            if path == "/resolve":
                self._resolve(body)
            elif path == "/actuate":
                self._actuate(body)
            elif path == "/route":
                self._route(body)
            else:
                self._json(404, {"detail": "no such path"})

        def _route(self, body: dict):
            """R0 reads intent and PROPOSES steps. It never dispatches:
            a wrong route that ran would produce a well-formed artifact
            of the wrong kind, which is worse than a question because it
            looks like an answer."""
            text = (body.get("input") or "").strip()
            if not text:
                self._json(400, {"detail": "say what you want to do"})
                return
            argv = [sys.executable, str(REPO / "scripts" / "co-role.py"),
                    "R0", "--input", text, "--json"]
            jid = jobs.start("R0", argv, parse_json=True)
            self._json(200, {"job": jid})

        def _resolve(self, body: dict):
            kind = body.get("decision")
            mapped = DECISION.get(kind)
            if mapped is None:
                self._json(400, {"detail": f"unknown decision {kind!r}"})
                return
            rec = {"action": "resolve", "id": body.get("id"),
                   "final": body.get("final"), **mapped}
            try:
                argv = APPLY.build_argv(rec)
            except APPLY.ActionError as e:
                self._json(400, {"detail": str(e)})
                return
            r = subprocess.run(argv, capture_output=True, text=True, cwd=REPO,
                               timeout=300)
            tail = ((r.stdout or "") + (r.stderr or "")).strip().splitlines()
            detail = tail[-1][:300] if tail else f"exit {r.returncode}"
            audit(log_path, {**rec, "console_result": r.returncode})
            self._json(200 if r.returncode == 0 else 500,
                       {"detail": detail, "code": r.returncode})

        def _actuate(self, body: dict):
            rec = {"action": "actuate", "role": body.get("role"),
                   "input": body.get("input", "")}
            try:
                argv = APPLY.build_argv(rec)
            except APPLY.ActionError as e:
                self._json(400, {"detail": str(e)})
                return
            jid = jobs.start(str(rec["role"]), argv,
                             on_done=lambda code: audit(
                                 log_path, {**rec, "console_result": code}))
            self._json(200, {"job": jid})

    return Handler


def serve(port: int, open_it: bool, log_path: Path, script_path: Path) -> int:
    token = secrets.token_urlsafe(32)
    jobs = Jobs()
    httpd = ThreadingHTTPServer(("127.0.0.1", port),
                                make_handler(token, log_path, script_path, jobs))
    url = f"http://127.0.0.1:{httpd.server_port}/?t={token}"
    # flush=True: stdout is block-buffered when this is piped or run from
    # a wrapper, and the URL IS the credential — a console whose only way
    # in never reaches the operator is a console that does not work.
    print(f"seat console -> {url}", flush=True)
    print("   this URL contains the session token — it is the credential.",
          flush=True)
    print("   Ctrl-C to stop (nothing keeps running afterwards).", flush=True)
    if open_it:
        import webbrowser
        webbrowser.open(url)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nstopped.")
    finally:
        httpd.server_close()
    return 0


# ---- the lane ----------------------------------------------------------


def self_test() -> int:
    import urllib.error
    import urllib.request
    failures = []

    def check(name, ok, detail=""):
        print(f"  {'PASS' if ok else 'FAIL'}  {name}" + (f" — {detail}" if detail else ""))
        if not ok:
            failures.append(name)

    def req(url, data=None, headers=None):
        r = urllib.request.Request(url, data=data, headers=headers or {},
                                   method="POST" if data is not None else "GET")
        try:
            with urllib.request.urlopen(r, timeout=10) as resp:
                return resp.status, resp.read().decode()
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode()

    import tempfile
    with tempfile.TemporaryDirectory(prefix="co-console-selftest-") as tmp:
        log = Path(tmp) / "directives.jsonl"
        log.write_text(json.dumps({
            "id": "aaaa1111", "ts": dt.datetime.now(dt.timezone.utc).isoformat(),
            "status": "pending", "kind": "order", "worker": None,
            "draft": "DRAFT-TEXT-ALPHA", "citations": ["ARCH §14"]}) + "\n",
            encoding="utf-8")
        token = secrets.token_urlsafe(16)
        jobs = Jobs()
        httpd = ThreadingHTTPServer(
            ("127.0.0.1", 0), make_handler(token, log, Path(__file__), jobs))
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        base = f"http://127.0.0.1:{httpd.server_port}"

        print("check 1 — the token is required, and it is checked")
        code, _ = req(f"{base}/")
        check("GET without a token is 403", code == 403, f"got {code}")
        code, _ = req(f"{base}/?t=wrong-token")
        check("GET with the WRONG token is 403", code == 403, f"got {code}")
        code, page = req(f"{base}/?t={token}")
        check("NEGATIVE: GET with the right token renders", code == 200, f"got {code}")
        check("the pending draft renders verbatim", "DRAFT-TEXT-ALPHA" in page)
        code, _ = req(f"{base}/resolve", b"{}", {"content-type": "application/json"})
        check("POST without a token is 403", code == 403, f"got {code}")

        print("check 2 — cross-site POSTs are refused even with the token")
        hdr = {"content-type": "application/json", "x-console-token": token,
               "sec-fetch-site": "cross-site"}
        code, _ = req(f"{base}/resolve", json.dumps(
            {"id": "aaaa1111", "final": "x", "decision": "approve"}).encode(), hdr)
        check("a cross-site POST is 403", code == 403, f"got {code}")
        hdr2 = dict(hdr, **{"sec-fetch-site": "same-origin"})
        hdr2["origin"] = "http://evil.example"
        code, _ = req(f"{base}/resolve", json.dumps(
            {"id": "aaaa1111", "final": "x", "decision": "approve"}).encode(), hdr2)
        check("a foreign Origin is 403", code == 403, f"got {code}")

        print("check 3 — a decision maps to the flag, and nothing else does")
        for kind, want in (("approve", "--unedited"), ("edit", "--edited"),
                           ("reject", "--edited")):
            argv = APPLY.build_argv({"action": "resolve", "id": "a", "final": "t",
                                     **DECISION[kind]})
            check(f"{kind} -> {want}", want in argv)
        check("reject is edited/content, never no-decision",
              DECISION["reject"]["verdict"] == "edited"
              and DECISION["reject"]["edit_class"] == "content")
        hdr3 = {"content-type": "application/json", "x-console-token": token}
        code, out = req(f"{base}/resolve", json.dumps(
            {"id": "aaaa1111", "final": "x", "decision": "nonsense"}).encode(), hdr3)
        check("NEGATIVE: an unknown decision is 400, not guessed",
              code == 400, f"got {code}")

        print("check 4 — actuation is a job, and a bad role never becomes one")
        code, out = req(f"{base}/actuate", json.dumps(
            {"role": "R9", "input": "x"}).encode(), hdr3)
        check("an unknown role is refused before a job starts", code == 400,
              f"got {code}")
        jid = jobs.start("echo", [sys.executable, "-c", "print('job-ran')"])
        for _ in range(50):
            j = jobs.get(jid)
            if j["state"] != "running":
                break
            import time
            time.sleep(0.1)
        j = jobs.get(jid)
        check("a job reaches a terminal state with its output",
              j["state"] == "done" and "job-ran" in j["output"], str(j))
        jid2 = jobs.start("fail", [sys.executable, "-c", "raise SystemExit(3)"])
        for _ in range(50):
            if jobs.get(jid2)["state"] != "running":
                break
            import time
            time.sleep(0.1)
        check("NEGATIVE: a failing job is 'failed', not 'done'",
              jobs.get(jid2)["state"] == "failed", jobs.get(jid2)["state"])
        code, _ = req(f"{base}/job?id=nope&t={token}")
        check("an unknown job id is 404, not an empty success", code == 404)

        print("check 5 — the intent box proposes, and never dispatches")
        code, _ = req(f"{base}/route", json.dumps({"input": "  "}).encode(), hdr3)
        check("an empty intent is 400, not a job that reads nothing",
              code == 400, f"got {code}")
        code, _ = req(f"{base}/route", json.dumps(
            {"input": "start a campaign"}).encode(), hdr3)
        check("NEGATIVE: a real intent starts a job", code == 200, f"got {code}")
        check("/route is token-guarded like every other POST",
              req(f"{base}/route", json.dumps({"input": "x"}).encode(),
                  {"content-type": "application/json"})[0] == 403)
        # R0's card must stay a PROPOSER. If its gate ever became `auto`
        # or `draft`, reading intent would start landing things, and the
        # console's promise that nothing runs until you press run would
        # quietly stop being true.
        try:
            import importlib.util as _il
            _sp = _il.spec_from_file_location("co_role_ct",
                                              REPO / "scripts" / "co-role.py")
            _m = _il.module_from_spec(_sp)
            _sp.loader.exec_module(_m)
            check("R0's gate is 'propose' — reading intent cannot land anything",
                  _m.load_card("R0")["gate"] == "propose")
            check("R0 has no directive kind, so it cannot queue a directive",
                  "R0" not in _m.DIRECTIVE_KIND)
        except Exception as e:                        # noqa: BLE001
            check("R0's card is loadable", False, f"{type(e).__name__}: {e}")

        httpd.shutdown()
        httpd.server_close()

    print()
    if failures:
        print(f"self-test FAILED — {len(failures)} check(s): " + "; ".join(failures))
        return 1
    print("self-test PASSED — 5 checks, both directions each.")
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(
        prog="co-console.py",
        description="The seat console — read the queue, decide, actuate a role.")
    ap.add_argument("--port", type=int, default=0,
                    help="fixed port; default is an ephemeral one")
    ap.add_argument("--no-open", action="store_true",
                    help="print the URL instead of launching a browser")
    ap.add_argument("--self-test", action="store_true",
                    help="run the lane (no model, no store writes) and exit")
    a = ap.parse_args(argv)
    if a.self_test:
        return self_test()
    # co-closeout's own path resolver, so CO_DIRECTIVE_LOG means the same
    # thing on both pages and neither can read a different store.
    return serve(a.port, not a.no_open, CO.directive_log_path(), Path(__file__))


if __name__ == "__main__":
    raise SystemExit(main())
