#!/usr/bin/env python3
"""Per-call latency census of one grounding-gate turn — what each model call
the gate issued cost, and which mechanism issued it.

WHY THIS FILE IS IN THE REPO. It was a script on one workstation, and the
finding it produced (below) is the kind a later reader has to be able to
reproduce. An instrument that exists on one box is not an instrument.
Canonical location is HERE; `~/.sovereign/comaintainer/gate-census.py` is a
stub that execs this file.

WHAT IT PRODUCED. On 2026-08-13 this named the gate's dominant cost class,
which a stage-level profile could not see: `scan_unsupported_specifics` at
10,881 ms — 41.0% of a 26,567 ms clean gate — for an output of FOUR
characters. The mechanism is prefill, and the cause is a one-line omission:
the per-claim judges declare `CompletionRequest.stable_prefix_len` and
restore a 5,508-token pinned prefix in ~26 ms; the scan declared none and
re-prefilled everything. See `sovereign/bench/chaos_monkey/results/
gate_call_census_20260813.txt` for the run this text describes.

═══ THE THREE ARMS, AND WHY THEY ARE NOT MERGED ═══

NAMED — the gate's own per-call census, written onto the grounding journal
  line by `sovereign-core/src/runtime/grounding/call_census.rs`. Every row
  carries the `GateCallMechanism` that issued the call. This is an EXACT
  join: the rows belong to this turn by construction, not by timestamp
  proximity. Columns: start offset, duration, mechanism, prompt chars,
  output chars, and the declared `stable_prefix` when there is one.

ROUTED — the daemon's own `routing outcome` lines, matched into the turn's
  wall-clock window. Zero instrumentation, and ANONYMOUS: this arm can see
  a 17.8 s call emitting 4 chars and cannot say who paid for it. Kept
  because it is the INDEPENDENT check on the NAMED arm — a mechanism that
  stops going through the funnel appears here as routed calls with no named
  counterpart. On the 2026-08-13 rewrite turn the two arms agreed 15 == 15
  with per-call durations within ~20 ms, which is what licensed trusting
  the NAMED arm at all.

PIN — `prefix_state` / `prefix_cache` lines, joined to NAMED rows by
  absolute time. This is the arm that says whether a call RESTORED a pinned
  prefix or paid a full prefill, and it is CATEGORICAL where a duration is
  merely suggestive: a call that restored carries
  `prefix_state: HIT … key=<hash> restored_tokens=N restore_ms=M`; a call
  that did not carries no such line at all. `key` is the FAMILY IDENTITY,
  so "did these two mechanisms share a prefix family" is answered by
  comparing keys, not by hoping a number got smaller. That distinction is
  the point: on a box under load a latency delta proves nothing, and the
  prefix-alignment work this instrument serves degrades SILENTLY (a
  mis-declared prefix does not error — it just quietly full-prefills).

═══ INPUTS ═══

  ~/.sovereign/journal/grounding-<date>.jsonl   the gate's decision lines,
      newest file, indexed from the end. NOTE: written only by non-test
      builds (`#[cfg(not(test))]` on the append) — a `cargo test` run used
      to append mock turns to this same stream and shift every index.
  ~/.sovereign/logs/daemon.err                  routing outcomes,
      inference.complete, prefix_state, prefix_cache.

Both are read-only. This script never writes.

═══ USAGE ═══

  gate_call_census.py [index]     index defaults to -1 (most recent turn);
                                  -2, -3 … walk backwards.

Reading it honestly: `calls: []` on a journal line is AMBIGUOUS by
construction — a turn that genuinely made no model call and a build without
the census both produce it. The disambiguator is arithmetic the reader
already has, and this script applies it: an empty census against a
multi-second `gate_ms` is an instrument failure, not a free turn.
"""
import re, os, json, glob, sys
from datetime import datetime, timedelta, timezone

ANSI = re.compile(r'\x1b\[[0-9;]*m')
TS = re.compile(r'^(\d{4}-\d{2}-\d{2}T[\d:]{8}\.\d+)Z')

JOURNAL = '~/.sovereign/journal/grounding-*.jsonl'
DAEMON_ERR = '~/.sovereign/logs/daemon.err'


def daemon_files(win_lo):
    """Every daemon log that could hold lines at or after `win_lo`, oldest first.

    THE LIVE LOG IS NOT ENOUGH, and the failure it caused is why this
    function exists. `daemon.err` rotates at ~10MB; when it does, a turn
    censused an hour earlier loses every daemon-side line, and the PIN arm
    renders `pin=none` for every call — which is INDISTINGUISHABLE from the
    finding it exists to report (a call that really did full-prefill).
    Observed 2026-08-13: a rotation at 21:00Z silently turned a turn with
    34 recorded prefix-cache HITs into a turn that appeared to have none.
    A file whose mtime precedes the window cannot contain the window, so
    the scan is cheap.
    """
    live = os.path.expanduser(DAEMON_ERR)
    out = []
    for path in glob.glob(live + '*'):
        try:
            if os.path.getmtime(path) >= win_lo.replace(tzinfo=timezone.utc).timestamp():
                out.append(path)
        except OSError:
            continue
    return sorted(out, key=os.path.getmtime)


