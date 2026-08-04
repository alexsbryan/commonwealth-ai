# On-prem grounded search — IT brief

**For:** the firm's IT team.
**Time to install:** about an hour, most of it waiting for files to copy.
**Network required:** none. This box never contacts the internet — see
`EGRESS.md`, which accounts for every outbound call in the source.

---

## What this is, in one paragraph

A question-answering system over the firm's own documents, running
entirely on one machine you control. Lawyers ask questions in plain
English; the system searches the documents, answers, and cites the exact
passages it used. What makes it different from a chatbot is what it does
when it *cannot* find the answer: it says so, rather than producing a
plausible paragraph. That behaviour is the product, and step 6 of the
install proves it on your hardware before anyone logs in.

---

## Security posture — the one page

**No data leaves the building.** The two processes make no outbound
connections. That is enforced three ways: by configuration, by which
code was compiled into the binaries, and by `IPAddressDeny=any` in the
systemd units — so if the first two are wrong somewhere, the kernel
refuses the connection and logs it. `EGRESS.md` is the line-by-line
audit, including three tools that *did* reach the internet and were
removed at compile time.

**The API is not the whole program.** Lawyers reach nginx over TLS.
nginx allowlists exactly thirteen routes and 404s everything else.
Behind it, two services talk only over loopback.

```
lawyers ──TLS──▶ nginx :443            route allowlist, access log
                    │ loopback
                    ▼
              sovereign-server :8080     searches, answers, cites.
                    │                    Built --no-default-features.
                    │ loopback
                    ▼
              svrn daemon :9741          owns the model files.
                    :9742 → 127.0.0.1    (internal port: no auth, ever)
```

**Routes that could reach a shell are not in the binary.** The general
build of this software assumes one operator who is also the developer —
it is the same program that runs on a laptop. Under that assumption
several surfaces are reasonable that are not reasonable here:

| Removed from this build | What it would have allowed |
|---|---|
| `POST /v1/solve`, `/v1/cycle/bdd` | A caller-supplied command reaching a shell, *inside* the authenticated API. Any valid key would be a shell on this box. |
| `POST /v1/documents/upload`, `/v1/corpora/upload` | Ingesting any absolute path on the server — including the config file holding the API keys — into a searchable corpus. |
| `/mcp`, `/mcp/message`, `/mcp/stats` | A developer control channel outside the authentication layer, guarded only by a same-host check that a reverse proxy satisfies for every remote caller. |
| The `search` tool's web fallback, `web_fetch`, `wikipedia_fetch` | Outbound calls to DuckDuckGo, Google, Wikipedia, or any URL, on an ordinary question. |

The last row is the one worth dwelling on: those three fired on normal
turns, had no configuration switch, and were not removed by the same
build flag that removed the others. We found them by auditing the source
for this deployment. They are gone from this binary, and check 0c of the
acceptance suite verifies that on your box.

**Authentication** is a static bearer token per key, over TLS. `nginx`
passes the header through; the application decides. Note the pilot's
tenancy model below — one shared tenant — which is a real limit, not a
detail.

**Logging.** nginx records who reached what and when. It does not record
question or answer text. The application journals to systemd. Questions
and answers are stored in one SQLite file (see Backup).

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
OCR assets, writes both configs, restores the prebuilt legal corpus,
registers the document share, and enables both units. It is idempotent —
re-running it will not overwrite your configs or regenerate keys.

Then four things it cannot do for you:

```bash
# 3. TLS certificates.
sudo install -D -m 0644 fullchain.pem /etc/ssl/firm-rag/fullchain.pem
sudo install -D -m 0600 privkey.pem   /etc/ssl/firm-rag/privkey.pem
sudo nginx -t && sudo systemctl reload nginx

# 4. Fill in the three probes. See the comments in the file — they must
#    come from your practice area and one of your own scans.
sudo $EDITOR /etc/firm-rag/acceptance-probes.env

# 5. Collect the API key (root-readable only) and hand it out.
sudo cat /etc/firm-rag/issued-keys.txt

# 6. Prove it. Against the TLS hostname, NOT localhost.
sudo ./acceptance.sh; echo "exit=$?"
```

