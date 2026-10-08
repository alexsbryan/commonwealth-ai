# On-prem grounded search — IT brief

**For:** the firm's IT team.
**Time to install:** about an hour, most of it waiting for files to copy
and the first index of your document share.
**Network required:** none. This box never contacts the internet — see
`EGRESS.md`, which accounts for every outbound call in the source.

---

## What this is, in one paragraph

A question-answering system over the firm's own documents, running
entirely on one machine you control. Lawyers ask questions in plain
English; the system searches the documents, answers, and cites the exact
passages it used. What makes it different from a chatbot is what it does
when it *cannot* find the answer: it says so, rather than producing a
plausible paragraph. That behaviour is the product, and step 4 of the
install proves it on your hardware before anyone logs in.

---

## Security posture — the one page

**No data leaves the building.** The process makes no outbound
connections. That is enforced three ways: by which surfaces are composed
into the binary, by configuration, and by `IPAddressDeny=any` in the
systemd unit — so if the first two are wrong somewhere, the kernel
refuses the connection and logs it. `EGRESS.md` is the line-by-line
audit, including the tools that *did* reach the internet and are not in
this binary.

**One process, one front door.** Lawyers reach nginx over TLS. nginx
allowlists exactly fourteen routes, each with the methods it takes, and
404s everything else. Behind it, one process listens on loopback.

```
lawyers ──TLS──▶ nginx :443            route allowlist, access log
                    │ loopback
                    ▼
              sovereign-onprem :9741   searches, answers, cites;
                                       owns the model files and the index.
                    :9742, :9748 → 127.0.0.1 only (internal, serve)
```

**Every caller presents a key — including loopback.** install.sh writes
API keys into the daemon's key store. A daemon holding any key
identifies EVERY caller by key, so nginx on the same host lends a remote
caller no trust it does not have. Keys are bearer tokens over TLS; nginx
passes the header through and the daemon decides.

**Routes that could reach a shell or the web are not in the binary.**
The general build of this software assumes one operator who is also the
developer — it is the same program that runs on a laptop. The on-prem
binary is built from a composition that leaves out what is reasonable
there and not here:

| Not in this binary | What it would have allowed |
|---|---|
| The code program (`/v1/solve/jobs`, `/v1/projects`) | A caller-supplied command reaching a shell. It answers a 503 naming its absence and pointing at `/v1/conversations`. |
| The MCP routes (`/mcp`, `/mcp/message`, `/mcp/stats`) | A developer control channel. They answer a 503: "this distribution does not serve MCP", with the same pointer; `GET /v1/mcp/servers` reports it not mounted. |
| The `search` tool's web fallback, `web_fetch`, `wikipedia_fetch`, `probe_url` | Outbound calls to DuckDuckGo, Google, Wikipedia, or any URL, on an ordinary question. Answers never offer a web search either: the prompts drop that offer when no registered tool reaches the web. |

Ingesting a server-side path (`POST /v1/documents`, the corpus register
routes) is still in the binary, because that is how IT indexes the
document share — and it takes IT's key. A lawyer's key is refused it by
name, and nginx does not proxy it. Acceptance check 0 proves all of this
at the daemon's own port, not only through nginx.

**Logging.** nginx records who reached what and when. It does not record
question or answer text. The process journals to systemd. Questions and
answers are stored in one SQLite file (see Backup).

---

## Install runbook

Prerequisites: a Linux x86-64 box, `nginx`, `curl`, `jq`, `zstd`, and a
GPU with enough memory for the model profile you were quoted. Root.

```bash
# 1. Verify the archive against the checksum we read to you separately.
sha256sum -c firm-rag-<version>.tar.zst.sha256

# 2. Unpack and install. install.sh re-verifies every file in the kit
#    against a manifest and refuses to run if anything differs.
tar --zstd -xf firm-rag-<version>.tar.zst
cd firm-rag-<version>
sudo ./install.sh --docs /srv/firm-docs --hostname firm-rag.example.com
```

`--docs` is the share holding the firm's documents. It is mounted
**read-only** into the service sandbox: this system indexes those files
and never writes to them.

`install.sh` creates a service account, stages the binaries, models and
OCR assets, writes the config, and starts the process twice. First
without keys, on loopback, to restore the prebuilt legal corpus and
index the document share (the slow step on a large share). Then it
issues two API keys, writes the corpus allow-list, and starts it with
keys. It is idempotent — re-running it will not overwrite your config or
regenerate keys.

Then four things it cannot do for you:

```bash
# 1. TLS certificates.
sudo install -D -m 0644 fullchain.pem /etc/ssl/firm-rag/fullchain.pem
sudo install -D -m 0600 privkey.pem   /etc/ssl/firm-rag/privkey.pem
sudo nginx -t && sudo systemctl reload nginx

# 2. Fill in the probes. See the comments in the file — they must come
#    from your practice area and one of your own scans.
sudo $EDITOR /etc/firm-rag/acceptance-probes.env

# 3. Collect the keys (root-readable only). 'firm' is the lawyers' key;
#    'it' is yours.
sudo cat /etc/firm-rag/issued-keys.txt

# 4. Prove it. Against the daemon's port AND the TLS hostname.
sudo BASE_URL=https://firm-rag.example.com \
     API_KEY=<firm key> ADMIN_KEY=<it key> ./acceptance.sh; echo "exit=$?"
```

