#!/usr/bin/env python3
"""Live peer-routing probe — asserts against the REAL mesh.

WHY THIS EXISTS
---------------
`mesh-soak.sh --workload offload` proves a peer *can* serve, but runs
inside a rootless netns so it cannot see the real fleet, and it never
makes a peer decline. `mesh-sim` hardcodes `shed: false`. DST covers
gossip only. So every layer proves acceptance, and nothing checks what
this node does when a real peer REFUSES — which is where both of the
2026-08-06/07 routing bugs lived:

  * a shed peer was booked as a fault, three of them quarantining a
    healthy neighbour for 60 s;
  * a peer declining a load-balanced turn failed the caller outright
    while this node had the very model loaded.

Both were found by hand against RuggedFox↔BeefyMac and neither was
reproducible afterwards, because the harness was a scratchpad script
that a reboot deleted. This is that script, made repeatable and made to
assert.

WHAT IT IS NOT: a quality bench. It says nothing about answer quality —
that is `sovereign-ci-bench.sh`. It checks ROUTING BEHAVIOUR only.

VERDICTS (four, not two — ARCH_PRINCIPLES §18.2)
  0  PASS               every probe that could run, passed
  1  FAIL               an invariant was violated
  4  COULD-NOT-JUDGE    preconditions absent (no online peer, daemon
                        down). A run that verified nothing is NEVER a
                        pass — same rule as sovereign-test.sh's
                        zero-test guard.

A probe whose trigger never occurred (no peer happened to shed) is
reported NOT-OBSERVED and never counted as a pass.

USAGE
  ./scripts/mesh-live-probe.py                       # default: 4 turns at `primary`
  ./scripts/mesh-live-probe.py --turns 8 --model primary
  ./scripts/mesh-live-probe.py --json findings.jsonl
"""
import argparse
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request

DAEMON = "http://127.0.0.1:9741"
# Reasons commonwealth-api's admission layer may return. A 503 from a
# peer gate MUST name one of these; an unstructured 503 means the
# numbers were lost between the gate and the wire, which is the gap
# note bef03728 tracks.
ADMISSION_REASONS = {"paused", "yielded_to_local", "ceiling_exceeded"}

findings = []


def finding(probe, verdict, detail, **extra):
    """One line per probe. `verdict` is pass|fail|not_observed|skipped."""
    rec = {"probe": probe, "verdict": verdict, "detail": detail, **extra}
    findings.append(rec)
    mark = {"pass": "  ok", "fail": "FAIL", "not_observed": "  --", "skipped": "  --"}
    print(f"{mark.get(verdict, '  ?')}  {probe}: {detail}")
    return rec


def get_json(path, timeout=10):
    with urllib.request.urlopen(f"{DAEMON}{path}", timeout=timeout) as r:
        return json.load(r)


