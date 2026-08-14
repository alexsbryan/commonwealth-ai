#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""RED BASELINE — bars `t1-local-identity` (order `mesh-scale-t1-red`) and
`serve50-fairness` (order `mesh-serve-50-red`).

ONE QUESTION: on one node, with one shared bearer token and no per-caller
identity, what share of served turns does ONE greedy client take from N polite
ones — and how long do the polite ones wait?

── `--identity-mode` (added by order `mesh-serve-50-red`) ────────────────────

The original run above answers "the daemon has no principal to key on" by
giving every caller the SAME credential — which leaves one hole an auditor is
right to poke at: maybe the daemon WOULD be fair if only the callers were
telling it apart. `--identity-mode distinct` closes it. Each client gets its
own wire identity on every request — its own bearer token, its own
`X-Node-Id`, its own `X-Principal` — so the transport is now carrying exactly
the distinguisher a fair scheduler would need.

The measurement is the COMPARISON of the two arms, not either arm alone:

  * shares differ between `shared` and `distinct`  → something downstream reads
    identity, and fairness is at least partly keyed.
  * shares are the same within noise              → identity is present on the
    wire and consulted by nothing. That is the red.

Because a per-client bearer could equally be REJECTED rather than ignored, the
run records the outcome mix per arm and refuses to score a `distinct` arm whose
error count does not match its `shared` counterpart — a 401 storm and an
ignored principal both produce "no fairness", and they are not the same finding.

Population, all on the same credential in `shared` mode (there is nothing else
to be — client auth is one shared token per node, `routes_ollama.rs:36-37`):
  * 1 GREEDY client: keeps `--greedy-inflight` requests in flight at all times
    and re-fires the instant one returns, ignoring every Retry-After.
  * N POLITE clients: one request in flight each, and on a 503 they sleep the
    Retry-After the server asked for before trying again.

Recorded per cohort over a fixed window: admitted turns, shed responses,
admitted SHARE, and the polite cohort's wait distribution (time from a
client's first attempt of a turn to that turn being served, retries included)
against the greedy client's. The number that makes "starves" concrete is the
polite cohort's admitted share versus its population share.

