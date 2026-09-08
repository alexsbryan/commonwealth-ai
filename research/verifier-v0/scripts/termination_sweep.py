#!/usr/bin/env python3
"""Does the refinement loop TERMINATE, or does it spin on a stuck population?

§10 measured one pass: the loop is safe (p_damage 1.64%) but 41.5% of revisions
are still flagged afterwards. That leaves the whole "more passes -> better"
story resting on an unmeasured question -- whether the churn decays or has a
floor. This iterates the loop and watches the flagged set.

THE TWO HYPOTHESES, and they predict different numbers
  GEOMETRIC: each pass clears a roughly constant fraction of whatever is still
    flagged. Pass 1 cleared 108/188 = 57.4%. If that rate holds, the flagged
    set goes 193 -> 80 -> 34 -> 15 -> 6, expected passes to termination ~1.7,
    thin tail. Depth works: buy passes, get quality.
  STUCK FLOOR: the easy items clear first and what remains is what the verifier
    structurally refuses. Clearance falls pass over pass and the flagged set
    converges to a non-zero floor. No amount of compute clears it, and the
    depth story hits a wall whose height is that floor.

PRE-REGISTERED BARS (written before pass 2 was run)
  Primary: pass-2 clearance rate, n=80, so a 95% CI of about +/-11pt.
    within [40%, 75%]  -> consistent with geometric; depth holds so far.
    below 40%          -> decay is slowing; a stuck population is forming and
                          its size is the wall. Report the floor, do not retry.
    (A rate ABOVE 75% would also be a miss -- it would mean pass 1 was
     unrepresentative -- and is reported as such rather than celebrated.)
  Secondary, and a distinct risk: CUMULATIVE DRIFT. Per-pass damage being 1.64%
    does not bound damage after four passes. The witness is applied at every
    pass against the ORIGINAL grounded claim, not the previous revision, so
    drift accumulates in the measurement the way it would in the answer.
    Bar: cumulative damage at the last pass must stay under 10%. Above that,
    depth is unsafe even if it terminates, and §10's per-pass reading is a
    floor that does not extrapolate.

Everything is in ONE run against ONE server, and tau is derived IN-RUN from a
fresh calibration sample -- §9's stored tau is a max over a chunk prefix and
does not transfer (judge_revisions.py header).

TWO PHASES, because the box is shared. `--phase generate` needs only the
daemon's already-resident 4B fast slot and adds no memory; `--phase judge`
needs rung-1000 served (4.6GB) and waits for room. Generate walks EVERY claim
through every pass regardless of whether it cleared -- a superset that CONTAINS
the real loop path exactly, since the real loop's pass-k text for a
still-flagged item is precisely the k-th revision. Items that clear early are
truncated at judge time, so the decoupling costs nothing but some spare
generation.
"""
import argparse, json, random, statistics, sys, threading
import concurrent.futures as cf
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT/"scripts"))
from measure_delta import load_items, revise
from judge_revisions import score_one
from delta_witness import witness as damage_witness

