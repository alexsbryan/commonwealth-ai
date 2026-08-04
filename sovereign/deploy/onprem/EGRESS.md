# EGRESS.md — every outbound connection this system can make

**Audience:** the firm's security reviewer.
**Method:** a line-by-line audit of the source, not a claim about intent.
Every row cites the file and line that makes the call. Where a claim in
an earlier draft of our own plan turned out to be wrong, the correction
is stated in place rather than quietly removed.

**Scope:** the two processes that run on the box — `svrn daemon run`
(the daemon) and `sovereign-server --no-default-features` (the API). Code
that ships in the tree but is not reachable from either is listed
separately at the end, because "it is in the binary" and "it can run"
are different facts and the reviewer is entitled to both.

---

## Bottom line

With the shipped configuration, the deployed processes make **no
outbound connections at all**. That required more than configuration:
three agent tools reached the open internet unconditionally on ordinary
chat turns and had no runtime switch. They are removed at compile time
by the `--no-default-features` build this kit installs.

There is **no telemetry** anywhere in the tree, **no update check**
reachable from either process, and **no HuggingFace reachability probe**
in the daemon boot path. Those three are the easy claims. The rest of
this document is the hard part.

**Defence in depth.** Config is the first control and the compile-time
feature is the second, but neither is enforced by the kernel. Both
systemd units therefore carry `IPAddressDeny=any` with an allowlist of
loopback only. If any of the analysis below is wrong, that is what
holds — and the denial appears in the audit log rather than as silent
traffic.

---

## 1. What was found and closed

These three were the real finding of the audit, and they falsified the
original plan's "zero egress is three config keys" claim.

| Tool id | Reaches | Trigger | Registered at |
|---|---|---|---|
| `search` (web fallback) | `html.duckduckgo.com`, then `www.google.com` when that bot-blocks, then `lite.duckduckgo.com` | **any chat turn** where the top LOCAL retrieval score is below `SCORE_SUFFICIENT` — i.e. precisely when the corpus is thin | `sovereign-server/src/main.rs`, `SearchTool::with_web(...)` |
| `web_fetch` | **any URL the model emits.** Validation is scheme-only: no host allowlist, follows up to 5 redirects | any chat turn where the model chooses it | `sovereign-server/src/main.rs`, `WebFetchTool::new()` |
| `wikipedia_fetch` | `en.wikipedia.org` | any chat turn; also the daemon's own MCP surface | `sovereign-server/src/main.rs` and `sovereign-cli-daemon/.../tool_registry.rs` |

Three things about this are worth stating plainly:

1. **They were not covered by any config key.** No TOML setting, no env
   var, no tool allowlist. We searched for `disabled_tools`,
   `tool_allowlist`, `deny_tools`, `SOVEREIGN_DISABLE_TOOLS` — no such
   surface exists in a production binary.
2. **`Permission::Network` is not a control.** `WebFetchTool` declares
   it, and `sovereign-core/src/executor.rs` will skip a tool whose
   permission is stored false — but that check exists at exactly one
   call site, inside the plan executor. The chat and agent paths call
   `tool.execute()` directly and never consult it.
3. **`--no-default-features` did not remove them.** It removed
   `ShellTool`, which sits three lines above them in the same function.
   The adjacency is why this was easy to miss.

**What we changed.** A `net-tools` cargo feature, default ON so every
other build in the fleet is unchanged, removed by
`--no-default-features`. Under the hardened build:

- `web_fetch` and `wikipedia_fetch` are **not registered at all** — the
  model cannot call a tool that does not exist in the registry.
- `search` **is still registered**, over the installed corpora only. It
  is constructed with `SearchTool::new` instead of
  `SearchTool::with_web`, so the web fallback has no backend to fall
  back to. Corpus search is the product; reaching the open web was a
  separate capability sharing a tool id.

`net-tools` is deliberately a different flag from `dev-routes`. Those
gate developer surfaces whose risk is *privilege* (a shell, an arbitrary
file read). These gate product features whose risk is *egress*. One flag
for two unrelated decisions would make neither name true.

