# SPDX-License-Identifier: AGPL-3.0-or-later
"""Re-derive an arm's score from an already-stored judge output.

The judge costs ~9.5 minutes per article on the pinned 27B, so re-running the
scorer over a directory must not re-buy the arms it has already judged. The
sidecar written by `score_one.py --save-judge` fingerprints WHAT was judged
(article_sha256 + judge_model), which is exactly the key needed to decide
whether a stored verdict applies to the bytes on disk right now.

Prints `OVERALL x100 = <n>` on a cache hit and exits 0; exits 1 on a miss, so
the caller judges for real. Re-derives rather than trusting a stored number:
`derive()` is the one scoring implementation (ARCH 10.6).
"""
import hashlib
import json
import os
import sys

sidecar, article, judge_model, task = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
if not os.path.exists(sidecar):
    sys.exit(1)

sha = hashlib.sha256(open(article, 'rb').read()).hexdigest()

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'lab'))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..'))
from score_one import derive  # noqa: E402

hit = None
for line in open(sidecar):
    line = line.strip()
    if not line:
        continue
    r = json.loads(line)
    if (r.get('article_sha256') == sha
            and r.get('judge_model') == judge_model
            and int(r.get('id', -1)) == task):
        hit = r  # last write wins — a re-judge supersedes an earlier one
if hit is None:
    sys.exit(1)

rec = derive(task, hit['judge_output'])
print('OVERALL x100 = %.4f' % (100 * rec['overall_score']))