def generate(a):
    """Phase 1: walk every claim through `passes` revisions on the daemon's fast
    slot. No judge, no extra memory. Errors are recorded, never skipped."""
    items, _ = load_items(a.fa)
    state = {it["id"]: {"orig": it["claim"], "cur": it["claim"], "ev": it["evidence_chunks"],
                        "q": it["question"], "kind": it["kind"]} for it in items}
    rows = []
    for p in range(1, a.passes + 1):
        print(f"=== generate pass {p}: {len(state)} claims ===", flush=True)
        def step(iid):
            s = state[iid]
            it = {"claim": s["cur"], "question": s["q"], "evidence_chunks": s["ev"]}
            try:
                return iid, revise(it, a.gen_url, a.gen_model), None
            except Exception as e:
                return iid, None, str(e)[:150]
        done = [0]
        with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
            for iid, rev, err in ex.map(step, list(state)):
                done[0] += 1
                if done[0] % 40 == 0:
                    print(f"  {done[0]}/{len(state)}", flush=True)
                if err:
                    rows.append({"id": iid, "pass": p, "revised": None, "error": err})
                    continue
                state[iid]["cur"] = rev
                w = damage_witness(rev, state[iid]["orig"], state[iid]["ev"])
                rows.append({"id": iid, "pass": p, "kind": state[iid]["kind"],
                             "revised": rev, "witness": w})
        nerr = sum(1 for r in rows if r["pass"] == p and r.get("error"))
        print(f"  pass {p} done ({nerr} errors)", flush=True)
    Path(a.traj).parent.mkdir(parents=True, exist_ok=True)
    with open(a.traj, "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    print(f"wrote {a.traj} ({len(rows)} rows)")
    return 0


def judge_phase(a):
    """Phase 2: score every revision in the trajectory, derive tau in-run, then
    reconstruct the REAL loop post-hoc -- an item stays active only while it is
    still flagged, so passes after it clears are discarded, not counted."""
    traj = [json.loads(l) for l in open(a.traj) if l.strip()]
    bk = {json.loads(l)["id"]: json.loads(l) for l in open(ROOT/"data/heldout-sep/bank.jsonl") if l.strip()}
    sc = {json.loads(l)["id"]: json.loads(l) for l in open(ROOT/"runs/headroom/scored.jsonl") if l.strip()}
    MH = "multi_hop_conjunction"
    rng = random.Random(a.seed)
    gpool = [i for i in sc if sc[i]["label"] == "grounded" and sc[i]["kind"] != MH]
    cal = rng.sample(gpool, min(a.n_cal, len(gpool)))
    J = dict(judge_url=a.judge_url, judge_model=a.judge_model,
             embed_url=a.embed_url, embed_model=a.embed_model)

    with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
        cms = [m for m in ex.map(lambda i: score_one(bk[i]["claim"], bk[i]["evidence_chunks"], **J)["margin"], cal)
               if m is not None]
    tau = sorted(cms)[max(0, int(round(a.fa*len(cms)))-1)]
    print(f"in-run tau = {tau:+.4f} from {len(cms)} calibration claims "
          f"(§10 measured +15.629)", flush=True)

    todo = [r for r in traj if r.get("revised")]
    print(f"judging {len(todo)} revisions", flush=True)
    done = [0]
    def sc_one(r):
        m = score_one(r["revised"], bk[r["id"]]["evidence_chunks"], **J)["margin"]
        done[0] += 1
        if done[0] % 100 == 0: print(f"  {done[0]}/{len(todo)}", flush=True)
        return {**r, "margin": m, "flagged": (m is not None and m <= tau)}
    with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
        scored = list(ex.map(sc_one, todo))
    with open(a.out, "w") as f:
        for r in scored: f.write(json.dumps(r) + "\n")

    byid = {}
    for r in scored: byid.setdefault(r["id"], {})[r["pass"]] = r
    active = sorted(byid)
    summary = []
    for p in range(1, a.passes + 1):
        if not active: break
        cleared, still, unk, dmg, chk = 0, [], 0, 0, 0
        for iid in active:
            r = byid[iid].get(p)
            if r is None or r["margin"] is None:
                unk += 1; still.append(iid); continue   # never counted as cleared (§18.3)
            w = r.get("witness") or {}
            if w.get("checkable"):
                chk += 1
                if w.get("damaged"): dmg += 1
            if r["flagged"]: still.append(iid)
            else: cleared += 1
        n = len(active)
        summary.append({"pass": p, "in": n, "cleared": cleared, "still_flagged": len(still),
                        "clearance_rate": round(cleared/n, 4), "unscorable": unk,
                        "cumulative_damaged": dmg, "checkable": chk,
                        "cum_damage_rate": round(dmg/chk, 4) if chk else None})
        active = still
    Path(a.out).with_name("termination_summary.json").write_text(
        json.dumps({"tau": tau, "fa": a.fa, "passes": summary}, indent=2))
    print("\n=== per-pass clearance: the geometric-vs-floor test ===")
    print(f"{'pass':>4} {'in':>5} {'cleared':>8} {'rate':>7} {'still':>6} {'cum-damage':>12}")
    for s_ in summary:
        cd = f"{s_['cumulative_damaged']}/{s_['checkable']}" if s_["checkable"] else "n/a"
        print(f"{s_['pass']:>4} {s_['in']:>5} {s_['cleared']:>8} {s_['clearance_rate']:>7.1%} "
              f"{s_['still_flagged']:>6} {cd:>12}")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fa", type=float, default=0.20)
    ap.add_argument("--passes", type=int, default=5)
    ap.add_argument("--n-cal", type=int, default=150)
    ap.add_argument("--judge-url", default="http://127.0.0.1:8089/v1")
    ap.add_argument("--judge-model", default="rung-1000")
    ap.add_argument("--gen-url", default="http://127.0.0.1:9741/v1")
    ap.add_argument("--gen-model", default="Qwen3.5-4B-UD-MTP-Q6_K_XL")
    ap.add_argument("--embed-url", default="http://127.0.0.1:9741/v1")
    ap.add_argument("--embed-model", default="Qwen3-Embedding-0.6B-Q8_0")
    ap.add_argument("--concurrency", type=int, default=4)
    ap.add_argument("--seed", type=int, default=17)
    ap.add_argument("--out", default=str(ROOT/"runs/delta/termination.jsonl"))
    ap.add_argument("--phase", choices=["generate", "judge", "both"], default="both")
    ap.add_argument("--traj", default=str(ROOT/"runs/delta/trajectory.jsonl"))
    a = ap.parse_args()

    if a.phase == "generate":
        return generate(a)
    if a.phase == "judge":
        return judge_phase(a)

    items, _ = load_items(a.fa)
    bk = {json.loads(l)["id"]: json.loads(l) for l in open(ROOT/"data/heldout-sep/bank.jsonl") if l.strip()}
    sc = {json.loads(l)["id"]: json.loads(l) for l in open(ROOT/"runs/headroom/scored.jsonl") if l.strip()}
    MH = "multi_hop_conjunction"
    rng = random.Random(a.seed)
    gpool = [i for i in sc if sc[i]["label"] == "grounded" and sc[i]["kind"] != MH]
    cal = rng.sample(gpool, min(a.n_cal, len(gpool)))

    J = dict(judge_url=a.judge_url, judge_model=a.judge_model,
             embed_url=a.embed_url, embed_model=a.embed_model)
    lock = threading.Lock()

    # ---- tau, in-run -------------------------------------------------------
    def cal_one(i):
        return score_one(bk[i]["claim"], bk[i]["evidence_chunks"], **J)["margin"]
    with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
        cms = [m for m in ex.map(cal_one, cal) if m is not None]
    tau = sorted(cms)[max(0, int(round(a.fa*len(cms)))-1)]
    print(f"in-run tau = {tau:+.4f} from {len(cms)} calibration claims "
          f"(§10 measured +15.629; a large gap means the instrument moved)", flush=True)

    # ---- the loop ----------------------------------------------------------
    state = {it["id"]: {"orig": it["claim"], "cur": it["claim"],
                        "ev": it["evidence_chunks"], "q": it["question"],
                        "kind": it["kind"], "flagged": True, "history": []}
             for it in items}
    active = list(state)
    rows, summary = [], []
    for p in range(1, a.passes+1):
        if not active:
            print(f"pass {p}: nothing flagged; loop TERMINATED", flush=True); break
        print(f"\n=== pass {p}: {len(active)} flagged claims ===", flush=True)

        def step(iid):
            s = state[iid]
            it = {"claim": s["cur"], "question": s["q"], "evidence_chunks": s["ev"]}
            try:
                rev = revise(it, a.gen_url, a.gen_model)
            except Exception as e:
                return iid, None, None, str(e)[:150]
            r = score_one(rev, s["ev"], **J)
            return iid, rev, r["margin"], None

        done = [0]
        results = []
        with cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
            for iid, rev, margin, err in ex.map(step, active):
                with lock:
                    done[0] += 1
                    if done[0] % 40 == 0: print(f"  {done[0]}/{len(active)}", flush=True)
                results.append((iid, rev, margin, err))

        cleared, still, errs, dmg = 0, [], 0, 0
        for iid, rev, margin, err in results:
            s = state[iid]
            if err or margin is None:
                errs += 1                       # errors never counted as cleared (§18.3)
                still.append(iid); continue
            s["cur"] = rev
            # cumulative drift: witness against the ORIGINAL grounded claim
            w = damage_witness(rev, s["orig"], s["ev"])
            s["history"].append({"pass": p, "margin": margin, "flagged": margin <= tau,
                                 "damaged_vs_original": w["damaged"],
                                 "checkable": w["checkable"], "absent": w["absent"]})
            if w["checkable"] and w["damaged"]: dmg += 1
            if margin <= tau: still.append(iid)
            else: cleared += 1
            rows.append({"id": iid, "pass": p, "kind": s["kind"], "revised": rev,
                         "margin": margin, "flagged": margin <= tau, "witness": w})
        n = len(active)
        chk = sum(1 for iid, rev, m, e in results if not e and m is not None
                  and damage_witness(state[iid]["cur"], state[iid]["orig"], state[iid]["ev"])["checkable"])
        rate = cleared/n if n else float("nan")
        summary.append({"pass": p, "in": n, "cleared": cleared, "still_flagged": len(still),
                        "clearance_rate": round(rate, 4), "errors": errs,
                        "cumulative_damaged": dmg, "checkable": chk})
        print(f"  cleared {cleared}/{n} = {rate:.1%}   still flagged {len(still)}   "
              f"errors {errs}   cumulative-damaged {dmg}/{chk}", flush=True)
        active = still

    Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    with open(a.out, "w") as f:
        for r in rows: f.write(json.dumps(r)+"\n")
    sp = Path(a.out).with_name("termination_summary.json")
    sp.write_text(json.dumps({"tau": tau, "fa": a.fa, "passes": summary}, indent=2))
    print(f"\nwrote {a.out} and {sp}")
    print("\n=== per-pass clearance (the geometric-vs-floor test) ===")
    for s in summary:
        print(f"  pass {s['pass']}: in {s['in']:4d}  cleared {s['cleared']:4d} "
              f"({s['clearance_rate']:.1%})  still {s['still_flagged']:4d}  "
              f"cum-damaged {s['cumulative_damaged']}/{s['checkable']}")

if __name__ == "__main__":
    sys.exit(main())