**The daemon's `wikipedia_fetch` remains registered** (its MCP tool
registry has no equivalent feature). It is reachable only from
`127.0.0.1:9741`, which on this box only `sovereign-server` talks to,
and `sovereign-server` does not drive the daemon's MCP surface. Anyone
who could reach it already has a shell on the box. `IPAddressDeny` is
the control; we are naming it rather than claiming it is absent.

---

## 2. Switched off by configuration

Each of these is live by default and off in the shipped config. The
config file comments explain each in place; this table is the index.

| # | Destination | Trigger | Key that stops it | Default |
|---|---|---|---|---|
| 1 | `en.wikipedia.org/w/api.php` (MediaWiki poller) | daemon startup, after 0-15 min jitter, then every 24 h | `[daemon] freshness_watchers_enabled = false` | **true** |
| 2 | n0 public relays + n0 DNS/pkarr (`iroh.link`) | daemon startup, when iroh resolves on | `[iroh] enabled = false` | auto |
| 3 | n0 relays + n0 DNS — **the join path** | `svrn mesh join`, or `POST :9742/internal/mesh/join` | `[iroh] discovery = "none"` — **`enabled` does NOT gate this** | n0 on |
| 4 | mDNS multicast `224.0.0.251:5353` / `ff02::fb` | daemon startup, unconditional when enabled | `[discovery] mdns = false` | **true** |
| 5 | Commonwealth activity reporting | server startup + a 60 s decay loop | leave `[commonwealth]` out of server-config.toml | unset |
| 6 | Operator-declared MCP servers | server startup | leave `[[mcp.servers]]` empty | empty |

### Corrections to our own earlier claims

An earlier draft of the deployment plan made four assertions here. Two
were wrong and one was half right. They are corrected rather than
deleted, because a reviewer who reads both documents deserves to see
which way the error went.

- **"`[iroh] enabled = false` removes all relay and n0 DNS traffic" —
  refuted.** The mesh-join path builds a relayed endpoint from
  `relay_urls` + `discovery` without ever consulting `enabled`. Only
  `discovery = "none"` closes it. That key is therefore load-bearing
  *independently* of `enabled`, and the shipped config sets both. (A
  second override exists: a mesh whose policy demands encryption turns
  iroh on regardless of an explicit `false`. Not reachable here — this
  box never creates or joins a mesh — but it is a real path and a
  reviewer will find it.)
- **"`freshness_watchers_enabled = true` spawns a Wikipedia poller AND a
  Wikimedia SSE stream at startup" — half right.** The MediaWiki poller
  is real. The Wikimedia `recentchange` SSE stream is **compiled-in dead
  code**: its `spawn()` has no caller in any binary. Nothing dials
  `stream.wikimedia.org`. Two further gates also apply to the poller: it
  needs a corpus-engine handle, and every tick returns early unless the
  `wikipedia-newsworthy` corpus is installed — which on this box it is
  not. The flag is still the right switch, because it is the only one
  that stops the task existing at all.
- **"`mobile_host` defaults iroh on, tunnelling the local HTTP port via
  third-party relays" — true of the *generator*, not of this
  deployment.** `MobileHostConfig` does default it true, and the
  resulting tunnel does forward accepted streams to the local HTTP port.
  But that type is reached only from the desktop app and from
  `svrn mobile serve`, neither of which runs here, and
  `sovereign-server`'s own `[iroh] enabled` defaults to **false**. The
  operational rule is "do not run `svrn mobile serve`, and do not use a
  mobile-host-generated server config" — not "a live risk in this box".
- **"`[discovery] mdns = false` is required because a multicast bind
  failure is fatal at boot" — confirmed.** The error propagates with `?`
  out of daemon startup and the process exits 1. On a hardened or
  containerized host this is the most likely cause of a first-boot
  failure.

### One more, verified local

`[knowledge_view]` defaults to `enabled = true` on the server and
background-ingests conversations into corpora. It makes **no network
call** — every file under the knowledge-view tree was searched for HTTP
clients and URL literals and none exist. It is nonetheless switched off
in the shipped config, for a confidentiality reason rather than an
egress one: on a single-tenant box it would fold one matter's
conversation into a corpus another matter can retrieve.

