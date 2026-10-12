#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Parse a pytest --junitxml report into the battery's graded counters.

Pure function of the XML: no daemon, no network, no environment. Unit-tested
against all-pass / partial / zero / empty fixtures (`--self-test`).

The upstream verifier uses `pytest --ctrf`; that plugin is not on PyPI (404 at
/simple/pytest-ctrf/), so the battery's verify invokes pytest with its native
--junitxml instead. The graded signal is the same per-test pass/fail set; the
reward definition is upstream's own (pytest exit 0) and does not come from this
file — run_battery.py writes it from the pytest exit code.
"""
import sys
import xml.etree.ElementTree as ET


def score_junit(path):
    """Return counters from a JUnit XML report.

    Raises on a structurally absent report (no <testsuites>/<testsuite>) so a
    crashed verify is never miscounted as a zero — absence of a result is not a
    result (ARCH §6).
    """
    root = ET.parse(path).getroot()
    suites = [root] if root.tag == "testsuite" else root.findall("testsuite")
    if not suites:
        raise ValueError(f"{path}: no <testsuite> element — verify produced no report")
    total = passed = failed = skipped = errors = 0
    failures = []
    for suite in suites:
        for case in suite.iter("testcase"):
            total += 1
            kids = list(case)
            failed_kids = [k for k in kids if k.tag in ("failure", "error")]
            if failed_kids:
                if failed_kids[0].tag == "error":
                    errors += 1
                else:
                    failed += 1
                name = f"{case.get('classname', '')}::{case.get('name', '')}"
                msg = failed_kids[0].get("message", "") or failed_kids[0].get("type", "")
                failures.append({"test": name, "message": msg[:300]})
            elif any(k.tag == "skipped" for k in kids):
                skipped += 1
            else:
                passed += 1
    return {
        "tests_total": total,
        "tests_passed": passed,
        "tests_failed": failed,
        "tests_errors": errors,
        "tests_skipped": skipped,
        "test_fraction": round(passed / total, 4) if total else 0.0,
        "first_failures": failures[:8],
    }


def _write(path, counters):
    import json

    with open(path, "w") as f:
        json.dump(counters, f, indent=1)


def main(argv):
    if len(argv) >= 2 and argv[1] == "--self-test":
        return _self_test(argv[2:])
    if len(argv) < 3:
        print(__doc__)
        print("usage: score_junit.py <junit.xml> [--out row.json]")
        return 2
    counters = score_junit(argv[1])
    if "--out" in argv:
        _write(argv[argv.index("--out") + 1], counters)
    else:
        print(counters)
    return 0


def _self_test(args):
    """Run the built-in fixtures (all-pass / partial / zero) and exit 0 iff all agree."""
    import json
    import tempfile
    from pathlib import Path

    def fixture(names_fail, skipped=0):
        cases = "".join(
            f'<testcase classname="mod" name="t{i}"/>'
            for i in range(100 - names_fail - skipped)
        )
        cases += "".join(
            f'<testcase classname="mod" name="f{i}"><failure message="x"/></testcase>'
            for i in range(names_fail)
        )
        cases += "".join(
            f'<testcase classname="mod" name="s{i}"><skipped/></testcase>'
            for i in range(skipped)
        )
        return f'<testsuite tests="100">{cases}</testsuite>'

    expect = [
        (fixture(0), dict(tests_total=100, tests_passed=100, tests_failed=0,
                          tests_errors=0, tests_skipped=0, test_fraction=1.0)),
        (fixture(40, skipped=10), dict(tests_total=100, tests_passed=50, tests_failed=40,
                                       tests_errors=0, tests_skipped=10, test_fraction=0.5)),
        (fixture(100), dict(tests_total=100, tests_passed=0, tests_failed=100,
                            tests_errors=0, tests_skipped=0, test_fraction=0.0)),
    ]
    ok = True
    with tempfile.TemporaryDirectory() as td:
        for i, (xml, want) in enumerate(expect):
            p = Path(td) / f"case{i}.xml"
            p.write_text(xml)
            got = score_junit(str(p))
            for k, v in want.items():
                if got[k] != v:
                    print(f"self-test case {i}: {k} = {got[k]}, want {v}")
                    ok = False
        # A structurally present-but-empty report must RAISE, not count as zero,
        # and so must an absent file: neither is a measurement (ARCH §6).
        empty = Path(td) / "empty.xml"
        empty.write_text("<root/>")
        for bad in (empty, Path(td) / "absent.xml"):
            try:
                score_junit(str(bad))
                print(f"self-test: {bad.name} did not raise")
                ok = False
            except Exception:
                pass
    print("score_junit self-test:", "ok" if ok else "FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
