#!/usr/bin/env python3
"""ra-5's five legs as one instrument — emits the FRACTION that pass.

co-lineage's contract: instruments emit VALUES only ("the verdict is
computed here, by the one decider"). This emits legs-passed / 5 as a bare
float. The legs are `ra-membership-is-order-free`'s floor_basis, verbatim;
each leg's test was watched failing first against its named defect before
this instrument existed (ROOT_CAUSE_FIXES A4).

Leg 5 is three checks and counts once — the rule is the BAR's unit (a
leg), not the test's.
"""
import subprocess
import sys

RAIL = "commonwealth-rail-core"

LEGS = [
    ("leg 1 — permutations agree", RAIL, [
        "every_interleaving_of_a_membership_set_folds_identically",
    ]),
    ("leg 2 — the cascade", RAIL, [
        "voiding_an_admit_drops_what_it_transitively_admitted",
    ]),
    ("leg 3 — the restore", RAIL, [
        "voiding_a_remove_restores_the_member_and_their_in_between_writes",
    ]),
    ("leg 4 — the lost laptop", RAIL, [
        "a_replacement_key_leaves_every_past_act_admitted",
    ]),
    ("leg 5 — the version gate", RAIL, [
        "the_membership_acts_ship_at_a_bumped_line_version",
        "a_ring_with_no_membership_acts_admits_exactly_as_before",
    ]),
    ("leg 5 — the envelope gate", "oplog", [
        "a_newer_version_line_is_skipped_before_its_body_is_parsed",
    ]),
]


def passes(crate: str, name: str) -> bool:
    r = subprocess.run(
        ["cargo", "test", "-q", "-p", crate, "--lib", "--", name],
        capture_output=True,
        text=True,
    )
    return r.returncode == 0


def main() -> int:
    legs = 0
    leg5_ok = True
    for leg, crate, names in LEGS:
        ok = all(passes(crate, n) for n in names)
        print(f"{'PASS' if ok else 'FAIL'}  {leg}", file=sys.stderr)
        if leg.startswith("leg 5"):
            leg5_ok = leg5_ok and ok
        elif ok:
            legs += 1
    if leg5_ok:
        legs += 1
    value = legs / 5.0
    print(f"{value:.2f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
