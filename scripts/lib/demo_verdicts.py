# SPDX-License-Identifier: AGPL-3.0-or-later
"""The four-verdict table every demo instrument prints — ONE decider.

`scripts/ring-room-demo.sh` had the loop and its exit rule inline in
`report`'s python heredoc, and `scripts/threat-gaps-demo.sh` needed the same
three things: a row recorder, the floor comparison, and the exit code. Copying
the loop would have made two implementations of one threshold rule (ARCH §8),
and the two would have drifted the first time a fifth verdict was wanted. The
room's copy moved here; both scripts import it.

Import it the way the instruments do, from a heredoc that knows where the
script lives:

    sys.path.insert(0, os.path.join(os.path.dirname(script_path), "lib"))
    import demo_verdicts

The exit rule is co-lineage's, unchanged: 1 if any bar FAILED, 4 if some bar
COULD-NOT-JUDGE, 0 when every bar PASSED. A bar whose value is `None` is
COULD-NOT-JUDGE — an unmeasured bar makes no claim and is never a pass
(ARCH §5, §6).
"""

import json


def new_rows():
    """A `(rows, row)` pair: the dict the report fills, and the recorder.

    `row(bar, value, reason="", **extra)` — `value` is `None` when the
    instrument could not measure the bar at all, which is a verdict of its own
    and never a zero.
    """
    rows = {}

    def row(bar, value, reason="", **extra):
        rows[bar] = dict(bar=bar, value=value, reason=reason, **extra)

    return rows, row


def verdict_for(value, floor, direction):
    """PASSED / FAILED / COULD-NOT-JUDGE for one reading against one floor."""
    if value is None:
        return "COULD-NOT-JUDGE"
    if direction == "lower_is_better":
        return "PASSED" if value <= floor else "FAILED"
    return "PASSED" if value >= floor else "FAILED"


def emit(rows, bars, order, **common):
    """Print one co-lineage measurement row per bar in `order`; return the exit code.

    `bars` maps a bar id to its campaign table (`floor`, `direction` are read
    from it, never restated by a caller). `common` is merged into every row —
    the artifact directory and the topology sentence are the run's, not the
    bar's.
    """
    for bar in order:
        r, b = rows[bar], bars[bar]
        r.update(
            floor=b["floor"],
            verdict=verdict_for(r["value"], b["floor"], b["direction"]),
            **common,
        )
        print(json.dumps(r))
    verdicts = [rows[b]["verdict"] for b in order]
    if "FAILED" in verdicts:
        return 1
    return 4 if "COULD-NOT-JUDGE" in verdicts else 0