**Step 4 is the install.** Gate on the exit code:

| Exit | Meaning |
|---|---|
| `0` | Ready. |
| `1` | Something failed. The output names what and why. Do not proceed. |
| `2` | Something could not be *judged* — a probe is missing, a service did not answer. **Not a pass.** Resolve and re-run. |

The suite checks, in order: the shell, upload and MCP routes are never
served (0), a request with no key is refused on every client route (0b),
no web tool is held (0c), every allow-listed corpus is listed (2), an
answer carries real citations (3), an unanswerable question produces a
refusal rather than a guess (4), the models are loaded (1, read after
the questions, since the main model loads on first use), and a scanned
PDF produces searchable text (5). Checks 0 and 0b run at the daemon's
port and through nginx; without `BASE_URL` the nginx leg is reported as
never run.

---

## Day-to-day

```bash
systemctl status firm-rag
journalctl -u firm-rag -f

# Is the system answering?
curl -sf https://<host>/health          # expect: ok

# What is in the index, and what did the last sweep skip? (IT's key;
# the corpus id is in [retrieval] corpora in /etc/firm-rag/daemon-config.toml)
curl -s -H "Authorization: Bearer <it key>" \
    http://127.0.0.1:9741/internal/corpus/watch/state/<corpus id> | jq
```

That last command is the one to check after adding documents. Files the
system could not read are listed under `failed_files` with a reason —
encrypted PDFs, formats with no extractor, scans it could not OCR.
Nothing is silently dropped, but nothing announces itself either: you
have to look.

**Adding documents:** copy them into the watched share. A sweep runs
every five minutes and picks up additions, edits and deletions.

**Keys:** `svrn daemon key --add <name>` (add `--group admin` for IT),
`--revoke <name>`, `--list`, run as the service account with
`SVRNMESH_DATA_DIR=/var/lib/firm-rag`. With the daemon running the verb
asks it, presenting IT's key from `SOVEREIGN_API_KEY`, and a change takes
effect on the next request, a revoke included. With the daemon stopped it
edits the key store, which the daemon reads when it starts.

**Restarting:** `systemctl restart firm-rag` reloads the models and
takes 30-90 seconds. The API returns errors during that window.

---

## Backup and restore

Everything that matters is in two places:

| Path | What | Replaceable? |
|---|---|---|
| `/var/lib/firm-rag/sovereign.db` | **every conversation and answer** | No. This is the only irreplaceable file. |
| `/var/lib/firm-rag/client-tokens/` | the API keys | Yes, but losing them means reissuing every key |
| `/var/lib/firm-rag/indexes/` | the search index | Yes — rebuilt from the documents, slowly |
| `/etc/firm-rag/` | the config and the issued-keys record | Yes |
| `/var/lib/firm-rag/models/` | model weights | Yes — from the kit |

```bash
systemctl stop firm-rag
tar -czf firm-rag-backup-$(date +%F).tar.gz \
    /var/lib/firm-rag/sovereign.db /var/lib/firm-rag/client-tokens /etc/firm-rag
systemctl start firm-rag
```

Stop the service first. SQLite is being written to while it runs, and a
copy taken mid-write may not restore.

Restore is the reverse, onto the same version of the software. The
documents themselves are on your share and are never modified by this
system, so they are covered by whatever already backs that share up.

---

## Settings that moved

The kit used to run a second process, `sovereign-server`, with its own
`server-config.toml`. Both are gone. Where each setting went:

| server-config.toml | Now |
|---|---|
| `[auth] mode`, `[auth.keys]` | the daemon's key store, `svrn daemon key` (install.sh issues `firm` and `it`) |
| `[retrieval] corpora` | `[retrieval] corpora` in `daemon-config.toml`, written by install.sh with the ids it installed |
| `[server] bind` | `[daemon] client_bind` / `client_port` (`install.sh --port`) |
| `[store] path` | `/var/lib/firm-rag/sovereign.db`, under `[data] dir` |
| `[inference]`, `[[inference.backends]]` | dropped: one process owns the weights (`[models]`) |
| `[server] max_concurrent_turns`, `max_per_user`, `max_queue_depth`, `retry_after_secs` | dropped: the daemon has no such keys |
| `[server] cors`, `allow_unauthenticated_remote` | dropped: no daemon equivalent; nginx is the only remote door |
| `[knowledge_view] enabled` | dropped: the daemon has no such key |
| `[iroh]` | `[iroh]` in `daemon-config.toml` (and not started at all here: EGRESS.md §5) |

---

## Honest limits

These are pilot constraints we chose, not defects to be discovered. Each
is listed with what it would take to remove.