One caveat: it takes an inference handle, which resolves to whatever
`[[inference.backends]]` is configured. Ours is the loopback daemon. If
that were ever pointed at a remote endpoint, this would become egress.

---

## 3. Loops that run but send nothing

Honest accounting: these background tasks start and tick. They have no
peers to talk to, so they emit no packets — but a reviewer watching
`ss -tnp` should know why the tasks exist.

| Loop | Behaviour with no mesh |
|---|---|
| Gossip (`/internal/gossip`) | targets come from `mesh.json` members; with none, zero packets |
| Auto-ingest collaboration | only reaches `http://127.0.0.1:{port}/internal/corpus/collaborate` |
| Peer inference / model fetch / worker control | all peer-address-driven; no peers, no calls |

`[daemon] max_peer_inflight = 0` additionally opts this node out of peer
inference admission entirely.

---

## 4. In the tree, not reachable here

Present in the binary or the source, and not on any path either process
takes. Listed so the reviewer who greps for hostnames finds the answer
here instead of raising it.

- **`huggingface.co`** — GGUF download and GLiNER model download. Reached
  only from `svrn setup`, `svrn setup fim`, and
  `svrn corpus extract-entities --download-model`. This install runs
  none of them: `install.sh` stages models from the tarball and writes
  both configs by hand, precisely so `svrn setup` is never invoked. The
  daemon's own boot path loads GLiNER from disk.
- **Bulk corpus sources** — `dumps.wikimedia.org`, `www.gutenberg.org`,
  `openalex.s3.amazonaws.com`, `www.courtlistener.com`, `www.sec.gov`,
  `www.govinfo.gov`, `www.federalregister.gov`, `archive.org`,
  `raw.githubusercontent.com` and others. These are URLs in *recipe TOML
  data files*, reached only by corpus-build verbs. The `us-code` corpus
  ships prebuilt as a snapshot; nothing on this box builds a corpus from
  a recipe.
- **`updates.sovereign.dev`** — a corpus index-manifest fetch. Its only
  caller is the desktop app's health builder. Not linked into either
  process here. This is the closest thing in the tree to an update
  check, and it is not reachable.
- **`registry.npmjs.org`** — an `npm install` subprocess in
  `svrn setup fim`. Not run.
- **CalDAV and SMTP** — `CalendarTool` and `EmailTool` exist and are
  registered nowhere in the workspace. `EmailTool` additionally sits
  behind a cargo feature neither binary enables.
- **`github.com/.../releases`** — a string that is printed, never
  fetched.

No `git fetch`/`clone`/`pull`/`ls-remote` anywhere in scope; every `git`
subprocess is a local read (`log`, `status`, `diff`, `rev-parse`). No
`curl`, `wget`, `rsync`, `ssh`, or `scp` subprocesses. The one
`tailscale` invocation is a local status query.

---

## 5. What to expect in the logs

DNS resolution happens as part of an HTTP call, never on its own — so a
box making no HTTP calls issues no DNS. If the firm's egress firewall
logs a denial from this host, it is a finding, not noise, and these are
the names to look for: `duckduckgo.com`, `google.com`,
`en.wikipedia.org`, `huggingface.co`, and any n0 relay. Each maps to a
row above; please send us the log line.

## 6. How to verify this yourself

Nothing here asks to be taken on trust:

```bash
# 1. The hardened binary is the one installed (also acceptance.sh check 0)
curl -s -o /dev/null -w '%{http_code}\n' -X POST https://<host>/v1/solve   # expect 404

# 2. The egress tools are gone from the registry
curl -s -H "Authorization: Bearer <key>" https://<host>/v1/tools \
  | jq -r '.[].name // .tools[].name' | sort
# expect: no web_fetch, no wikipedia_fetch. `search` present = corpus search.

# 3. Nothing is dialling out
ss -tnp | grep -v '127.0.0.1'    # expect only inbound :443 from clients

# 4. The kernel-level control is armed
systemctl show firm-rag-server -p IPAddressDeny -p IPAddressAllow
systemctl show firm-rag-daemon -p IPAddressDeny -p IPAddressAllow
```