**Step 6 is the install.** Gate on the exit code:

| Exit | Meaning |
|---|---|
| `0` | Ready. |
| `1` | Something failed. The output names what and why. Do not proceed. |
| `2` | Something could not be *judged* — a probe is missing, a service did not answer. **Not a pass.** Resolve and re-run. |

The suite runs twelve checks in six groups: the dangerous routes are
gone (0), an unauthenticated request is refused (0b), the outbound tools
are not registered (0c), the models are loaded (1), the legal corpus is
present (2), an answer carries real citations (3), an unanswerable
question produces a refusal rather than a guess (4), and a scanned PDF
produces searchable text (5).

We built the suite to distinguish "failed" from "could not tell", and
tested it against a dead box, a correctly-hardened stub, and a
deliberately leaky one, to confirm it reports each correctly. An earlier
version reported an unreachable service as "route is REACHABLE", which
is the worst thing a security check can say.

---

## Day-to-day

```bash
systemctl status firm-rag-daemon firm-rag-server
journalctl -u firm-rag-daemon -f
journalctl -u firm-rag-server -f

# Is the system answering?
curl -sf https://<host>/health          # expect: ok

# What is in the index, and what did the last sweep skip?
sudo -u firmrag /opt/firm-rag/bin/svrn corpus watch-status firm-docs
sudo -u firmrag /opt/firm-rag/bin/svrn corpus watch-status firm-docs --failures
```

That last command is the one to check after adding documents. Files the
system could not read are listed there with a reason — encrypted PDFs,
formats with no extractor, scans it could not OCR. Nothing is silently
dropped, but nothing announces itself either: you have to look.

**Adding documents:** copy them into the watched share. A sweep runs
every five minutes and picks up additions, edits and deletions.

**Restarting:** `systemctl restart firm-rag-daemon` reloads the models
and takes 30-90 seconds. The API returns errors during that window.

---

## Backup and restore

Everything that matters is in two places:

| Path | What | Replaceable? |
|---|---|---|
| `/var/lib/firm-rag/sovereign.db` | **every conversation and answer** | No. This is the only irreplaceable file. |
| `/var/lib/firm-rag/indexes/` | the search index | Yes — rebuilt from the documents, slowly |
| `/etc/firm-rag/` | both configs and the issued keys | Yes, but losing the keys means reissuing them |
| `/var/lib/firm-rag/models/` | model weights | Yes — from the kit |

```bash
systemctl stop firm-rag-server firm-rag-daemon
tar -czf firm-rag-backup-$(date +%F).tar.gz \
    /var/lib/firm-rag/sovereign.db /etc/firm-rag
systemctl start firm-rag-daemon firm-rag-server
```

Stop the services first. SQLite is being written to while they run, and
a copy taken mid-write may not restore.

Restore is the reverse, onto the same version of the software. The
documents themselves are on your share and are never modified by this
system, so they are covered by whatever already backs that share up.

---

## Honest limits

These are pilot constraints we chose, not defects to be discovered. Each
is listed with what it would take to remove.

**One shared tenant.** Every API key sees every document *and every
conversation*. There is no per-user or per-matter access control. Two
consequences:

- **A matter under an ethical wall must not be ingested into this
  system.** A conflicts screen is incompatible with one shared tenant.
  This is the constraint most likely to matter to the firm, and it is
  not a setting we can turn on.
- Two practice groups cannot share this box. The conversation list is
  filtered by tenant *after* the database limit is applied, so a busy
  colleague's afternoon would make your own conversation list render
  empty. **Hard blocker for a second group** — not a tuning problem.

**No single sign-on.** Static bearer tokens, issued by hand, revoked by
editing a config and restarting. Fine for a dozen pilot users; not fine
for a firm.

**Concurrency.** Questions queue on one model. The box is configured for
four at a time; beyond that, callers wait. The REST API gives no
"you are third in line" signal while waiting — it simply takes longer.
Roughly ten simultaneous users is where this becomes noticeable.

