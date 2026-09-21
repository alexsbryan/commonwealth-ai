#!/usr/bin/env python3
"""Leg (a) of bar rg-ring-doc-sheds-its-guest-code, as corrected in ledger A57.

Counts the NON-COMMENT lines OUTSIDE THE ASK PANEL that match /guest/i. The ask
panel is two things and nothing else: the `const GUEST = …` declaration, and the
top-level `if (GUEST) {` block through its closing brace. Everything else that
says "guest" in code — another `if (GUEST)`, a `payload.guest`, a renamed arm
that still reads the flag — counts. A comment explaining why the page no longer
names anyone does not: that comment is what the deletion owes the next reader.

usage: ring-doc-guest-lines.py [--self-test] FILE...   → one JSON object
"""
import json, re, sys

DECL = re.compile(r"^\s*const GUEST\s*=")
PANEL = re.compile(r"^if \(GUEST\)\s*\{\s*$")          # top level only: column 0
COMMENT = re.compile(r"^\s*(//|/\*|\*)")


def code_of(line: str) -> str:
    """The line with a trailing `// …` comment removed. `://` (a URL) is kept."""
    return re.sub(r"(?<!:)//.*$", "", line)


def guest_lines(text: str, exempt: list | None = None) -> list[tuple[int, str]]:
    """Counted lines. `exempt`, when given, receives what was NOT counted and why —
    the panel's line range and the declaration — so the exemption is readable."""
    hits, depth, seen_panel = [], 0, False
    exempt = exempt if exempt is not None else []
    for n, line in enumerate(text.splitlines(), 1):
        if depth:                                   # inside the ask panel
            depth += line.count("{") - line.count("}")
            if not depth:
                exempt[-1] += f"-{n}"
            continue
        if COMMENT.match(line):
            continue
        if PANEL.match(line) and not seen_panel:    # ONE panel; a second is code
            seen_panel, depth = True, line.count("{") - line.count("}")
            exempt.append(f"ask-panel {n}")
            continue
        if DECL.match(line):
            exempt.append(f"decl {n}")
            continue
        if re.search("guest", code_of(line), re.I):
            hits.append((n, line.strip()))
    return hits


def self_test() -> int:
    clean = ("// guest mode is the door's\nconst GUEST = has('token');\nlet x = 1;\n"
             "if (GUEST) {\n  ask({ guest: true });\n  if (y) { z(); }\n}\nlet after = 2; // guests see this\n")
    cases = [
        ("the shed tree reads 0", clean, 0),
        ("a guest field in a payload counts", clean + "const p = { guest: name };\n", 1),
        ("a second if (GUEST) arm counts", clean + "if (GUEST) {\n  hold();\n}\n", 1),
        ("a nested use of the flag outside the panel counts", clean + "function f() {\n  if (GUEST) return;\n}\n", 1),
        ("a renamed arm that still reads the flag counts", clean + "const visitor = GUEST ? name : null;\n", 1),
        ("a block placed BEFORE the ask panel cannot take its exemption for free",
         "const GUEST = has('token');\nif (GUEST) {\n  hold();\n}\nif (GUEST) {\n  ask();\n}\n", 1),
        ("code after the panel's closing brace is read again", clean + "send(guestName);\n", 1),
        ("a comment-only mention does not count", clean + "  // nothing about a guest rides in here\n", 0),
        ("a URL is not a comment", "fetch('http://x/guest');\n", 1),
    ]
    bad = 0
    for name, text, want in cases:
        got = len(guest_lines(text))
        print(("  pass  " if got == want else "  FAIL  ") + name + ("" if got == want else f"  got {got} want {want}"))
        bad += got != want
    return 1 if bad else 0


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        sys.exit(self_test())
    out = {"count": 0, "lines": [], "exempt": {}}
    for f in sys.argv[1:]:
        out["exempt"][f] = []
        for n, l in guest_lines(open(f, encoding="utf-8").read(), out["exempt"][f]):
            out["count"] += 1
            out["lines"].append(f"{f}:{n}: {l[:120]}")
    print(json.dumps(out))