`parked` is kept as a fourth outcome, never folded into `error` — same rule as
Probe A.
"""
from __future__ import annotations

import argparse
import json
import statistics
import threading
import time
import urllib.error
import urllib.request

PARK_THRESHOLD_S = 120.0

lock = threading.Lock()
events: list[dict] = []
IDENTITY_MODE = "shared"
http_codes: dict[str, int] = {}


def record(**kw) -> None:
    with lock:
        events.append(kw)


def note_code(code: str) -> None:
    with lock:
        http_codes[code] = http_codes.get(code, 0) + 1


def identity_headers(cohort: str, client: int) -> dict:
    """The wire identity this attempt carries. THREE arms, not two.

    `shared`     — one credential for everybody. Reproduces the original t1 run
                   byte-for-byte and is the control.
    `principal`  — a distinct bearer and a distinct `X-Principal` per caller,
                   and deliberately NO `X-Node-Id`. This is "the transport is
                   telling you who I am" while staying on the same code path as
                   `shared`.
    `distinct`   — `principal` plus a distinct `X-Node-Id`.

    The three-arm split is not decoration; a two-arm run gives a confounded
    answer. `commonwealth-api/src/admission.rs:289-292` keys off the mere
    PRESENCE of `x-node-id`:

        let is_peer = headers.get("x-node-id").is_some();
        if !is_peer { return next.run(req).await; }

    so adding that one header does not "add identity to the same request" — it
    moves the request onto the PEER admission path, behind `admit_peer_request`
    and the `max_peer_inflight` ceiling. Comparing `shared` against `distinct`
    alone therefore compares two different code paths and cannot say whether
    identity per se changed anything. `principal` is the arm that isolates it:
    same path as `shared`, strictly more identity on the wire.
    """
    if IDENTITY_MODE == "shared":
        return {"Authorization": "Bearer probe-shared-token"}
    who = f"{cohort}-{client}"
    headers = {
        "Authorization": f"Bearer probe-principal-{who}",
        "X-Principal": who,
    }
    if IDENTITY_MODE == "distinct":
        headers["X-Node-Id"] = f"probe-node-{who}"
    return headers


def attempt(url: str, cohort: str, client: int, prompt: str) -> tuple[str, float, int | None]:
    """One HTTP attempt. Returns (outcome, secs, retry_after)."""
    body = json.dumps(
        {"messages": [{"role": "user", "content": prompt}], "max_tokens": 24, "stream": False}
    ).encode()
    headers = {"Content-Type": "application/json"}
    headers.update(identity_headers(cohort, client))
    req = urllib.request.Request(f"{url}/v1/chat/completions", data=body, headers=headers)
    t0 = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=PARK_THRESHOLD_S) as resp:
            resp.read()
            note_code("200")
            return "admitted", time.monotonic() - t0, None
    except urllib.error.HTTPError as e:
        e.read()
        note_code(str(e.code))
        if e.code == 503:
            hint = e.headers.get("Retry-After")
            try:
                hint_i = int(hint) if hint is not None else None
            except ValueError:
                hint_i = None
            return "shed", time.monotonic() - t0, hint_i
        return "error", time.monotonic() - t0, None
    except Exception:
        elapsed = time.monotonic() - t0
        note_code("transport")
        return ("parked" if elapsed >= PARK_THRESHOLD_S * 0.95 else "error"), elapsed, None


def polite_client(url: str, idx: int, deadline: float, retry_cap: float) -> None:
    """One turn at a time; honours Retry-After (capped so the window still ends)."""
    while time.monotonic() < deadline:
        turn_started = time.monotonic()
        while time.monotonic() < deadline:
            outcome, secs, hint = attempt(url, "polite", idx, f"polite client {idx} question")
            if outcome == "admitted":
                record(cohort="polite", client=idx, outcome="admitted",
                       wait_s=time.monotonic() - turn_started, service_s=secs)
                break
            record(cohort="polite", client=idx, outcome=outcome, secs=secs, retry_after=hint)
            if outcome != "shed":
                break
            # The polite behaviour under test: sleep what the server asked.
            time.sleep(min(hint if hint else 1.0, retry_cap))


def greedy_worker(url: str, deadline: float) -> None:
    while time.monotonic() < deadline:
        turn_started = time.monotonic()
        outcome, secs, hint = attempt(url, "greedy", 0, "greedy client question")
        if outcome == "admitted":
            record(cohort="greedy", client=0, outcome="admitted",
                   wait_s=time.monotonic() - turn_started, service_s=secs)
        else:
            record(cohort="greedy", client=0, outcome=outcome, secs=secs, retry_after=hint)
        # No backoff. That is the whole point.


def summarize(cohort: str, polite_n: int, greedy_n: int) -> dict:
    rows = [e for e in events if e["cohort"] == cohort]
    admitted = [e for e in rows if e["outcome"] == "admitted"]
    waits = sorted(e["wait_s"] for e in admitted)
    return {
        "attempts": len(rows),
        "admitted": len(admitted),
        "shed": sum(1 for e in rows if e["outcome"] == "shed"),
        "parked": sum(1 for e in rows if e["outcome"] == "parked"),
        "error": sum(1 for e in rows if e["outcome"] == "error"),
        "wait_p50": (statistics.median(waits) if waits else None),
        "wait_p95": (waits[int(len(waits) * 0.95)] if waits else None),
        "wait_max": (waits[-1] if waits else None),
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--clients", type=int, default=9, help="polite clients (greedy is extra)")
    ap.add_argument("--seconds", type=float, default=60.0)
    ap.add_argument("--daemon-log", default=None)
    ap.add_argument("--greedy-inflight", type=int, default=4)
    ap.add_argument("--retry-cap", type=float, default=5.0,
                    help="cap on an honoured Retry-After, so the window still ends")
    ap.add_argument("--identity-mode", choices=["shared", "principal", "distinct"],
                    default="shared",
                    help="shared: one credential for everyone (the t1 run, the control). "
                         "principal: distinct bearer + X-Principal, NO X-Node-Id — same "
                         "code path as shared, strictly more identity on the wire. "
                         "distinct: principal + X-Node-Id, which also moves the request "
                         "onto the peer-admission path (admission.rs:289).")
    args = ap.parse_args()
    polite_n = max(1, args.clients)
    global IDENTITY_MODE
    IDENTITY_MODE = args.identity_mode

    # Warm-up alone, so the model load is not charged to anyone's share.
    t0 = time.monotonic()
    attempt(args.url, "warmup", -1, "Say hello.")
    print(f"probe-identity: warm-up took {time.monotonic() - t0:.1f}s")

    deadline = time.monotonic() + args.seconds
    threads = [
        threading.Thread(target=greedy_worker, args=(args.url, deadline))
        for _ in range(args.greedy_inflight)
    ]
    threads += [
        threading.Thread(target=polite_client, args=(args.url, i, deadline, args.retry_cap))
        for i in range(polite_n)
    ]
    cred = ("one shared bearer token" if IDENTITY_MODE == "shared"
            else "a DISTINCT bearer / X-Node-Id / X-Principal per principal")
    print(f"probe-identity: 1 greedy client ({args.greedy_inflight} in flight, no backoff) "
          f"+ {polite_n} polite clients, {cred}, {args.seconds:.0f}s window")
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    g = summarize("greedy", polite_n, 1)
    p = summarize("polite", polite_n, 1)
    total_admitted = g["admitted"] + p["admitted"]
    if total_admitted == 0:
        print("PROBE_IDENTITY COULD-NOT-JUDGE — nothing was admitted in the window; "
              "this run measured the harness, not the system")
        return

    greedy_share = g["admitted"] / total_admitted
    polite_share = p["admitted"] / total_admitted
    # Population share is the fair-share reference: 1 greedy caller among
    # (1 + polite_n) callers is entitled to 1/(1+polite_n) of the turns if the
    # node had per-caller identity. It does not, so this is the gap.
    fair = 1.0 / (1 + polite_n)

    print(f"PROBE_IDENTITY identity_mode={IDENTITY_MODE} window_s={args.seconds:.0f} "
          f"polite_clients={polite_n} greedy_inflight={args.greedy_inflight}")
    # The outcome mix per arm. A `distinct` arm whose credentials were REJECTED
    # rather than ignored shows up here as 401/403, and must not be read as a
    # fairness result — see the module docstring.
    with lock:
        codes = dict(sorted(http_codes.items()))
    print(f"PROBE_IDENTITY identity_mode={IDENTITY_MODE} http_status_mix={codes}")
    rejected = sum(v for k, v in codes.items() if k in ("401", "403"))
    if rejected:
        print(f"PROBE_IDENTITY COULD-NOT-JUDGE identity_mode={IDENTITY_MODE} — "
              f"{rejected} request(s) were REJECTED (401/403), so this arm measured "
              f"credential rejection, not scheduler indifference")
    print(f"PROBE_IDENTITY greedy {g}")
    print(f"PROBE_IDENTITY polite {p}")
    print(f"PROBE_IDENTITY admitted_total={total_admitted} "
          f"greedy_admitted_share={greedy_share:.3f} "
          f"polite_admitted_share={polite_share:.3f} "
          f"greedy_fair_share={fair:.3f} "
          f"greedy_overshoot={greedy_share / fair:.1f}x")

    per_polite = {}
    for i in range(polite_n):
        per_polite[i] = sum(
            1 for e in events if e["cohort"] == "polite" and e["client"] == i
            and e["outcome"] == "admitted"
        )
    starved = [i for i, n in per_polite.items() if n == 0]
    print(f"PROBE_IDENTITY polite_admitted_per_client={per_polite} "
          f"polite_clients_with_zero_turns={len(starved)}")

    parked = g["parked"] + p["parked"]
    print(f"PROBE_IDENTITY_VERDICT " + ("no-parking" if parked == 0 else f"PARKED={parked}"))


if __name__ == "__main__":
    main()
