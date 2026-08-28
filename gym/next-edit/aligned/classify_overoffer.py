#!/usr/bin/env python3
"""M1a — why does the symbol lane over-offer across files?

A DIAGNOSIS, not a filter. It writes no daemon code and proposes no
mechanism; it partitions the population M0 already measured
(`probe_symbol_lane.py`, which this imports rather than re-derives) so
that M1b can pick a filter on evidence instead of on a guess.

The population, fixed by M0 and not recomputed here:
    cross-file predicted 1156 · author-edited 694 · overlap 593
    => 563 over-offered, 101 missed.

Pre-registered in sovereign/docs/specs/NEXT_EDIT_SYMBOL_LANE.md §M1a,
written before this ran. For every candidate filter: junk removed, good
lost, precision after, recall after (denominator stays 694). A filter is
a candidate for M1b iff precision >= 60% AND recall >= 80%.

Run from the REPO ROOT — every path here is repo-relative.
"""
from __future__ import annotations

import argparse
import collections
import re
import sqlite3

from probe_symbol_lane import DB, SIG_SPAN, derive_episodes, git

# ---------------------------------------------------------------- parsing

OPEN, CLOSE = {"(": ")", "[": "]", "<": ">"}, {")": "(", "]": "[", ">": "<"}


def split_top_level(text: str) -> list[str]:
    """Split on commas at bracket depth 0. `Vec<A, B>` is ONE argument."""
    out, buf, depth = [], [], 0
    i = 0
    while i < len(text):
        c = text[i]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
        elif c == "<" and not (i and text[i - 1] in "-=<"):
            depth += 1
        elif c == ">" and not (i and text[i - 1] in "-=>"):
            depth = max(0, depth - 1)
        elif c == "," and depth == 0:
            out.append("".join(buf))
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    if "".join(buf).strip():
        out.append("".join(buf))
    return [p.strip() for p in out if p.strip()]


def paren_group(text: str, open_at: int) -> str | None:
    """The text inside the parens that open at `open_at`, or None if unbalanced."""
    depth = 0
    for i in range(open_at, len(text)):
        if text[i] == "(":
            depth += 1
        elif text[i] == ")":
            depth -= 1
            if depth == 0:
                return text[open_at + 1:i]
    return None


def arity_of(params: str) -> int:
    """Argument count, excluding a leading receiver."""
    parts = split_top_level(params)
    if parts and re.match(r"^(&\s*(mut\s+)?|mut\s+)?self\b", parts[0]):
        parts = parts[1:]
    return len(parts)


def declaration(text: str, name: str) -> dict | None:
    """The signature of `fn name` in `text`, as {params, arity, sig}."""
    m = re.search(rf"\bfn\s+{re.escape(name)}\b", text)
    if not m:
        return None
    open_at = text.find("(", m.end())
    if open_at < 0:
        return None
    params = paren_group(text, open_at)
    if params is None:
        return None
    close_at = open_at + len(params) + 1
    tail = text[close_at + 1:]
    # the signature ends at the body brace or the `;` of a trait method
    end = len(tail)
    depth = 0
    for i, c in enumerate(tail):
        if c in "([<":
            depth += 1
        elif c in ")]>":
            depth = max(0, depth - 1)
        elif c in "{;" and depth == 0:
            end = i
            break
    sig = text[m.start():close_at + 1 + end]
    return {"params": params, "arity": arity_of(params),
            "sig": re.sub(r"\s+", " ", sig).strip()}


def norm(s: str) -> str:
    return re.sub(r"\s+", " ", s).strip()


# ------------------------------------------------------- episode-level kind

def change_kind(old: dict | None, new: dict | None) -> str:
    """How the declaration changed between commit^ and the commit."""
    if new is None:
        return "unparsed_new"
    if old is None:
        return "new_function"          # did not exist before: nothing to fan out
    if norm(old["sig"]) == norm(new["sig"]):
        return "signature_unchanged"   # the trigger line moved, the contract did not
    if old["arity"] != new["arity"]:
        return "arity_changed"
    op, np_ = split_top_level(old["params"]), split_top_level(new["params"])
    otypes = [norm(p.split(":", 1)[1]) if ":" in p else norm(p) for p in op]
    ntypes = [norm(p.split(":", 1)[1]) if ":" in p else norm(p) for p in np_]
    if otypes != ntypes:
        return "param_type_changed"
    if [norm(p) for p in op] != [norm(p) for p in np_]:
        return "param_renamed_only"
    return "return_or_generics_only"


