#!/usr/bin/env python3
"""Desktop-bridge arm driver: N warm desktop turns of one question via the
command bridge (:9745), fresh conversation per turn, one jsonl row per turn.
Seat-run instrument (orders gate-tombstone-ladder / drafter-attribution-
discipline / judge-calibration-replay 2026-08-13; audit-economy ladder-shadow
2026-08-14). Modeled byte-for-byte on
sovereign-cli-llm/src/bench_cmd/desktop_bridge.rs::run_bridge_live.

VERSIONED 2026-08-14 out of the gitignored runs/ tree, where three arms' worth
of instrument was one `rm -rf` from gone. Pair it with capture_shadow_rows.sh
when the arm reads tracing rows out of a log — that script exists because the
capture step has a silent-empty failure mode; read its header before writing a
new arm that greps logs.

Turn 0 is a warmup and is excluded by analysis, but it must still COMPLETE.
Requires the desktop bridge: SOVEREIGN_COMMAND_BRIDGE=1, or there is no
instrument."""
import json, sys, time, urllib.request, urllib.error, datetime

BRIDGE = "http://127.0.0.1:9745"
SPEC = "seat-portfolio-baseline"
QUESTION = "Is free will compatible with determinism?"
TURNS = int(sys.argv[1]) if len(sys.argv) > 1 else 21   # turn 0 = warmup, excluded by analysis
OUT = sys.argv[2] if len(sys.argv) > 2 else "sovereign/bench/chaos_monkey/results/ewalltime_desktop_20260814_portfolio_baseline.jsonl"

def req(method, path, body=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(BRIDGE + path, data=data, method=method,
                               headers={"content-type": "application/json", **(headers or {})})
    with urllib.request.urlopen(r, timeout=30) as resp:
        return json.loads(resp.read())

def invoke(cmd, args):
    body = req("POST", "/invoke", {"cmd": cmd, "args": args}, {"x-sovereign-spec": SPEC})
    if body.get("ok") is not True:
        raise RuntimeError(f"invoke {cmd} failed: {body.get('error')}")
    return body.get("result")

def events_since(seq):
    return req("GET", f"/events/recent?since_seq={seq}").get("rows", [])

def one_turn(i, out):
    conv = invoke("create_conversation", {})
    conv_id = conv["id"]
    since = (events_since(0) or [{"seq": -1}])[-1]["seq"] + 1
    t0 = time.time()
    started = invoke("send_message_stream", {"message": QUESTION, "conversationId": conv_id})
    mid = started["message_id"]
    deadline = t0 + 360
    while True:
        for row in events_since(since):
            if row.get("event") == "message-complete" and row.get("payload", {}).get("message_id") == mid:
                wall = round(time.time() - t0, 1)
                rec = {"turn": i, "conversation_id": conv_id, "wall_s": wall,
                       "ts": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                       "payload": row["payload"]}
                out.write(json.dumps(rec) + "\n"); out.flush()
                gate = row["payload"].get("metadata", {}).get("grounding_gate", {})
                print(f"turn {i}: wall {wall}s action={gate.get('action')}", flush=True)
                return True
            if row.get("event") == "message-error" and row.get("payload", {}).get("message_id") == mid:
                out.write(json.dumps({"turn": i, "conversation_id": conv_id, "error": row["payload"].get("message"),
                                      "ts": datetime.datetime.now(datetime.timezone.utc).isoformat()}) + "\n"); out.flush()
                print(f"turn {i}: ERROR {row['payload'].get('message')}", flush=True)
                return False
        if time.time() > deadline:
            out.write(json.dumps({"turn": i, "conversation_id": conv_id, "error": "timeout 360s",
                                  "ts": datetime.datetime.now(datetime.timezone.utc).isoformat()}) + "\n"); out.flush()
            print(f"turn {i}: TIMEOUT", flush=True)
            return False
        time.sleep(1)

def main():
    req("GET", "/healthz")
    req("POST", "/listen", {"event": "message-complete"})
    req("POST", "/listen", {"event": "message-error"})
    with open(OUT, "a") as out:
        for i in range(TURNS):
            try:
                one_turn(i, out)
            except Exception as e:
                print(f"turn {i}: EXCEPTION {e}", flush=True)
                with open(OUT, "a") as o2:
                    o2.write(json.dumps({"turn": i, "error": str(e),
                                         "ts": datetime.datetime.now(datetime.timezone.utc).isoformat()}) + "\n")
                time.sleep(10)
            time.sleep(5)
    print("ALL TURNS DONE", flush=True)

if __name__ == "__main__":
    main()
