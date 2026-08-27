# SPDX-License-Identifier: AGPL-3.0-or-later
"""Accumulate per-arm replay reports into one.

The sweep flies ONE arm per `cargo test` invocation so that a daemon crash
costs one arm instead of the whole run. Each invocation writes its own
`compose-replay.json` containing only that arm, overwriting the previous one,
so without this the sweep would finish holding the timings of its last arm and
nothing else.

Last write wins per arm, so re-flying an arm supersedes its earlier row rather
than duplicating it.
"""
import json
import os
import sys

src, dst = sys.argv[1], sys.argv[2]
if not os.path.exists(src):
    sys.exit(0)

new = json.load(open(src))
if os.path.exists(dst):
    merged = json.load(open(dst))
else:
    merged = {k: v for k, v in new.items() if k != 'arms'}
    merged['arms'] = []

by_arm = {a['arm']: a for a in merged.get('arms', [])}
for a in new.get('arms', []):
    by_arm[a['arm']] = a
merged['arms'] = sorted(by_arm.values(), key=lambda a: int(a['arm'].split('x')[0]))
json.dump(merged, open(dst, 'w'), indent=1)
print('    merged -> %d arm(s) in %s' % (len(merged['arms']), dst))