def chat(prompt, model, node_id=None, max_tokens=24, timeout=300):
    """Returns (status, body_text, seconds, headers)."""
    body = {
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens,
        "stream": False,
    }
    req = urllib.request.Request(
        f"{DAEMON}/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    if node_id:
        req.add_header("X-Node-Id", node_id)
    t0 = time.time()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode(), time.time() - t0, dict(r.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(), time.time() - t0, dict(e.headers)
    except Exception as e:  # noqa: BLE001
        return None, f"{type(e).__name__}: {e}", time.time() - t0, {}


def served_by(body):
    """Attribution from the response: 'peer <name>' or 'local'."""
    try:
        model = json.loads(body).get("model", "")
    except Exception:  # noqa: BLE001
        return "unparseable"
    return model.split("@ peer ", 1)[1].strip() if "@ peer " in model else "local"


# ── preflight ──────────────────────────────────────────────────────

def preflight():
    try:
        status = get_json("/v1/mesh/status")
    except Exception as e:  # noqa: BLE001
        finding("preflight.daemon", "skipped", f"daemon unreachable at {DAEMON}: {e}")
        return None
    members = status.get("members", [])
    me = next((m for m in members if m.get("is_self")), None)
    peers = [m for m in members if not m.get("is_self") and m.get("status") == "online"]
    if me is None:
        finding("preflight.identity", "skipped", "status lists no self member")
        return None
    if not peers:
        finding(
            "preflight.peers", "skipped",
            "no ONLINE peer in the mesh — a live peer probe with no peer "
            "verifies nothing about peer routing",
        )
        return None
    finding(
        "preflight", "pass",
        f"{me['name']} + {len(peers)} online peer(s): {', '.join(p['name'] for p in peers)}",
        self_node_id=me["node_id"], peers=[p["name"] for p in peers],
    )
    return {"me": me, "peers": peers, "status": status}


def local_advertises(model):
    """Does THIS node advertise `model`?

    The routing invariant below — "a peer declining must never fail the
    caller" — only holds when we could have served it ourselves. Assuming
    that would turn a CORRECT refusal (nobody in the mesh has the model)
    into a false alarm, so it is checked, not assumed.
    """
    try:
        manifest = get_json("/oicp/v1/capabilities")
    except Exception:  # noqa: BLE001
        return None
    return any(m.get("id") == model for m in manifest.get("models", []))


def decision_log_path(explicit=None):
    """Where the daemon writes structured routing outcomes.

    `SOVEREIGN_DECISION_LOG` is what arms it. Without the log this probe
    can still see THAT a turn succeeded, but not WHO was asked — and
    after the load-balanced-fallback fix those look identical from the
    response, because a peer that declines produces a local answer.
    """
    for cand in (explicit, os.environ.get("SOVEREIGN_DECISION_LOG"),
                 os.path.expanduser("~/.sovereign/decisions-EXP.jsonl")):
        if cand and os.path.exists(cand):
            return cand
    return None


def read_outcomes_since(path, offset):
    """Outcome records appended after `offset`, plus the new offset."""
    out = []
    with open(path, "rb") as fh:
        fh.seek(offset)
        chunk = fh.read()
        new_offset = fh.tell()
    for line in chunk.decode("utf-8", "replace").splitlines():
        try:
            rec = json.loads(line)
        except ValueError:
            continue
        if rec.get("event") == "outcome":
            out.append(rec)
    return out, new_offset


def probe_shed_handling(outcomes):
    """THE invariant this harness exists for.

    A peer declining is not a failure. Every outcome that recorded a
    shed must have gone on to be SERVED — by another peer or by us.
    `served_by.kind == "failed"` after a shed is the 2026-08-06 bug:
    the caller got a 503 for a model this node had loaded.
    """
    shed_outcomes = [
        o for o in outcomes
        if any(f.get("shed") for f in (o.get("failovers") or []))
    ]
    if not shed_outcomes:
        finding(
            "shed.never_fails_a_servable_caller", "not_observed",
            f"no peer declined during this run ({len(outcomes)} outcome record(s)) "
            "— the fallback path was not exercised, which is not the same as correct",
            outcomes=len(outcomes),
        )
        return
    failed = [o for o in shed_outcomes if (o.get("served_by") or {}).get("kind") == "failed"]
    if failed:
        finding(
            "shed.never_fails_a_servable_caller", "fail",
            f"{len(failed)}/{len(shed_outcomes)} turns were FAILED after a peer "
            f"merely declined — a shed means 'ask someone else', and this node "
            f"could answer. First: {json.dumps(failed[0].get('failovers'))[:200]}",
            shed_turns=len(shed_outcomes), failed=len(failed),
        )
    else:
        finding(
            "shed.never_fails_a_servable_caller", "pass",
            f"{len(shed_outcomes)} turn(s) hit a declining peer and every one was "
            "still served",
            shed_turns=len(shed_outcomes),
        )


def probe_attribution(outcomes):
    """Three-way truth the HTTP response cannot give you."""
    kinds = {}
    for o in outcomes:
        k = (o.get("served_by") or {}).get("kind", "?")
        kinds[k] = kinds.get(k, 0) + 1
    tried = sum(1 for o in outcomes if o.get("failovers"))
    finding(
        "routing.attribution", "pass" if outcomes else "not_observed",
        f"outcomes by server: {kinds}; {tried} turn(s) attempted a peer first"
        if outcomes else "no outcome records were written for this run",
        kinds=kinds, peer_attempts=tried,
    )


# ── P1: local traffic is never gated; peer traffic is gated STRUCTURALLY ──

def probe_admission(me, model):
    """The receiving side. Same POST twice, differing only in X-Node-Id.

    This is the gate M5 armed. Local requests must never be refused —
    the user's own chat must not 503 because *they* are using their
    machine. Peer-shaped requests may be refused, but only with a named
    reason a load balancer can branch on.
    """
    st, body, secs, _ = chat("Say OK.", model, max_tokens=8)
    if st == 200:
        finding("admission.local_never_gated", "pass", f"unstamped turn served in {secs:.2f}s")
    else:
        finding(
            "admission.local_never_gated", "fail",
            f"an unstamped (local) turn was refused with {st} in {secs:.2f}s — "
            f"the user's own chat must never be gated: {body[:200]}",
        )

    st, body, secs, hdrs = chat("Say OK.", model, node_id=me["node_id"], max_tokens=8)
    if st == 200:
        finding(
            "admission.peer_gate_structured", "not_observed",
            f"peer-shaped turn was ADMITTED in {secs:.2f}s (node quiet) — the "
            "gate did not fire, so its shape is unverified this run",
        )
        return
    retry_after = hdrs.get("Retry-After") or hdrs.get("retry-after")
    try:
        reason = json.loads(body).get("reason")
    except Exception:  # noqa: BLE001
        reason = None
    if st == 503 and reason in ADMISSION_REASONS and retry_after:
        finding(
            "admission.peer_gate_structured", "pass",
            f"peer-shaped turn refused in {secs:.2f}s with reason={reason} "
            f"Retry-After={retry_after}",
            reason=reason, retry_after=retry_after,
        )
    else:
        finding(
            "admission.peer_gate_structured", "fail",
            f"a peer-shaped refusal must carry a named reason AND Retry-After so "
            f"the caller can branch without parsing prose; got status={st} "
            f"reason={reason!r} retry_after={retry_after!r} body={body[:200]}",
        )


# ── P2: a peer declining must never fail a caller this node can serve ──

def probe_routing(model, turns):
    """The regression invariant, and the reason this file exists.

    Fire concurrent named turns. Whatever the peers do — serve, shed,
    or drop — every turn must come back 200, because this node
    advertises the same model and can always answer. A hard error here
    is the 2026-08-06 bug.
    """
    results = {}

    # Pin the local slot first. `locate_named_model` breaks ties in
    # favour of local, so with an idle node the balancer picks local
    # every time and the peer path is never exercised — the probe would
    # pass while testing nothing. One long local turn raises
    # `local_inflight` above the peers' and makes the hop actually
    # happen. This is the step that reached BeefyMac by hand.
    holder = threading.Thread(
        target=lambda: chat(
            "Write a detailed paragraph on consensus algorithms.", model, max_tokens=320
        ),
        daemon=True,
    )
    holder.start()
    time.sleep(3)

    def run(i):
        results[i] = chat(
            f"In one sentence, define quorum. (probe {i})", model, max_tokens=40
        )

    threads = []
    for i in range(turns):
        t = threading.Thread(target=run, args=(i,), daemon=True)
        threads.append(t)
        t.start()
        time.sleep(0.25)
    for t in threads:
        t.join(timeout=300)

    failed = [(i, r) for i, r in results.items() if r[0] != 200]
    attribution = {}
    for i, (st, body, _, _) in results.items():
        if st == 200:
            who = served_by(body)
            attribution[who] = attribution.get(who, 0) + 1

    if failed:
        i, (st, body, secs, _) = failed[0]
        finding(
            "routing.no_hard_failure_when_servable", "fail",
            f"{len(failed)}/{turns} turns failed while this node advertises "
            f"'{model}' and could have served them. First: {st} in {secs:.2f}s "
            f"{body[:220]}",
            failed=len(failed), turns=turns,
        )
    else:
        finding(
            "routing.no_hard_failure_when_servable", "pass",
            f"{turns}/{turns} turns served; attribution {attribution}",
            attribution=attribution, turns=turns,
        )

    peer_served = sum(v for k, v in attribution.items() if k != "local")
    if peer_served:
        finding(
            "routing.peer_reachable", "pass",
            f"{peer_served}/{turns} turns were served BY A PEER — peer routing is live",
            peer_served=peer_served,
        )
    else:
        finding(
            "routing.peer_reachable", "not_observed",
            f"0/{turns} turns were ANSWERED by a peer. This does not mean no peer "
            "was tried — a peer that declines produces a local answer. See "
            "routing.attribution, which reads the decision log and can tell the "
            "two apart.",
        )
    return attribution


# ── P3: admission safety, the invariant MESH_QA already names ──

def probe_admission_safety(samples):
    """peer_inflight ≤ ceiling throughout, → 0 at quiescence.

    Same invariant the multi-process soak asserts over HTTP; the status
    surface carries these fields for exactly this purpose.
    """
    over = [s for s in samples if s["cur"] > s["ceil"]]
    if over:
        finding(
            "admission_safety.within_ceiling", "fail",
            f"peer in-flight exceeded the ceiling {len(over)}x "
            f"(worst {max(s['cur'] for s in over)} > {over[0]['ceil']})",
        )
    else:
        peak = max((s["cur"] for s in samples), default=0)
        finding(
            "admission_safety.within_ceiling", "pass",
            f"peer in-flight stayed within ceiling (peak {peak}/"
            f"{samples[0]['ceil'] if samples else '?'})",
            peak=peak,
        )
    # Quiescence: let anything in flight drain, then require zero.
    for _ in range(20):
        cur = get_json("/v1/mesh/status").get("peer_inflight_current", 0)
        if cur == 0:
            finding("admission_safety.drains_to_zero", "pass",
                    "peer in-flight returned to 0 at quiescence")
            return
        time.sleep(1)
    finding(
        "admission_safety.drains_to_zero", "fail",
        f"peer in-flight stuck at {cur} after 20s of quiet — a leaked counter "
        "makes this peer look permanently busy and mis-ranks it forever",
    )


def sampler(stop, out):
    while not stop.is_set():
        try:
            s = get_json("/v1/mesh/status", timeout=5)
            out.append({"cur": s.get("peer_inflight_current", 0),
                        "ceil": s.get("peer_inflight_ceiling", 0)})
        except Exception:  # noqa: BLE001
            pass
        time.sleep(0.25)


def main():
    global DAEMON
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--turns", type=int, default=4)
    ap.add_argument("--daemon", default=DAEMON,
                    help="daemon base URL. Point at a stub to falsify the probes "
                         "themselves — validate the instrument before the result "
                         "(ARCH_PRINCIPLES §18.4)")
    ap.add_argument("--model", default="primary",
                    help="a model BOTH this node and a peer advertise (default: primary)")
    ap.add_argument("--json", metavar="PATH", help="write findings JSONL here")
    ap.add_argument("--decision-log", metavar="PATH",
                    help="daemon routing-outcome JSONL (default: $SOVEREIGN_DECISION_LOG)")
    args = ap.parse_args()
    DAEMON = args.daemon

    print("mesh-live-probe — routing behaviour against the real fleet\n")
    ctx = preflight()
    if ctx is None:
        print("\nCOULD-NOT-JUDGE — preconditions absent. Nothing was verified.")
        write(args.json)
        return 4

    probe_admission(ctx["me"], args.model)

    samples, stop = [], threading.Event()
    t = threading.Thread(target=sampler, args=(stop, samples), daemon=True)
    t.start()
    log = decision_log_path(args.decision_log)
    log_offset = os.path.getsize(log) if log else 0

    servable = local_advertises(args.model)
    if servable is False:
        finding(
            "routing.precondition", "skipped",
            f"this node does not advertise '{args.model}', so a failed turn would "
            "be a CORRECT refusal rather than the regression this probe hunts — "
            "pass --model with something local",
        )
    else:
        probe_routing(args.model, args.turns)
    stop.set()
    t.join(timeout=2)
    probe_admission_safety(samples)

    if log:
        outcomes, _ = read_outcomes_since(log, log_offset)
        probe_attribution(outcomes)
        probe_shed_handling(outcomes)
    else:
        finding(
            "shed.never_fails_a_servable_caller", "skipped",
            "no decision log found — set SOVEREIGN_DECISION_LOG on the daemon. "
            "Without it a declining peer and an idle one look identical from the "
            "response, so this run cannot speak to the fallback at all",
        )

    write(args.json)
    fails = [f for f in findings if f["verdict"] == "fail"]
    unobserved = [f for f in findings if f["verdict"] == "not_observed"]
    print()
    if fails:
        print(f"FAIL — {len(fails)} invariant(s) violated")
        return 1
    print(f"PASS — {len([f for f in findings if f['verdict'] == 'pass'])} checks"
          + (f", {len(unobserved)} not observed this run" if unobserved else ""))
    return 0


def write(path):
    if not path:
        return
    with open(path, "w") as fh:
        for f in findings:
            fh.write(json.dumps(f) + "\n")
    print(f"\nfindings → {path}")


if __name__ == "__main__":
    sys.exit(main())
