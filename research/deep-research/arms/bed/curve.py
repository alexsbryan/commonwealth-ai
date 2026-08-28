# SPDX-License-Identifier: AGPL-3.0-or-later
"""Render the section-evidence-budget curve.

Sorted by passages so the SHAPE is readable, which is the whole question the
sweep asks. An arm with no score prints n/a, never 0 — a budget the judge
could not score is not a budget that scored badly.
"""
import json
import os
import sys

rows = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
rep = {}
if len(sys.argv) > 2 and os.path.exists(sys.argv[2]):
    rep = {a['arm']: a for a in json.load(open(sys.argv[2]))['arms']}

def passages(arm):
    return int(arm.split('x')[0])

rows.sort(key=lambda r: passages(r['arm']))
print('    %-8s %9s %8s %8s %9s' % ('arm', 'overall', 'delta', 'words', 'min'))
base = None
for r in rows:
    a = rep.get(r['arm'], {})
    ms = a.get('ms')
    sc = r.get('overall')
    if base is None and sc is not None:
        base = sc
    d = '%+.2f' % (sc - base) if (sc is not None and base is not None) else '—'
    print('    %-8s %9s %8s %8d %9s' % (
        r['arm'],
        '%.4f' % sc if sc is not None else 'n/a',
        d,
        r['words'],
        '%.1f' % (ms / 60000) if ms else 'n/a'))
print()
print('    Deltas are vs the lowest-budget arm that scored.')
print('    Pre-audit drafts, n=1: a single-run delta is not a result (18.5).')