def resolve_old(commit: str, decl_path: str, name: str) -> tuple[dict | None, str]:
    """The declaration as it stood at commit^, found by SYMBOL not by path.

    `git show commit^:decl_path` returns nothing for a file the commit
    moved, which reads as "the function is new" and is wrong: on a 40-episode
    sample, 20 of the functions so classified existed in the parent tree
    under another path. A bulk file move makes every function in the file
    look like a fresh signature edit, so this distinction is load-bearing
    rather than cosmetic. Falling back to a tree-wide search is conservative
    against that finding: a same-named function elsewhere counts as
    "existed", which SHRINKS the class this correction creates.
    """
    src = git("show", f"{commit}^:{decl_path}")
    if src and re.search(rf"\bfn\s+{re.escape(name)}\b", src):
        return declaration(src, name), "same_path"
    hits = git("grep", "-l", "-E", rf"\bfn\s+{re.escape(name)}\b",
               f"{commit}^", "--", "*.rs").strip()
    if not hits:
        return None, "absent"
    other = hits.split("\n")[0].split(":", 1)[1]
    return declaration(git("show", f"{commit}^:{other}"), name), "moved"


# ---------------------------------------------------------- site-level axes

TEST_PATH = re.compile(r"(^|/)(tests?|benches|examples|fuzz)/|(^|/)test_[^/]*\.rs$"
                       r"|_tests?\.rs$|(^|/)bench(es)?_[^/]*\.rs$")


def in_cfg_test(lines: list[str], idx: int) -> bool:
    """Is this line inside a `#[cfg(test)]` item? Brace-counted upward."""
    depth = 0
    for i in range(idx, -1, -1):
        line = lines[i]
        depth += line.count("}") - line.count("{")
        if depth < 0:
            # we just walked out of the enclosing block; look just above it
            for j in range(max(0, i - 6), i + 1):
                if "cfg(test)" in lines[j] or "cfg(feature" in lines[j]:
                    return True
            depth = 0
    return False


def crate_of(path: str) -> str:
    parts = path.split("/")
    for i, seg in enumerate(parts):
        if seg == "crates" and i + 1 < len(parts):
            return "/".join(parts[:i + 2])
    return parts[0] if parts else ""


def is_call(lines: list[str], line: int, end_col: int) -> tuple[bool, int | None]:
    """Does an actual call paren follow this occurrence, and with what arity?

    Uses the compiler's own column, so this asks about THE occurrence the
    graph named rather than about the first textual match on the line.
    """
    if not (0 <= line < len(lines)):
        return False, None
    text = "\n".join(lines[line:line + 40])
    off = end_col
    first = lines[line]
    if end_col > len(first):
        return False, None
    # skip a turbofish between the name and the call parens
    m = re.match(r"\s*(::\s*<[^;{}]*?>)?\s*\(", text[off:])
    if not m:
        return False, None
    params = paren_group(text, off + m.end() - 1)
    return True, (arity_of(params) if params is not None else None)


# ------------------------------------------------------------------ report

def cluster_ci(rows: list[dict], author_by_commit: dict[str, int], b: int = 2000):
    """Resample COMMITS with replacement, not sites.

    Episodes inside one commit share an author, an intent and a bulk-move
    status, so they are not independent draws; a rate quoted over sites as
    though they were overstates its own precision (ARCH 18.5). Returns
    (per-commit table, (precision, lo, hi), (recall, lo, hi)).
    """
    import random
    by: dict[str, list[int]] = collections.defaultdict(lambda: [0, 0, 0])
    for r in rows:
        cell = by[r["commit"]]
        cell[0] += r["tp"]
        cell[1] += 1
    for cm, a in author_by_commit.items():
        by[cm][2] += a
    random.seed(11)
    keys = list(by)
    ps, rs = [], []
    for _ in range(b):
        pick = [by[random.choice(keys)] for _ in keys]
        tp = sum(c[0] for c in pick)
        n = sum(c[1] for c in pick)
        a = sum(c[2] for c in pick)
        if n:
            ps.append(tp / n)
        if a:
            rs.append(tp / a)
    ps.sort()
    rs.sort()
    def band(v):
        return (v[int(.025 * len(v))], v[int(.975 * len(v))]) if v else (0.0, 0.0)
    tot_tp = sum(c[0] for c in by.values())
    tot_n = sum(c[1] for c in by.values())
    tot_a = sum(c[2] for c in by.values())
    plo, phi = band(ps)
    rlo, rhi = band(rs)
    return by, (tot_tp / tot_n if tot_n else 0, plo, phi), (tot_tp / tot_a if tot_a else 0, rlo, rhi)


