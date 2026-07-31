#!/usr/bin/env python3
"""Harvest next-edit eval cases (NEXT_EDIT.md §6, gym/next-edit/README.md).

Mines this repo's git history for natural repeated-edit episodes —
commits where ≥3 single-line hunks induce the same expanded rule —
plus authored edge-case probes. Ground truth comes from an
independent Python replica of the rule lane's expansion / guard /
site / threshold logic, written from the spec; where a case's intent
is the point, the replica's output is hand-asserted at build time.

Deterministic: no RNG anywhere — a re-run against the same git
history produces the same bank.

Output: gym/next-edit/cases.jsonl — one JSON object per line:
  {id, kind, language, note?, request: {history, text, cursor, path,
   language, debug}, expect: {fire, exact?, sites?, support?,
   rule_find?, rule_replace?, reasons?, expect_capped?,
   over_offer_baseline?}}

Offsets in `request.cursor` and `expect.sites` are UTF-16 code units
(the wire contract).
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]

# ---- replica of the rule lane (commonwealth-api/src/next_edit.rs) ----
# Char-space (Python str) instead of bytes; order-equivalent.

HISTORY_WINDOW = 8
MAX_CTX = 40
MAX_EDITS = 256


def is_ctx_char(c: str) -> bool:
    return (c.isascii() and c.isalnum()) or c in "_$."


def is_word_char(c: str) -> bool:
    return (c.isascii() and c.isalnum()) or c == "_"


def expand_rule(unit: dict) -> dict | None:
    """{find, replace, guard_left, guard_right, absorbed_left} or None."""
    before, after = unit["before"], unit["after"]
    if before == after or "\n" in before or "\n" in after:
        return None
    left_src, right_src = unit["left"], unit["right"]

    n = 0
    for c in reversed(left_src):
        if n >= MAX_CTX or not is_ctx_char(c):
            break
        n += 1
    left = left_src[len(left_src) - n:] if n else ""

    n = 0
    for c in right_src:
        if n >= MAX_CTX or not is_ctx_char(c):
            break
        n += 1
    right = right_src[:n]
    if right_src[n:n + 1] == "(":
        right = right_src[:n + 1]

    find = f"{left}{before}{right}"
    replace = f"{left}{after}{right}"
    if find == replace or not find.strip():
        return None
    return {
        "find": find,
        "replace": replace,
        "guard_left": bool(find) and is_word_char(find[0]),
        "guard_right": bool(find) and is_word_char(find[-1]),
        "absorbed_left": left,
    }


def rule_key(r: dict) -> tuple:
    return (r["find"], r["replace"], r["guard_left"], r["guard_right"])


def already_applied(text: str, o: int, find: str, replace: str) -> bool:
    """True when the occurrence of `find` at `o` sits inside an existing
    instance of `replace` aligned on `find`'s position within it — i.e.
    the user already made this edit here. Only possible when the rule
    is insertion-shaped (replace contains find)."""
    if find == replace or find not in replace:
        return False
    start = 0
    while True:
        f = replace.find(find, start)
        if f < 0:
            return False
        b = o - f
        if b >= 0 and text[b:b + len(replace)] == replace:
            return True
        start = f + 1


def find_guarded_sites(text: str, find: str, guard_left: bool, guard_right: bool,
                       replace: str = "") -> list[int]:
    if not find:
        return []
    out, at = [], 0
    while True:
        o = text.find(find, at)
        if o < 0:
            break
        left_ok = not guard_left or not (o > 0 and is_word_char(text[o - 1]))
        e = o + len(find)
        right_ok = not guard_right or not (e < len(text) and is_word_char(text[e]))
        if left_ok and right_ok and not already_applied(text, o, find, replace):
            out.append(o)
        at = o + len(find)
    return out


def queue_order(sites: list[int], cursor: int) -> list[int]:
    return [s for s in sites if s >= cursor] + [s for s in sites if s < cursor]


def should_fire(find: str, support: int, remaining_sites: int) -> bool:
    if remaining_sites < 1:
        return False
    n = len(find.strip())
    if support <= 1:
        return False
    if support == 2:
        return n >= 4
    return n >= 2


def predict(history: list[dict], text: str, cursor: int) -> dict:
    """Replica of next_edit::predict. Sites/edits in char offsets."""
    rules = [expand_rule(u) for u in history]
    recent = [r for r in rules[-HISTORY_WINDOW:] if r]
    if not recent:
        return {"fire": False, "reason": "no_rule", "rule": None, "support": 0, "sites": []}
    rule = recent[-1]
    support = sum(1 for r in recent if rule_key(r) == rule_key(rule))
    sites = queue_order(
        find_guarded_sites(text, rule["find"], rule["guard_left"], rule["guard_right"],
                           rule["replace"]),
        cursor,
    )
    if not should_fire(rule["find"], support, len(sites)):
        reason = "no_sites" if not sites else "below_threshold"
        return {"fire": False, "reason": reason, "rule": rule, "support": support, "sites": []}
    return {
        "fire": True, "reason": None, "rule": rule, "support": support,
        "sites": sites[:MAX_EDITS], "capped": len(sites) > MAX_EDITS,
        "total_sites": len(sites),
    }


# ---- UTF-16 helpers --------------------------------------------------

def u16len(s: str) -> int:
    return sum(2 if ord(c) > 0xFFFF else 1 for c in s)


def chars_to_u16(text: str, offsets: list[int]) -> dict[int, int]:
    need = sorted(set(offsets))
    out, u, j = {}, 0, 0
    for i, c in enumerate(text):
        while j < len(need) and need[j] == i:
            out[need[j]] = u
            j += 1
        u += 2 if ord(c) > 0xFFFF else 1
    while j < len(need):
        if need[j] != len(text):
            raise ValueError(f"offset {need[j]} beyond text")
        out[need[j]] = u
        j += 1
    return out


# ---- case assembly ---------------------------------------------------

def wire_expect_sites(text: str, char_sites: list[int], rule: dict) -> list[dict]:
    conv = chars_to_u16(text, char_sites)
    flen = u16len(rule["find"])
    return [
        {"start": conv[s], "end": conv[s] + flen, "new_text": rule["replace"]}
        for s in char_sites
    ]


def make_case(cid: str, kind: str, language: str, history: list[dict], text: str,
              cursor_char: int, expect: dict, note: str | None = None,
              path: str | None = None) -> dict:
    cursor_u16 = chars_to_u16(text, [cursor_char])[cursor_char]
    case = {
        "id": cid,
        "kind": kind,
        "language": language,
        "request": {
            "history": history,
            "text": text,
            "cursor": cursor_u16,
            "path": path or f"{cid.split(':')[0]}.{language}",
            "language": language,
            "debug": True,
        },
        "expect": expect,
    }
    if note:
        case["note"] = note
    return case


# ---- git mining ------------------------------------------------------

LANG_OF = {
    ".rs": "rust", ".ts": "typescript", ".mts": "typescript", ".tsx": "typescript",
    ".js": "javascript", ".svelte": "svelte", ".py": "python", ".sh": "shell",
    ".toml": "toml", ".md": "markdown", ".yml": "yaml", ".yaml": "yaml",
    ".json": "json",
}
SKIP_SUBSTR = ("/target/", "node_modules", ".lance", "/fixtures/", "gym/",
               "Cargo.lock", "package-lock", ".min.", ".jsonl")
MAX_FILE_BYTES = 400_000
MAX_MID_CHARS = 100
CTX_CHARS = 48  # mirrors the extension's ±48-char unit context capture


def git(*args: str) -> bytes:
    return subprocess.run(["git", "-C", str(REPO), *args],
                          capture_output=True, check=True).stdout


def line_starts(lines: list[str]) -> list[int]:
    starts, acc = [], 0
    for ln in lines:
        starts.append(acc)
        acc += len(ln) + 1  # the "\n" join separator
    return starts


def strip_common(a: str, b: str) -> tuple[int, int]:
    p = 0
    while p < len(a) and p < len(b) and a[p] == b[p]:
        p += 1
    s = 0
    while s < len(a) - p and s < len(b) - p and a[len(a) - 1 - s] == b[len(b) - 1 - s]:
        s += 1
    return p, s


def hunks_of(old: str, new: str) -> list[dict]:
    """Line-paired single-line transformations with unit context."""
    import difflib
    old_lines, new_lines = old.split("\n"), new.split("\n")
    starts = line_starts(old_lines)
    sm = difflib.SequenceMatcher(None, old_lines, new_lines, autojunk=False)
    hunks = []
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag != "replace" or (i2 - i1) != (j2 - j1):
            continue
        for k in range(i2 - i1):
            a, b = old_lines[i1 + k], new_lines[j1 + k]
            if a == b:
                continue
            p, s = strip_common(a, b)
            mid_a, mid_b = a[p:len(a) - s], b[p:len(b) - s]
            if len(mid_a) > MAX_MID_CHARS or len(mid_b) > MAX_MID_CHARS:
                continue
            abs_p = starts[i1 + k] + p
            unit = {
                "before": mid_a,
                "after": mid_b,
                "left": old[max(0, abs_p - CTX_CHARS):abs_p],
                "right": old[abs_p + len(mid_a):abs_p + len(mid_a) + CTX_CHARS],
            }
            rule = expand_rule(unit)
            if rule is None:
                continue
            hunks.append({"line": i1 + k, "p": p, "mid_a": mid_a, "mid_b": mid_b,
                          "unit": unit, "rule": rule})
    return hunks


def apply_hunks(old_lines: list[str], hunks: list[dict]) -> list[str]:
    out = old_lines[:]
    for h in hunks:
        a = out[h["line"]]
        out[h["line"]] = a[:h["p"]] + h["mid_b"] + a[h["p"] + len(h["mid_a"]):]
    return out


def build_positive(commit: str, path: str, old: str, group: list[dict],
                   counters: dict) -> dict | None:
    rule = group[0]["rule"]
    find = rule["find"]
    k = 2 if len(find.strip()) >= 4 else 3
    if len(group) <= k:
        return None
    group = sorted(group, key=lambda h: h["line"])
    replayed, held = group[:k], group[k:]

    old_lines = old.split("\n")
    sent_lines = apply_hunks(old_lines, replayed)
    text = "\n".join(sent_lines)
    if len(text.encode()) > 500_000:
        return None
    starts = line_starts(sent_lines)
    last = replayed[-1]
    cursor = starts[last["line"]] + last["p"] + len(last["mid_b"])

    history = [h["unit"] for h in replayed]
    pred = predict(history, text, cursor)
    if not pred["fire"] or rule_key(pred["rule"]) != rule_key(rule):
        counters["replay_did_not_fire"] += 1
        return None

    expected_chars = []
    for h in held:
        site = starts[h["line"]] + h["p"] - len(h["rule"]["absorbed_left"])
        if text[site:site + len(find)] != find:
            counters["held_site_mismatch"] += 1
            continue
        if site not in pred["sites"]:
            counters["held_site_shadowed"] += 1
            continue
        expected_chars.append(site)
    if not expected_chars:
        return None

    lang = LANG_OF.get(Path(path).suffix, "other")
    expect = {
        "fire": True,
        "support": k,
        "rule_find": find,
        "rule_replace": rule["replace"],
        "sites": wire_expect_sites(text, expected_chars, rule),
        "over_offer_baseline": len(pred["sites"]) - len(expected_chars),
    }
    cid = f"{commit[:8]}:{path}:{held[0]['line'] + 1}"
    return make_case(cid, "harvest-pos", lang, history, text, cursor, expect,
                     path=Path(path).name)


def build_neg_dissimilar(commit: str, path: str, old: str, singles: list[dict]) -> dict | None:
    two = sorted(singles, key=lambda h: h["line"])[:2]
    if len(two) < 2 or rule_key(two[0]["rule"]) == rule_key(two[1]["rule"]):
        return None
    old_lines = old.split("\n")
    sent_lines = apply_hunks(old_lines, two)
    text = "\n".join(sent_lines)
    if len(text.encode()) > 500_000:
        return None
    starts = line_starts(sent_lines)
    last = two[-1]
    cursor = starts[last["line"]] + last["p"] + len(last["mid_b"])
    history = [h["unit"] for h in two]
    pred = predict(history, text, cursor)
    if pred["fire"]:  # the two rules coincide after expansion — not a negative
        return None
    lang = LANG_OF.get(Path(path).suffix, "other")
    expect = {"fire": False, "reasons": ["below_threshold", "no_sites"]}
    cid = f"{commit[:8]}:{path}:dissimilar"
    return make_case(cid, "harvest-neg", lang, history, text, cursor, expect,
                     note="two dissimilar edits — support 1 must stay silent",
                     path=Path(path).name)


def build_neg_exhausted(commit: str, path: str, old: str, group: list[dict]) -> dict | None:
    rule = group[0]["rule"]
    if len(rule["find"].strip()) < 4 or len(group) != 2:
        return None
    group = sorted(group, key=lambda h: h["line"])
    old_lines = old.split("\n")
    sent_lines = apply_hunks(old_lines, group)
    text = "\n".join(sent_lines)
    if len(text.encode()) > 500_000:
        return None
    starts = line_starts(sent_lines)
    last = group[-1]
    cursor = starts[last["line"]] + last["p"] + len(last["mid_b"])
    history = [h["unit"] for h in group]
    pred = predict(history, text, cursor)
    if pred["fire"] or pred["reason"] != "no_sites":
        return None  # remaining sites exist elsewhere in the file — legit fire
    lang = LANG_OF.get(Path(path).suffix, "other")
    expect = {"fire": False, "reasons": ["no_sites"]}
    cid = f"{commit[:8]}:{path}:exhausted"
    return make_case(cid, "harvest-neg", lang, history, text, cursor, expect,
                     note="all sites already edited — must be silent no_sites",
                     path=Path(path).name)


def mine(max_commits: int, pos_quota: int, neg_quota: int) -> tuple[list[dict], dict]:
    counters = {"replay_did_not_fire": 0, "held_site_mismatch": 0,
                "held_site_shadowed": 0, "commits_scanned": 0}
    commits = git("log", "--no-merges", "--format=%H",
                  f"--max-count={max_commits}").decode().split()
    pos, neg = [], []
    rule_use: dict[tuple, int] = {}
    for commit in commits:
        if len(pos) >= pos_quota and len(neg) >= neg_quota:
            break
        counters["commits_scanned"] += 1
        try:
            names = git("diff-tree", "-r", "--no-renames", "--diff-filter=M",
                        "--name-only", "--format=", commit).decode().split("\n")
        except subprocess.CalledProcessError:
            continue
        files = [n for n in names if n and Path(n).suffix in LANG_OF
                 and not any(s in n for s in SKIP_SUBSTR)][:20]
        for path in files:
            try:
                old_b = git("show", f"{commit}^:{path}")
                new_b = git("show", f"{commit}:{path}")
            except subprocess.CalledProcessError:
                continue
            if len(old_b) > MAX_FILE_BYTES or len(new_b) > MAX_FILE_BYTES:
                continue
            try:
                old, new = old_b.decode(), new_b.decode()
            except UnicodeDecodeError:
                continue
            hunks = hunks_of(old, new)
            if not hunks:
                continue
            groups: dict[tuple, list[dict]] = {}
            for h in hunks:
                groups.setdefault(rule_key(h["rule"]), []).append(h)

            made_here = 0
            for key, group in sorted(groups.items()):
                if len(pos) >= pos_quota or made_here >= 2:
                    break
                if len(group) < 3 or rule_use.get(key, 0) >= 2:
                    continue
                case = build_positive(commit, path, old, group, counters)
                if case:
                    pos.append(case)
                    rule_use[key] = rule_use.get(key, 0) + 1
                    made_here += 1

            if len(neg) < neg_quota:
                singles = [g[0] for g in groups.values() if len(g) == 1]
                case = build_neg_dissimilar(commit, path, old, singles)
                if case:
                    neg.append(case)
            if len(neg) < neg_quota:
                for key, group in sorted(groups.items()):
                    case = build_neg_exhausted(commit, path, old, group)
                    if case:
                        neg.append(case)
                        break
    return pos + neg, counters


# ---- authored probes -------------------------------------------------

def unit(before: str, after: str, left: str, right: str) -> dict:
    return {"before": before, "after": after, "left": left, "right": right}


def authored(cid: str, note: str, language: str, history: list[dict], text: str,
             cursor: int, expect_fire: bool, hand_sites: list[int] | None = None,
             expect_capped: bool = False) -> dict:
    """Build an authored case; ground truth from the replica, with the
    intent hand-asserted (a replica/intent clash fails harvest loudly)."""
    pred = predict(history, text, cursor)
    assert pred["fire"] == expect_fire, f"{cid}: replica fire={pred['fire']}, intent={expect_fire}"
    if expect_fire:
        if hand_sites is not None:
            assert pred["sites"] == hand_sites, \
                f"{cid}: replica sites {pred['sites']} != intent {hand_sites}"
        assert pred.get("capped", False) == expect_capped, f"{cid}: capped mismatch"
        rule = pred["rule"]
        expect = {
            "fire": True,
            "exact": True,
            "support": pred["support"],
            "rule_find": rule["find"],
            "rule_replace": rule["replace"],
            "sites": wire_expect_sites(text, pred["sites"], rule),
            "expect_capped": expect_capped,
        }
    else:
        expect = {"fire": False, "reasons": [pred["reason"]]}
    return make_case(cid, "authored", language, history, text, cursor, expect, note=note)


def console_text(n_debug: int, n_log: int, prefix: str = "", eol: str = "\n") -> str:
    lines = [f'console.debug("l{i}");' for i in range(n_debug)]
    lines += [f'console.log("l{i + n_debug}");' for i in range(n_log)]
    body = eol.join(lines) + eol
    return prefix + body


def authored_cases() -> list[dict]:
    cases = []
    cu = [unit("log", "debug", "console.", '("l0");'),
          unit("log", "debug", 'l0");\nconsole.', '("l1");')]

    # A1 canonical: 2 supports, queue of remaining sites in doc order.
    text = console_text(2, 4)
    cur = text.index('console.log') - 1
    cases.append(authored(
        "a01-canonical", "console.log walk — 2 supports fire, 4-site queue",
        "typescript", cu, text, cur, True,
        hand_sites=[text.index('console.log("l2")'), text.index('console.log("l3")'),
                    text.index('console.log("l4")'), text.index('console.log("l5")')]))

    # A2 emoji prefix: byte/UTF-16 offsets diverge before every site.
    text = console_text(2, 3, prefix="// \U0001F4A1\U0001F4A1 offsets probe\n")
    cases.append(authored(
        "a02-utf16-astral", "astral-plane chars before sites — wire offsets must be UTF-16",
        "typescript", cu, text, 0, True))

    # A3 word-boundary guards + support-3-fires-short-rule.
    text = ("the dog sat\nthe dog ran\nthe dog howled\n"
            "the cat slept\nconcatenate the cats\na cat scattered\n")
    h3 = [unit("cat", "dog", "the ", " sat"),
          unit("cat", "dog", "sat\nthe ", " ran"),
          unit("cat", "dog", "ran\nthe ", " howled")]
    standalone = [text.index("cat slept"), text.index("cat scattered")]
    cases.append(authored(
        "a03-guards-support3", "guards keep `cat` out of concatenate/cats/scattered; "
        "3 supports fire a 3-char rule", "markdown", h3, text,
        text.index(" howled"), True, hand_sites=standalone))

    # A4 the support-2 row: a short rule must NOT fire on 2 supports.
    cases.append(authored(
        "a04-short-rule-2sup", "3-char rule at support 2 — below_threshold",
        "markdown", h3[:2], text, text.index(" ran"), False))

    # A5 exhausted: rule induced but no site remains.
    text5 = console_text(3, 0)
    cases.append(authored(
        "a05-no-sites", "all sites already edited — silent no_sites",
        "typescript", cu, text5, 0, False))

    # A6 one edit never fires.
    cases.append(authored(
        "a06-single-support", "one edit — below_threshold",
        "typescript", cu[:1], console_text(1, 3), 0, False))

    # A7 multi-line unit is uninducible.
    cases.append(authored(
        "a07-multiline-unit", "multi-line unit — no_rule",
        "rust", [unit("a\nb", "c\nd", "", ""), unit("a\nb", "c\nd", "", "")],
        "a\nb\na\nb\n", 0, False))

    # A8 a no-op settle between real units must not erase support.
    cases.append(authored(
        "a08-noop-tail", "trailing no-op unit — support survives, fires",
        "typescript", cu + [unit("x", "x", "", "")], console_text(2, 3), 0, True))

    # A9 CRLF document.
    text9 = console_text(2, 3, eol="\r\n")
    cases.append(authored(
        "a09-crlf", "CRLF line endings — sites + offsets intact",
        "typescript", cu, text9, 0, True))

    # A10 deletion-shaped rule.
    t10 = "res\nres\nres.unwrap()\nres.unwrap()\nres.unwrap()\n"
    h10 = [unit(".unwrap()", "", "res", "\nres"),
           unit(".unwrap()", "", "\nres", "\nres.unwrap()")]
    cases.append(authored(
        "a10-deletion", "deletion rule res.unwrap()→res — 3 remaining sites",
        "rust", h10, t10, t10.index("res.unwrap()"), True))

    # A11 insertion-shaped rule: replace CONTAINS find, so already-edited
    # lines still match textually — they must NOT be re-proposed.
    t11 = ("a = await fetch(u1)\nb = await fetch(u2)\n"
           "c = fetch(u3)\nd = fetch(u4)\ne = fetch(u5)\n")
    h11 = [unit("", "await ", "a = ", "fetch(u1)"),
           unit("", "await ", "u1)\nb = ", "fetch(u2)")]
    bare = [t11.index("fetch(u3)"), t11.index("fetch(u4)"), t11.index("fetch(u5)")]
    cases.append(authored(
        "a11-insertion-idempotent", "insertion rule — already-inserted sites are not re-proposed",
        "typescript", h11, t11, t11.index("u2)") + 3, True, hand_sites=bare))

    # A12 MAX_EDITS cap: 300 sites → 256 edits + capped flag.
    t12 = console_text(2, 300)
    cases.append(authored(
        "a12-max-edits-cap", "300 sites — queue capped at 256, edits_capped true",
        "typescript", cu, t12, 0, True, expect_capped=True))

    # A13 wrap order: sites before the cursor come after those beyond it.
    lines = [f'console.log("l{i}");' for i in range(6)]
    lines[2] = 'console.debug("l2");'
    lines[3] = 'console.debug("l3");'
    t13 = "\n".join(lines) + "\n"
    h13 = [unit("log", "debug", 'l1");\nconsole.', '("l2");'),
           unit("log", "debug", 'l2");\nconsole.', '("l3");')]
    cur13 = t13.index('console.log("l4")') - 1
    wrap = [t13.index('console.log("l4")'), t13.index('console.log("l5")'),
            t13.index('console.log("l0")'), t13.index('console.log("l1")')]
    cases.append(authored(
        "a13-wrap-order", "queue wraps: doc order from cursor, then the sites above",
        "typescript", h13, t13, cur13, True, hand_sites=wrap))

    # A14 non-ASCII BMP chars inside the rule itself.
    t14 = ('# \U0001F30D\nprint("wörld")\nprint("wörld")\n'
           'print("héllo")\nprint("héllo")\n')
    h14 = [unit("héllo", "wörld", 'print("', '")'),
           unit("héllo", "wörld", '")\nprint("', '")')]
    cases.append(authored(
        "a14-non-ascii-rule", "accented chars in find/replace + astral char above",
        "python", h14, t14, 0, True))

    # A15 tab-indented sites; 4-char rule fires exactly at the ≥4 bar.
    t15 = ("build:\n\tprintf done\nlint:\n\tprintf done\ntest:\n\techo done\n"
           "docs:\n\techo done\n# echoes here must not match\n")
    h15 = [unit("echo", "printf", "build:\n\t", " done"),
           unit("echo", "printf", "lint:\n\t", " done")]
    t15_sites = [t15.index("echo done", t15.index("test:")),
                 t15.index("echo done", t15.index("docs:"))]
    cases.append(authored(
        "a15-tabs-4char-bar", "tab context; 4-char rule at support 2 — exactly at the bar",
        "makefile", h15, t15, 0, True, hand_sites=t15_sites))

    return cases


# ---- main ------------------------------------------------------------

def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--max-commits", type=int, default=4000)
    ap.add_argument("--pos", type=int, default=80)
    ap.add_argument("--neg", type=int, default=25)
    ap.add_argument("--out", type=Path, default=Path(__file__).with_name("cases.jsonl"))
    args = ap.parse_args()

    cases = authored_cases()
    mined, counters = mine(args.max_commits, args.pos, args.neg)
    cases += mined

    with args.out.open("w", encoding="utf-8") as fh:
        for c in cases:
            fh.write(json.dumps(c, ensure_ascii=False) + "\n")

    by_kind: dict[str, int] = {}
    by_lang: dict[str, int] = {}
    for c in cases:
        by_kind[c["kind"]] = by_kind.get(c["kind"], 0) + 1
        by_lang[c["language"]] = by_lang.get(c["language"], 0) + 1
    print(f"wrote {len(cases)} cases to {args.out}")
    print(f"  kinds: {by_kind}")
    print(f"  langs: {by_lang}")
    print(f"  mining: {counters}")
    if counters["replay_did_not_fire"] or counters["held_site_mismatch"]:
        print("  note: skip counters above are harvest-side sanity exclusions — "
              "surges there mean the replica or the miner drifted", file=sys.stderr)


if __name__ == "__main__":
    main()