**One corpus scope for every key.** Each key owns its own conversations
— a key cannot list or read another key's. But every key retrieves from
the same `[retrieval] corpora`. There is no per-user or per-matter
document access control:

- **A matter under an ethical wall must not be ingested into this
  system.** A conflicts screen needs per-matter corpus grants, which
  this pilot does not have. This is the constraint most likely to matter
  to the firm, and it is not a setting we can turn on.

**No single sign-on.** Static bearer keys, issued and revoked with
`svrn daemon key`. Fine for a dozen pilot
users; not fine for a firm.

**Concurrency.** Questions queue on one model. The REST API gives no
"you are third in line" signal while waiting — it simply takes longer.
Roughly ten simultaneous users is where this becomes noticeable.

**Document formats.** PDF, TXT, MD, HTML, MHTML, EPUB, DOCX. Scanned
PDFs are handled via OCR. **Not supported: `.doc`, `.msg`, `.pst`,
`.xlsx`.** For litigation, `.msg`/`.pst` is the likely first ask, and it
is not a small piece of work.

**OCR quality.** Scanned pages are read by a recognition model and then
cleaned up by the language model. On a keyed box the clean-up call is
currently refused (EGRESS.md §4), so scans are indexed as raw
recognised text, marked as such. It is good, not perfect. A misread
digit in a damages figure is a real failure mode; treat OCR'd text as a
finding aid pointing at the original page, not as the record.

**No desktop app, no mesh, no mobile access.** All deliberately out of
scope for the pilot.

**Test coverage.** The on-prem binary's composition and its key-scoped
routes are tested in our CI on a model-free engine. Nothing there runs
your models, your corpus or your nginx: `acceptance.sh` is the control
for that, and it runs on *your* box against the binaries you actually
installed.

---

## If something goes wrong

**The service will not start.** It refuses to start rather than starting
degraded, so the journal names the reason:
`journalctl -u firm-rag -n 100`. Most likely causes, in order:

1. A model path in `daemon-config.toml` that does not exist. The startup
   check labels the slot `UNREADABLE` and prints a repair hint.
2. A corrupt model file — copied incompletely from the kit. This one is
   not caught by the startup check; it fails later with
   `failed to load models`. Re-verify against `MANIFEST.sha256`.
3. A port another process holds (9741, 9742 or 9748).

**Every request gets 401.** That is right without a key. With one, the
key may have been added after the process started — restart it — or
revoked.

**Answers have no citations.** Check that the corpus is listed
(`curl -H "Authorization: Bearer <key>" https://<host>/v1/corpora`) and
that `[retrieval] corpora` in `daemon-config.toml` names it. On a keyed
box an empty or misspelled allow-list grants nothing, silently — which
is why `acceptance.sh` check 2 re-reads the list against the running
system rather than trusting the file.

**Scanned PDFs are not searchable.** Read the watched corpus's
`failed_files` (Day-to-day, above). If they appear as `scanned_no_text`,
OCR is not running; the journal will carry an `ocr:unavailable` line
naming every path it looked in for the OCR models.

**Everything is slow.** One model, one queue. Check for a re-index
running against the document share — a large addition can occupy the
box for a while.

---

## What v2 would add

In the order we would build it, each tied to a limit above:

1. **Per-matter access control.** Removes the ethical-wall constraint.
   Largest piece of work here, and the one that turns a pilot into
   something the firm can standardise on.
2. **SSO** against the firm's identity provider, producing the same
   key-shaped identity the daemon already decides on.
3. **`.msg` / `.pst` ingestion.** The litigation-specific gap.
4. **Queue position and progress** on the REST path, so a slow answer
   looks like a slow answer rather than a broken system.
5. **More legal corpora** — court opinions, agency guidance. Held out of
   v1 for licensing and size, not for technical reasons.

---

## Files this kit installs

| Path | What |
|---|---|
| `/opt/firm-rag/bin/` | `sovereign-onprem`, and the `svrn` CLI with the two siblings its install verbs run |
| `/var/lib/firm-rag/` | models, OCR assets, search index, keys, **conversations** |
| `/var/lib/firm-rag/config.toml` | a link to the config, the path the `svrn` verbs read |
| `/etc/firm-rag/daemon-config.toml` | model paths, ports, retrieval scope, network switches |
| `/etc/firm-rag/issued-keys.txt` | the keys install.sh issued, mode 0600 |
| `/etc/firm-rag/acceptance-probes.env` | your test probes |
| `/etc/systemd/system/firm-rag.service` | the one unit |
| `/etc/nginx/conf.d/firm-rag.conf` | TLS + the route allowlist |
| `/etc/nginx/snippets/firm-rag-proxy.conf` | shared proxy settings |
| `/etc/nginx/snippets/firm-rag-proxy-headers.conf` | the forwarding headers, which the stream route takes alone |

`daemon-config.toml` is commented in detail, including which keys are
dangerous to change and why. It is worth reading before editing — it
rejects no unknown key, so a typo is silently ignored rather than
reported.
