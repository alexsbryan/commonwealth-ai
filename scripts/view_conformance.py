#!/usr/bin/env python3
"""A SECOND implementation of the View fold, checked against the frozen
fixture — the conformance proof the golden vectors exist for.

Written from the FORMULA in the fixture and in the module docs of
`shared/crates/commonwealth-rail-core/src/view.rs`, deliberately
without reading that crate's code: if a second party cannot implement the
fold from the spec text, the spec is not a spec. What this proves is the
folding rule and its edge cases (an empty window folds to the domain hash;
ids at one seq are sorted then deduped; pairs outside [from, mark] are
ignored). It does NOT prove the op-id derivation — that is `oplog`'s
contract, pinned separately.

Usage:  python3 scripts/view_conformance.py [path/to/view_golden.json]
Needs:  pip install blake3
Exit:   0 all cases pass; 1 a case diverges; 3 could-not-judge (no blake3).
"""
import json
import sys

try:
    import blake3
except ImportError:
    print("view-conformance: COULD-NOT-JUDGE — `pip install blake3`")
    sys.exit(3)

DOMAIN = b"cwth/view/1"


def fold(actor: str, frm: int, mark: int, pairs) -> str:
    """h0 = BLAKE3(domain || 0x00 || actor);
    hs = BLAKE3(h{s-1} || 0x00 || be64(s) || 0x00 || id)
    over ids at s sorted then deduped, s in [from, mark]."""
    h = blake3.blake3(DOMAIN + b"\0" + actor.encode()).digest()
    by_seq = {}
    for seq, id_ in pairs:
        if frm <= seq <= mark:
            by_seq.setdefault(seq, set()).add(id_)
    for seq in sorted(by_seq):
        for id_ in sorted(by_seq[seq]):
            step = h + b"\0" + seq.to_bytes(8, "big") + b"\0" + id_.encode()
            h = blake3.blake3(step).digest()
    return h.hex()


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else (
        "shared/crates/commonwealth-rail-core/fixtures/view_golden.json"
    )
    doc = json.load(open(path))
    failures = 0
    for case in doc["cases"]:
        got = fold(case["actor"], case["from"], case["mark"], [tuple(p) for p in case["ops"]])
        ok = got == case["head"]
        print(("PASS " if ok else "FAIL ") + case["name"])
        if not ok:
            print(f"   fixture {case['head']}")
            print(f"   this    {got}")
            failures += 1
    if failures:
        print(f"view-conformance FAILED — {failures} case(s) diverge from the fixture")
        return 1
    print(f"view-conformance PASSED — {len(doc['cases'])} cases, second implementation agrees")
    return 0


if __name__ == "__main__":
    sys.exit(main())