def rate(tp: int, fp: int) -> str:
    tot = tp + fp
    return f"{tp / tot * 100:5.1f}%" if tot else "    --"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--window", type=int, default=8000)
    ap.add_argument("--db", default=DB)
    ap.add_argument("--dump", help="write per-site rows as TSV here")
    # M0 published TWO headlines. The cross-file one is M1a's subject, but the
    # clustering below is a property of how the population was built, not of
    # which slice is read, so the all-sites headline must be tested by the
    # same ruler rather than assumed to survive it.
    ap.add_argument("--all-sites", action="store_true")
    args = ap.parse_args()
    con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)

    episodes, counters, corpus = derive_episodes(
        con, args.window, cross_file_only=not args.all_sites)

    # Occurrence COLUMNS for the named sites. A second scan of `refs`, for a
    # different question than M0 asked (where in the line, not which line);
    # there is still no index on callee_qualified.
    wanted = {e["qualified"] for e in episodes}
    cols: dict[tuple[str, str, int], tuple[int, int]] = {}
    for q, f, l, sc, ec in con.execute(
            "select callee_qualified, file_path, line, start_col, end_col from refs"):
        if q in wanted:
            cols.setdefault((q, f, l), (sc, ec))

    rows = []
    for e in episodes:
        name, qual, commit = e["symbol"], e["qualified"], e["commit"]
        decl_path = e["decl_path"]
        new_decl = declaration("\n".join(corpus.lines(decl_path)[e["decl_start"]:
                                                                 e["decl_end"] + 1]), name)
        old_decl, old_status = resolve_old(commit, decl_path, name)
        kind = change_kind(old_decl, new_decl)
        e["kind"] = kind
        d_crate = crate_of(decl_path)
        truth = e["author_sites"]
        for (f, l) in e["pred_sites"]:
            lines = corpus.lines(f)
            sc, ec = cols.get((qual, f, l), (-1, -1))
            called, call_arity = is_call(lines, l, ec) if ec >= 0 else (False, None)
            rows.append({
                "tp": (f, l) in truth, "symbol": name, "commit": commit,
                "kind": kind, "old_status": old_status,
                "old_arity": old_decl["arity"] if old_decl else None,
                "new_arity": new_decl["arity"] if new_decl else None,
                "call": called, "call_arity": call_arity,
                "test_path": bool(TEST_PATH.search(f)),
                "cfg_test": in_cfg_test(lines, l),
                "same_crate": crate_of(f) == d_crate,
                "file": f, "line": l,
            })

    TP = sum(r["tp"] for r in rows)
    FP = len(rows) - TP
    A = sum(len(e["author_sites"]) for e in episodes)
    print(f"\nM1a — over-offer classification (cross-file, rust, index-aligned)")
    print(f"  episodes {len(episodes)} · predicted {len(rows)} · "
          f"author-edited {A} · overlap {TP} · OVER-OFFERED {FP}")
    expect = (2999, 2752, 2597) if args.all_sites else (1156, 694, 593)
    if (len(rows), A, TP) != expect:
        print(f"  !! population differs from M0's {expect} — investigate before reading on")

    # --- instrument check first (ARCH 18.4): every file here is byte-identical
    # to HEAD and HEAD compiles, so a predicted call site should already carry
    # the NEW arity. If it does not, the arity extractor is wrong.
    ok = [r for r in rows if r["call"] and r["call_arity"] is not None
          and r["new_arity"] is not None]
    match_new = sum(1 for r in ok if r["call_arity"] == r["new_arity"])
    print(f"\n  INSTRUMENT CHECK — call sites parsed {len(ok)}/{len(rows)}; "
          f"arity matches the NEW declaration {match_new}/{len(ok)} "
          f"= {match_new / len(ok) * 100:.1f}%" if ok else "\n  INSTRUMENT CHECK — no parsable sites")

    def axis(title: str, keyfn) -> None:
        print(f"\n  {title}")
        buckets: dict[object, list[int]] = collections.defaultdict(lambda: [0, 0])
        for r in rows:
            buckets[keyfn(r)][0 if r["tp"] else 1] += 1
        print(f"    {'bucket':<26s} {'TP':>5s} {'FP':>5s} {'precision':>10s}")
        for k, (tp, fp) in sorted(buckets.items(), key=lambda kv: -(kv[1][0] + kv[1][1])):
            print(f"    {str(k):<26s} {tp:>5d} {fp:>5d} {rate(tp, fp):>10s}")

    axis("BY SIGNATURE-CHANGE KIND (episode-level)", lambda r: r["kind"])
    axis("BY WHETHER THE OCCURRENCE IS A CALL", lambda r: "call" if r["call"] else "not-a-call")
    axis("BY TEST/BENCH/EXAMPLE PATH", lambda r: "test-ish path" if r["test_path"] else "product path")
    axis("BY #[cfg(test)] / #[cfg(feature)] ENCLOSURE", lambda r: "cfg-gated" if r["cfg_test"] else "always-built")
    axis("BY CRATE DISTANCE", lambda r: "same crate" if r["same_crate"] else "other crate")
    axis("BY WHERE THE PRE-COMMIT DECLARATION WAS FOUND", lambda r: r["old_status"])

    # --- CLUSTERING. last_touching() keeps, per file, the commit that touched
    # it LAST, and alignment then requires that file to be untouched since. So
    # the population is whatever recent commits touched the most files, and a
    # squash-merged PR touches hundreds. Episodes inside one commit share an
    # author, an intent and a bulk-move status.
    aut = collections.Counter()
    for e in episodes:
        aut[e["commit"]] += len(e["author_sites"])
    by_commit, prec, rec = cluster_ci(rows, aut)
    print(f"\n  CLUSTERING — {len(by_commit)} commits supply all {len(rows)} sites")
    cum = 0
    for cm, (tp, n, _) in sorted(by_commit.items(), key=lambda kv: -kv[1][1])[:5]:
        cum += n
        print(f"    {cm[:9]}  {n:>4d} sites  prec {rate(tp, n - tp)}  "
              f"cum {cum / len(rows) * 100:4.1f}%  {git('log', '-1', '--format=%s', cm).strip()[:44]}")
    print(f"    -> effective n for a CI is the COMMIT, not the site.")
    print(f"    cluster-bootstrap 95% CI   precision {prec[0] * 100:.1f}% "
          f"[{prec[1] * 100:.1f}, {prec[2] * 100:.1f}]   recall {rec[0] * 100:.1f}% "
          f"[{rec[1] * 100:.1f}, {rec[2] * 100:.1f}]")
    for label, band, bar in (("precision", prec, .60), ("recall", rec, .80)):
        if band[1] < bar < band[2]:
            print(f"    !! the {bar * 100:.0f}% {label} bar lies INSIDE the interval — "
                  f"M0's verdict on this slice is COULD-NOT-JUDGE, not pass or fail")

    # --- THE TARGET SHAPE. The spec's hypothesis is about a function that
    # EXISTED and whose PARAMETER LIST then changed: that is the only shape
    # where a call site is obliged to change. Everything else in the table
    # above is a different event wearing the same trigger. Reported against
    # the pre-registered YIELD bar, because a bar met by the wrong episodes
    # was never met.
    TARGET = ("arity_changed", "param_type_changed")
    tgt = [r for r in rows if r["kind"] in TARGET]
    teps = {(r["symbol"], r["commit"]) for r in tgt}
    tcom = {r["commit"] for r in tgt}
    ta = sum(len(e["author_sites"]) for e in episodes if e.get("kind") in TARGET)
    ttp = sum(1 for r in tgt if r["tp"])
    print(f"\n  THE TARGET SHAPE — an existing function whose parameter list changed")
    print(f"    episodes {len(teps)} (bar >= 25)  ·  commits {len(tcom)}  ·  "
          f"sites {len(tgt)} of {len(rows)} = {len(tgt) / len(rows) * 100:.1f}%")
    print(f"    author-edited {ta} · overlap {ttp}")
    if len(teps) < 25:
        print(f"    YIELD BAR NOT MET. The spec pre-registered 'trigger yield >= 25")
        print(f"    episodes, else report the CI and rank nothing'. M0 read that bar")
        print(f"    against episodes of every kind. On the shape the hypothesis names,")
        print(f"    the yield is {len(teps)}. No rate is published for this slice.")
    elif ta:
        taut = collections.Counter()
        for e in episodes:
            if e.get("kind") in TARGET:
                taut[e["commit"]] += len(e["author_sites"])
        _, tp_ci, tr_ci = cluster_ci(tgt, taut)
        print(f"    precision {tp_ci[0] * 100:.1f}% [{tp_ci[1] * 100:.1f}, {tp_ci[2] * 100:.1f}]"
              f"   recall {tr_ci[0] * 100:.1f}% [{tr_ci[1] * 100:.1f}, {tr_ci[2] * 100:.1f}]"
              f"   (same ruler: {len(tcom)} clusters)")
        for label, band, bar in (("precision", tp_ci, .60), ("recall", tr_ci, .80)):
            verdict = "PASS" if band[1] >= bar else ("FAIL" if band[2] < bar else "COULD-NOT-JUDGE")
            print(f"      {label:<10s} bar {bar * 100:.0f}%  ->  {verdict}")

    # --- the pre-registered filter table
    print(f"\n  CANDIDATE FILTERS — pre-registered ruler "
          f"(precision >= 60% AND recall >= 80% to advance to M1b)")
    print(f"    {'filter (keep only ...)':<34s} {'junk-':>6s} {'good':>5s} "
          f"{'prec':>7s} {'recall':>7s}  verdict")
    print(f"    {'':34s} {'rm':>6s} {'lost':>5s}")
    cands = [
        ("calls (drop non-call occurrences)", lambda r: r["call"]),
        ("product paths (drop test-ish)", lambda r: not r["test_path"]),
        ("always-built (drop cfg-gated)", lambda r: not r["cfg_test"]),
        ("other-crate sites", lambda r: not r["same_crate"]),
        ("same-crate sites", lambda r: r["same_crate"]),
        ("episodes whose arity changed", lambda r: r["kind"] == "arity_changed"),
        ("episodes with a real sig change",
         lambda r: r["kind"] not in ("signature_unchanged", "new_function", "unparsed_new")),
        ("call AND real sig change",
         lambda r: r["call"] and r["kind"] not in ("signature_unchanged", "new_function", "unparsed_new")),
        ("call AND arity changed",
         lambda r: r["call"] and r["kind"] == "arity_changed"),
        ("call AND real sig AND product path",
         lambda r: r["call"] and not r["test_path"]
         and r["kind"] not in ("signature_unchanged", "new_function", "unparsed_new")),
    ]
    for label, keep in cands:
        kt = sum(1 for r in rows if r["tp"] and keep(r))
        kf = sum(1 for r in rows if not r["tp"] and keep(r))
        prec = kt / (kt + kf) if kt + kf else 0.0
        rec = kt / A if A else 0.0
        verdict = "CANDIDATE" if prec >= .60 and rec >= .80 else ""
        print(f"    {label:<34s} {FP - kf:>6d} {TP - kt:>5d} "
              f"{prec * 100:6.1f}% {rec * 100:6.1f}%  {verdict}")

    if args.dump:
        keys = ["tp", "kind", "old_status", "call", "call_arity", "old_arity", "new_arity",
                "test_path", "cfg_test", "same_crate", "symbol", "file", "line", "commit"]
        with open(args.dump, "w") as fh:
            fh.write("\t".join(keys) + "\n")
            for r in rows:
                fh.write("\t".join(str(r[k]) for k in keys) + "\n")
        print(f"\n  per-site rows -> {args.dump}")


if __name__ == "__main__":
    main()
