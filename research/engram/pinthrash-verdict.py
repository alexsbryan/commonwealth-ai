#!/usr/bin/env python3
"""Apply the PRE-REGISTERED pin-thrash bars (PRE-REGISTRATION-pinthrash.md).
Usage: pinthrash-verdict.py "<journal-since>" "<journal-until>" [facts_strict]"""
import subprocess, sys, re, collections
since, until = sys.argv[1], sys.argv[2]
facts = sys.argv[3] if len(sys.argv) > 3 else None
out = subprocess.run(["journalctl","--since",since,"--until",until,"--no-pager"],
                     capture_output=True, text=True).stdout
pat = re.compile(r'prefix_state: (LEARNED|HIT).*?key=([0-9a-f]+) (?:pinned|restored)_tokens=(\d+)')
seq, learns, hits = collections.defaultdict(list), collections.Counter(), collections.Counter()
for kind, key, n in pat.findall(out):
    if kind == "LEARNED": learns[key] += 1; seq[key].append(int(n))
    else: hits[key] += 1
if not learns and not hits:
    print("VOID — no prefix_state events in the window"); sys.exit(3)
print(f"{'key':18} {'LEARN':>6} {'HIT':>5}  pin sequence")
repeaters = []
for k in sorted(set(list(learns)+list(hits)), key=lambda k: -learns[k]):
    s = seq[k]
    dup = len(s) != len(set(s))
    if dup: repeaters.append(k)
    print(f"{k:18} {learns[k]:>6} {hits[k]:>5}  {s}{'   <-- REPEATED PIN (thrash)' if dup else ''}")
L, H = sum(learns.values()), sum(hits.values())
print(f"\ntotal LEARN={L} (baseline 19)   HIT={H} (baseline 25)   keys with a repeated pin: {len(repeaters)} (baseline 5)")
if facts:
    print(f"facts strict: {facts} (bar: 34/35 +/- 1 — OUTRANKS everything below)")
if repeaters:
    v = f"NO-GO / PARTIAL — {len(repeaters)} key(s) still repeat a pin size: {repeaters}. The structural signature of thrash survives."
elif L <= 10 and H >= 30:
    v = f"WIN — no key repeats a pin size; LEARN {L}<=10, HIT {H}>=30. Thrash eliminated."
else:
    v = f"PARTIAL — no key repeats a pin (the structural bar PASSES), but LEARN={L} (bar <=10) / HIT={H} (bar >=30) missed. Report both."
print("\nVERDICT: " + v)