def daemon_lines(win_lo):
    """De-ANSI'd (timestamp, text) pairs, across rotations. Untimed lines skipped."""
    for path in daemon_files(win_lo):
        with open(path, errors='replace') as fh:
            for ln in fh:
                s = ANSI.sub('', ln)
                m = TS.match(s)
                if m:
                    yield datetime.fromisoformat(m.group(1)), s


def main():
    idx = int(sys.argv[1]) if len(sys.argv) > 1 else -1
    journals = sorted(glob.glob(os.path.expanduser(JOURNAL)))
    if not journals:
        sys.exit(f"no grounding journal found at {JOURNAL}")
    with open(journals[-1]) as fh:
        rows = [json.loads(l) for l in fh if l.strip()]
    r = rows[idx]
    end = datetime.fromisoformat(r['ts'].replace('Z', '+00:00')).replace(tzinfo=None)
    start = end - timedelta(milliseconds=r['gate_ms'])
    # Guard the denominator once: a sub-millisecond gate would otherwise
    # divide by zero and hide the rows it did record.
    gate_ms = max(1, r['gate_ms'])
    print(f"turn {r['ts']} surface={r['surface']} verdict={r['verdict']} "
          f"action={r['action']} chunks={r['chunks']} retried={r['retried']} "
          f"gate_ms={r['gate_ms']}")

    # ── collect the daemon-side facts in one pass ──
    win_lo, win_hi = start - timedelta(seconds=5), end + timedelta(seconds=2)
    out_chars, routed, pins, prefills = {}, [], [], []
    daemon_seen = 0
    for t, s in daemon_lines(win_lo):
        if not (win_lo <= t <= win_hi):
            continue
        daemon_seen += 1
        if 'inference.complete' in s:
            m = re.search(r'response_chars=(\d+)', s)
            out_chars[t.strftime('%H:%M:%S.%f')[:11]] = m.group(1) if m else '?'
        elif 'routing outcome' in s:
            d = re.search(r'total_ms=Some\(([0-9.]+)\)', s)
            if d:
                routed.append((t, float(d.group(1)), 'fast' if 'fallback' in s else 'primary'))
        elif 'prefix_state' in s:
            # Four states, not two. A pin that LEARNED is not a pin that HIT,
            # and a restore that FAILED is a different fact again from a call
            # that never had an entry to restore (ARCH §18.1).
            if 'HIT' in s:
                state = 'HIT'
            elif 'LEARNED' in s:
                state = 'LEARN'
            elif 'fail' in s.lower():
                state = 'restore-FAILED'
            else:
                continue
            k = re.search(r'key=([0-9a-f]+)', s)
            n = re.search(r'(?:restored_tokens|pinned_tokens)=(\d+)', s)
            ms = re.search(r'(?:restore_ms|save_ms)=(\d+)', s)
            pins.append((t, state, k.group(1) if k else '?',
                         int(n.group(1)) if n else 0,
                         int(ms.group(1)) if ms else 0))
        elif 'prefix_cache' in s and 'prefill scope' in s:
            n = re.search(r'new_prefill_tokens=(\d+)', s)
            if n:
                prefills.append((t, int(n.group(1))))

    def pin_for(a, b):
        """The pin event inside [a, b], if any. ABSENCE IS THE FINDING: a call
        with no prefix_state line at all paid a full prefill — that is exactly
        the measured shape of the specifics scan."""
        for t, state, key, ntok, ms in pins:
            if a <= t <= b:
                new = next((v for u, v in prefills if a <= u <= b), None)
                return state, key, ntok, ms, new
        return None

    # FOUR VERDICTS, not two (ARCH §18.1). "No daemon line in this window" is
    # could-not-judge for the PIN and ROUTED arms; reporting it as `pin=none`
    # would be reporting the instrument's blindness as a measurement.
    daemon_ok = daemon_seen > 0
    if not daemon_ok:
        print(f"\n!! DAEMON EVIDENCE UNAVAILABLE for this window "
              f"({start:%H:%M:%S}-{end:%H:%M:%S}Z). Searched: "
              f"{', '.join(os.path.basename(p) for p in daemon_files(win_lo)) or 'nothing'}. "
              f"The log has almost certainly rotated past this turn. PIN and ROUTED "
              f"below are COULD-NOT-JUDGE, not zero — do not read `pin=none` as "
              f"'this call full-prefilled'.")

    # ── NAMED ──
    calls = r.get('calls')
    print("\nNAMED (gate call census — mechanism per call, exact join):")
    if calls is None:
        print("  absent: this journal line predates the per-call census")
    elif not calls:
        if r['gate_ms'] > 2000:
            print(f"  EMPTY but gate_ms={r['gate_ms']} — a mechanism ran and recorded "
                  f"nothing. Instrument failure, not a free turn.")
        else:
            print("  no model calls (exempt release, or a deterministic path only)")
    else:
        named_ms = 0
        per_mech_restored, per_mech_full = {}, {}
        for i, c in enumerate(calls, 1):
            cs = start + timedelta(milliseconds=c['start_offset_ms'])
            ce = cs + timedelta(milliseconds=c['ms'])
            pin = pin_for(cs - timedelta(milliseconds=200), ce)
            if pin:
                state, key, ntok, rms, newtok = pin
                newtxt = f" new={newtok}" if newtok is not None else ""
                pintxt = f" pin={state} key={key[:8]} restored={ntok} in {rms}ms{newtxt}"
                per_mech_restored[c['mechanism']] = (
                    per_mech_restored.get(c['mechanism'], 0) + ntok)
            elif not daemon_ok:
                pintxt = "  pin=? — no daemon evidence (COULD-NOT-JUDGE)"
            else:
                pintxt = "  pin=none — FULL PREFILL"
                per_mech_full[c['mechanism']] = per_mech_full.get(c['mechanism'], 0) + 1
            decl = c.get('stable_prefix_bytes')
            decl = f" declared={decl}B" if decl is not None else ""
            bad = "" if c.get('ok', True) else "  <<ERR"
            print(f"  {i:>3} +{c['start_offset_ms']:>6}ms {c['ms']:>8}ms "
                  f"{c['mechanism']:<18} in={c['prompt_chars']:>7}ch "
                  f"out={c['out_chars']:>6}ch{decl}{pintxt}{bad}")
            named_ms += c['ms']
        agg = {}
        for c in calls:
            n, ms, inch = agg.get(c['mechanism'], (0, 0, 0))
            agg[c['mechanism']] = (n + 1, ms + c['ms'], inch + c['prompt_chars'])
        print("  --- by mechanism (share of gate wall clock) ---")
        for m, (n, ms, inch) in sorted(agg.items(), key=lambda kv: -kv[1][1]):
            print(f"  {m:<18} n={n:<3} {ms:>8}ms  {100*ms/gate_ms:>5.1f}%  "
                  f"prefill={inch:>8}ch")
        print(f"  named_ms={named_ms}  gate_ms={r['gate_ms']}  "
              f"unattributed_ms={r['gate_ms'] - named_ms} "
              f"({100*named_ms/gate_ms:.1f}% of the gate is model calls)")
        if not daemon_ok:
            print("  --- pin per mechanism: COULD-NOT-JUDGE (no daemon evidence) ---")
        else:
            print("  --- pin per mechanism (restored tokens vs calls that full-prefilled) ---")
        for m in (sorted(agg) if daemon_ok else []):
            print(f"  {m:<18} restored={per_mech_restored.get(m, 0):>7}tok  "
                  f"full_prefill_calls={per_mech_full.get(m, 0)}")
        keys = sorted({p[2] for p in pins})
        print(f"  --- pin families in this turn: {len(keys)} {keys} ---")
        print("      Two mechanisms share a family iff they share a key. That is the "
              "pass condition for any prefix-alignment change — not a faster number, "
              "which load can fake in either direction.")

    # ── ROUTED ──
    print("\nROUTED (daemon routing outcomes in the turn window — anonymous):")
    n = busy = 0
    for t, ms, slot in sorted(routed):
        if not (start <= t <= end + timedelta(milliseconds=300)):
            continue
        st = t - timedelta(milliseconds=ms)
        ch = (out_chars.get(t.strftime('%H:%M:%S.%f')[:11])
              or out_chars.get((t - timedelta(milliseconds=1)).strftime('%H:%M:%S.%f')[:11])
              or '?')
        n += 1
        if slot == 'primary':
            busy += ms
        print(f"  {n:>3} {st.strftime('%H:%M:%S.%f')[:12]}->"
              f"{t.strftime('%H:%M:%S.%f')[:12]} {ms:>8.0f}ms {slot:<7} out_chars={ch}")
    print(f"  calls={n}  primary_busy_ms={busy:.0f}  gate_ms={r['gate_ms']}  "
          f"serial_coverage={100*busy/gate_ms:.1f}%")

    # ── cross-check ──
    if calls:
        if n != len(calls):
            print(f"\nCROSS-CHECK: {len(calls)} named vs {n} routed. A gap means either "
                  f"a call escaped the census funnel, or the routed window caught "
                  f"non-gate traffic (retrieval and the claim-class classifier both "
                  f"issue calls inside the gate's wall clock). Read both arms before "
                  f"trusting either.")
        else:
            print(f"\nCROSS-CHECK: {len(calls)} named == {n} routed.")


if __name__ == '__main__':
    main()