**Document formats.** PDF, TXT, MD, HTML, MHTML, EPUB, DOCX. Scanned
PDFs are handled via OCR. **Not supported: `.doc`, `.msg`, `.pst`,
`.xlsx`.** For litigation, `.msg`/`.pst` is the likely first ask, and it
is not a small piece of work.

**OCR quality.** Scanned pages are read by a recognition model and then
cleaned up by the language model. It is good, not perfect. A misread
digit in a damages figure is a real failure mode; treat OCR'd text as a
finding aid pointing at the original page, not as the record.

**No desktop app, no mesh, no mobile access.** All deliberately out of
scope for the pilot.

**Test coverage.** The API layer in this configuration has no automated
tests in our CI, and no release job builds it. `acceptance.sh` exists
because of that: it is the compensating control, and it runs on *your*
box against the binaries you actually installed. We would rather tell
you this than have you find it.

---

## If something goes wrong

**The daemon will not start.** It refuses to start rather than starting
degraded, so the journal names the reason:
`journalctl -u firm-rag-daemon -n 100`. Most likely causes, in order:

1. A model path in `daemon-config.toml` that does not exist. The startup
   check labels the slot `UNREADABLE` and prints a repair hint.
2. A corrupt model file — copied incompletely from the kit. This one is
   not caught by the startup check; it fails later with
   `failed to load models`. Re-verify against `MANIFEST.sha256`.
3. On a hardened or containerized host, a multicast socket bind. The
   shipped config sets `[discovery] mdns = false` to avoid it; if you
   edited that key, put it back.

**Answers have no citations.** Check that the corpus is listed
(`curl -H "Authorization: Bearer <key>" https://<host>/v1/corpora`) and
that `[retrieval] corpora` in `server-config.toml` names it. A typo
there is silent — neither config file rejects unknown keys, which is why
`acceptance.sh` re-reads values from the running system rather than
trusting the file.

**Scanned PDFs are not searchable.** Run
`svrn corpus watch-status firm-docs --failures`. If they appear as
`scanned_no_text`, OCR is not running; the daemon journal will carry an
`ocr:unavailable` line naming every path it looked in for the OCR
models. If they do *not* appear as failed but still are not searchable,
OCR ran and produced poor text — check the journal for a
`raw OCR (cleanup unavailable)` marker.

**Everything is slow.** One model, one queue. Check for a re-index
running against the document share
(`svrn corpus watch-status firm-docs`) — a large addition can occupy the
box for a while.

---

## What v2 would add

In the order we would build it, each tied to a limit above:

1. **Per-matter access control.** Removes the ethical-wall constraint and
   unblocks a second practice group. Largest piece of work here, and the
   one that turns a pilot into something the firm can standardise on.
2. **SSO** against the firm's identity provider.
3. **`.msg` / `.pst` ingestion.** The litigation-specific gap.
4. **Queue position and progress** on the REST path, so a slow answer
   looks like a slow answer rather than a broken system.
5. **More legal corpora** — court opinions, agency guidance. Held out of
   v1 for licensing and size, not for technical reasons.

---

## Files this kit installs

| Path | What |
|---|---|
| `/opt/firm-rag/bin/` | four binaries |
| `/var/lib/firm-rag/` | models, OCR assets, search index, **conversations** |
| `/etc/firm-rag/daemon-config.toml` | model paths, ports, network switches |
| `/etc/firm-rag/server-config.toml` | API keys, retrieval scope |
| `/etc/firm-rag/issued-keys.txt` | generated keys, mode 0600 |
| `/etc/firm-rag/acceptance-probes.env` | your three test probes |
| `/etc/systemd/system/firm-rag-{daemon,server}.service` | the two units |
| `/etc/nginx/conf.d/firm-rag.conf` | TLS + the route allowlist |
| `/etc/nginx/snippets/firm-rag-proxy.conf` | shared proxy settings |

Both `.toml` files are commented in detail, including which keys are
dangerous to change and why. They are worth reading before editing —
neither rejects an unknown key, so a typo is silently ignored rather
than reported.
