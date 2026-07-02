# Runbook — two-network no-VPN join test (JOINER side)

**Audience:** an agent (or operator) on a *second machine, on a genuinely
different network from the founder*, with no VPN. This is validation item (a)
of the mesh enterprise-hardening plan: prove that the no-VPN connect code
reaches the founder across real networks and NAT, and measure the cost.

You are the **JOINER**. The **FOUNDER** is the other machine (the one that
handed you this runbook); it has already created the mesh and produced an
invite link. Your job is to join over iroh, confirm mesh + corpus work, record
which path (direct vs relayed) and the latency, and report back.

This test is only meaningful if the two machines genuinely cannot reach each
other by IP. **Do not run both on the same LAN or the same VPN** — that would
pass without exercising the relay/hole-punch path, defeating the point.

---

## 0. Preconditions (verify before starting — abort if any fail)

1. **Sovereign is built and the daemon runs here.**
   ```
   sovereign daemon status        # or: sovereign daemon start
   ```
2. **You are on a different network from the founder, no VPN.** Confirm no
   shared subnet and no VPN interface:
   ```
   ip -4 addr | grep -v '127.0.0.1'      # note your addresses
   ip link | grep -iE 'tun|tailscale|wg' # expect NO output (no VPN)
   ```
   Compare your subnet to the founder's (they'll tell you theirs). If they
   overlap, or a VPN iface exists, **stop** — this won't be a real cross-network
   test. A phone hotspot or a cloud VM on a different provider is a good joiner.
3. **You have the founder's full invite link** — the one containing `dial=`
   (NOT a bare `cwth-…` key). It looks like:
   ```
   sovereign://join/cwth-XXXX-XXXX-XXXX?name=...&dial=<hex>@<relay-or-addr>[,...]
   ```
   If the founder gave you only a bare key, ask them to re-share from
   `sovereign mesh status` (the `join link:` line) — the bare key can't cross
   networks.

Record start context:
```
date -u; uname -a; sovereign --version 2>/dev/null || true
```

---

## 1. Join over iroh

Join with the FULL link, single-quoted:
```
time sovereign mesh join 'sovereign://join/cwth-XXXX-XXXX-XXXX?name=...&dial=<...>'
```
Capture the wall-clock `time` output (join latency across the real network).

If it fails, capture the exact error (it should name the failure — relay
unreachable, dial parse, etc.) and skip to §5 with a FAIL.

---

## 2. Confirm convergence + transport path

```
sovereign mesh status          # expect members_total >= 2, founder Online
sovereign mesh transport       # per-peer iroh path
```
From `sovereign mesh transport`, record the founder's `path`:
- `relayed` — reached via the relay (expected when both sides are behind NAT
  with no hole-punch). **This is the headline success: no VPN, different
  networks, still connected.**
- `direct` — a direct hole-punched path was established (even better).
- `mixed` — both active.
- `idle`/absent — not connected over iroh; investigate (see §4).

Also grab the raw JSON for the report:
```
curl -s http://127.0.0.1:9741/v1/mesh/status | python3 -m json.tool > /tmp/joiner-status.json
```

## 3. Prove a real feature works across the link

Pick whichever the founder hosts:

- **Corpus search** (founder hosts a corpus; ask which id):
  ```
  curl -s -X POST http://127.0.0.1:9741/v1/knowledge/search \
    -H 'content-type: application/json' \
    -d '{"query_text":"<a term in the founder corpus>","corpora":["<corpus-id>"],"limit":5}' \
    | python3 -m json.tool
  ```
  Expect non-empty `results` attributed to the founder's corpus — proof that a
  ControlPlane/KnowledgeSearch round-trip crossed the network over iroh.

- **Chat** (founder serves a model): send one `/v1/chat/completions` turn and
  confirm a streamed completion. Capture time-to-first-token.

## 4. Resilience (optional but valuable)

- Toggle your network (airplane-mode / drop the interface ~10s, restore).
  Re-run `sovereign mesh status` — the mesh should reconverge without a re-join.
- Re-run `sovereign mesh transport` — note whether the path changed
  (direct↔relayed) after the flap.

---

## 5. Report back

Write a findings blob and hand it to the founder (paste, shared file, or if
you're a mesh agent, message the founder node). Include:

```
two-network join test — JOINER report
  joiner network:     <ISP / cloud / hotspot; your public-facing situation>
  no VPN confirmed:   <yes/no>  (ip link showed no tun/wg/tailscale)
  distinct subnet:    <yes/no>  (vs founder's subnet)
  join result:        <SUCCESS | FAIL: reason>
  join latency:       <the `time` from §1>
  founder path:       <direct | relayed | mixed | idle>   (from `mesh transport`)
  feature check:      <corpus search: N results | chat: TTFT Xs | FAIL>
  reconnect after flap: <clean | re-join needed | n/a>
  notes:              <anything surprising>
attach: /tmp/joiner-status.json
```

### Pass criteria
- **PASS** if: joined with no VPN, from a genuinely distinct network, mesh
  converged to ≥2 members, and a feature round-trip (corpus or chat) succeeded —
  regardless of whether the path is `direct` or `relayed`.
- **FAIL** if: join failed, or the only way it worked was a shared subnet/VPN
  (i.e. not actually a cross-network test).

Note the path honestly: `relayed` is a legitimate PASS (that's the fallback
working), but record it — a fleet that only ever gets `relayed` may want a
closer/self-hosted relay for latency.

---

## Founder-side quick reference (for the machine handing this out)

```
sovereign mesh create "Cross-Net Test"      # if not already a mesh founder
sovereign mesh status                        # copy the `join link:` (has dial=)
# tell the joiner your subnet (ip -4 addr) so they can confirm it differs
# after the test: sovereign mesh transport   # you should see the joiner too
```
If the founder's `join link:` shows no `dial=`, its iroh endpoint hasn't learned
a reachable address yet — wait a few seconds and re-read status (the invite
refreshes live). If it never appears, check `[iroh] enabled` isn't forced off
and that the founder can reach a relay (`sovereign doctor` → `iroh_egress`).
