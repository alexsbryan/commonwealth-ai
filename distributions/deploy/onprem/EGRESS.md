# EGRESS.md — every outbound connection this system can make

**Audience:** the firm's security reviewer.
**Method:** a line-by-line audit of the source, not a claim about intent.
Every row cites the file that makes the call. Where a claim in an earlier
version of this document turned out to be wrong, the correction is stated
in place rather than quietly removed.

**Scope:** the one process that runs on the box — `sovereign-onprem`, the
on-prem distribution (`distributions/crates/sovereign-onprem`). It composes
the answer service (svrn), model serving (serve) and document ingest, and
nothing else. Code that is linked into the binary but never started by
this composition is listed separately, because "it is in the binary" and
"it can run" are different facts and the reviewer is entitled to both.

(Until 2026-10 the kit ran two processes, `svrn daemon run` and
`sovereign-server --no-default-features`. The second is gone; its
section of this document went with it.)

---

## Bottom line

With the shipped configuration, the process makes **no outbound
connection off the box**. The surfaces that could reach the open internet
on an ordinary question are not composed in the binary at all — the
withholding is the composition, not a setting. Two IT-only admin routes
can still start a download (§3); a lawyer's key cannot reach them and
nginx does not proxy them.

There is **no telemetry**, **no update check**, and **no HuggingFace
reachability probe** in the boot path.

**Defence in depth.** The composition is the first control and the
configuration the second, but neither is enforced by the kernel. The
systemd unit therefore carries `IPAddressDeny=any` with an allowlist of
loopback only. If any of the analysis below is wrong, that is what holds
— and the denial appears in the audit log rather than as silent traffic.

---

## 1. Not composed in this binary

These were the real finding of the original audit: agent tools that
reached the open internet on **any chat turn**, with no config key, no
env var and no tool allowlist governing them. `Permission::Network` is
not a control for them: it is consulted at one call site, the plan
executor, and the chat path calls `tool.execute()` directly.

| Tool | Reaches | How this binary withholds it |
|---|---|---|
| `search` (web fallback) | `html.duckduckgo.com`, then `www.google.com`, then `lite.duckduckgo.com`, whenever the top LOCAL retrieval score is thin | `Posture::Sealed` (sovereign-onprem `main.rs`) withholds web reach: `search` is built over the installed corpora only, and the boot log reports `withheld search:web-fallback` |
| `web_fetch` | any URL the model emits; scheme-only validation | the web bundle is withheld by the same posture and never registered |
| `wikipedia_fetch` | `en.wikipedia.org` | the wikipedia bundle is withheld by the same posture |
| `probe_url` | any URL a recipe author names | not linked: the binary has no sovereign-recipe-author edge (ingest composes without recipe authoring) |

The type behind `web_fetch` (sovereign-tools `WebFetchTool`) is still
linked, through sovereign-daemon; what the binary withholds is the
REGISTRATION. That is why the proof is runtime, not a `strings` scan:
`GET /v1/tools` lists what the turn runtime holds, and acceptance.sh
check 0c fails if any of these is there. The binary's own test
(`sovereign-onprem/tests/sealed_composition_e2e.rs`) boots it with every
proxy variable pointed at a connection counter and asserts that a turn
dials nothing.

---

## 2. Switched off by configuration

Live by default in the daemon, off in the shipped
`daemon-config.toml`. The config comments explain each in place.

| Destination | Trigger | Key that stops it | Default |
|---|---|---|---|
| `en.wikipedia.org/w/api.php` (MediaWiki freshness poller) | daemon startup, after 0-15 min jitter, then every 24 h | `[daemon] freshness_watchers_enabled = false` (the boot log says `freshness watchers skipped`) | **true** |
| Operator-declared MCP servers | daemon startup: each `[[mcp_servers]]` entry is loaded into the tool registry | leave `[[mcp_servers]]` out | empty |

The poller's `recentchange` SSE stream is compiled-in dead code: its
`spawn()` has no caller. Nothing dials `stream.wikimedia.org`.

---

## 3. IT-only routes that can reach the network

Reachable only with IT's key (group `admin`), from the box itself: the
daemon refuses them to a lawyer's key (`sovereign-daemon api_keys.rs`,
`KEY_SCOPE`), each also requires a loopback peer, and nginx proxies none
of them (`nginx/firm-rag.conf`).

| Route | Reaches | Where |
|---|---|---|
| `POST /v1/admin/assets/download` | `huggingface.co` — a GGUF or the GLiNER model, fetched into the serving root as a job | sovereign-daemon `assets_http.rs` forwards it to serve; sovereign-compute `assets.rs` runs `setup_planner::download_gguf` / `gliner_ner::download_model` |
| ingest's recipe routes (`/internal/corpus/recipes/*`) | whatever URLs a recipe's sources name (bulk corpora: `dumps.wikimedia.org`, `www.govinfo.gov`, …) | sovereign-daemon `recipe_http.rs` |

Nothing in the install or the runbook calls either. `IPAddressDeny=any`
refuses the connection if one is ever made.

---

## 4. Loopback only

These calls stay on 127.0.0.1. They are listed because a reviewer
watching `ss -tnp` will see them.

| Call | Target | Where |
|---|---|---|
| cw-rails presence poll, work-atlas store, ring rail, model-slot registration | `[daemon] rails_base`, `http://127.0.0.1:9747` in the shipped config. **Nothing listens there** — this box runs no cw-rails — so each is refused on loopback and logged as an absence | sovereign-daemon `daemon.rs` (`media_presence::run`, `RailsKv::new`, `RailsRingRail::new`); the foreground-yield post (`foreground_post.rs`) sends nothing while the window is 0 |
| hosted serve | `127.0.0.1:9748` (`SOVEREIGN_SERVE_PORT`) | serve runs in this process and binds loopback; the daemon forwards admin reads to it |
| OCR cleanup | the daemon's own client port | the OCR context calls the daemon's chat route to clean recognised text. On a daemon holding API keys this call carries no key and is refused (401): the text is indexed RAW, marked `raw OCR (cleanup unavailable: daemon error 401)`, and acceptance check 5 reports it |

`[daemon] max_peer_inflight = 0` additionally opts this node out of peer
inference admission. With no mesh there are no peers, so the gossip,
peer-inference and model-fetch loops have no target and send nothing.

---

## 5. Linked, never started

The binary links the mesh crates through sovereign-daemon → sovereign-mesh
(commonwealth-discovery's mDNS, commonwealth-transport's iroh with its
n0 relays and DNS). The on-prem composition passes **no mesh** to
`process::run` (sovereign-onprem `main.rs`: the `mesh` argument is
`None`, and serve is hosted without `rails_mesh::join`), so none of them
is started: no multicast on `224.0.0.251:5353`, no relay, no n0 DNS.

`daemon-config.toml` still sets `[iroh] enabled = false`,
`discovery = "none"` and `[discovery] mdns = false`. Those keys are not
what keeps this box quiet any more; they are stated so that a binary that
did start a mesh would still find relays and multicast off.

**Correction to the earlier version of this document.** It listed iroh,
n0 and mDNS as live-by-default and closed by those config keys. That was
true of the two-process kit, whose daemon formed a solo mesh at boot. It
is not the mechanism here.

Also in the source and on no path this process takes:

- `svrn setup`, `svrn setup fim`, `svrn mesh fetch-ner` (HuggingFace,
  `registry.npmjs.org`): install.sh stages models from the tarball and
  writes the config by hand, so none of them runs.
- `updates.sovereign.dev` (a corpus index-manifest fetch): its only
  caller is the desktop app.
- CalDAV and SMTP (`CalendarTool`, `EmailTool`): registered nowhere.
- `github.com/.../releases`: a string that is printed, never fetched.

---

## 6. What to expect in the logs

DNS resolution happens as part of an HTTP call, never on its own — so a
box making no HTTP calls issues no DNS. If the firm's egress firewall
logs a denial from this host, it is a finding, not noise, and these are
the names to look for: `huggingface.co`, `en.wikipedia.org`,
`duckduckgo.com`, `google.com`, and any n0 relay. Each maps to a row
above; please send us the log line.

## 7. How to verify this yourself

Nothing here asks to be taken on trust:

```bash
# 1. The surfaces that could reach a shell or the web are not served
#    (also acceptance.sh check 0, at the daemon's port and through nginx)
curl -s -o /dev/null -w '%{http_code}\n' -X POST https://<host>/v1/solve/jobs   # expect 404 (nginx)
curl -s -H "Authorization: Bearer <it key>" -X POST http://127.0.0.1:9741/mcp   # expect 503, "does not serve MCP"

# 2. No web tool is held (acceptance.sh check 0c)
curl -s -H "Authorization: Bearer <firm key>" https://<host>/v1/tools | jq -r '.tools[].id' | sort
# expect: no web_fetch, wikipedia_fetch or probe_url. `search` present = corpus search.

# 3. Nothing is dialling out
ss -tnp | grep -v '127.0.0.1'    # expect only inbound :443 from clients

# 4. The kernel-level control is armed
systemctl show firm-rag -p IPAddressDeny -p IPAddressAllow
```
