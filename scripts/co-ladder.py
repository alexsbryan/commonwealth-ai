#!/usr/bin/env python3
"""co-ladder.py — the campaign ladder becomes a schema, not prose.

WHY (operator direction 2026-09-10, "if we need schema we need schema"):
the rung list lived in a comment block inside a TOML file. Three attempts to
read it mechanically each produced a confident wrong answer — the accounting
tables downstream matched as 34 extra OPEN rungs; deduping on the rung id
truncated the ladder before phase 5 because ids are not unique (there are two
`4a` rungs); scoping to the file's own "── LADDER ─" banner dropped phase 5 on
cw-lift and found the wrong one of two ladders on sv-surface. Each fix was a
new heuristic over the same un-schema'd prose, which is the mole-whacking the
compass forbids. cw-lift's own file already records the cost: the instrument
that greps that block for DONE has been corrected TWICE for stale shas, and a
2026-09-09 entry says seven ladder shas do not resolve at all.

THE SCHEMA. `[[rung]]` entries in quality/campaigns/<id>.toml — the file is
already TOML, the rung list just was not:

    [[rung]]
    id     = "5c"
    title  = "commonwealth-work: codec, fold, one predicate, executor"
    status = "done"            # done | partial | open | descoped
    sha    = ["239f93354"]     # REQUIRED when done or partial
    demo   = "D3"              # which demo it moves; "none" is legal, and
                               # a rung that moves none should say why in note
    note   = "..."             # optional one line

WHAT THE TABLE OWNS AND WHAT THE PROSE OWNS. The table owns STATUS — it is the
one decider for "did this rung land" (§10.6). The comment block keeps the
RATIONALE, the long account of what each rung learned, which is worth more
than the status line and which no schema should try to hold. `migrate` writes
a header saying exactly that, so the next reader is not left guessing which of
the two to believe.

FOUR VERDICTS, NOT TWO (§18.1). `check` reports pass / fail / could-not-judge
(git unavailable, so a sha's existence is unknown) / never-ran, and a sha it
cannot resolve is `fail`, never a shrug.

  scripts/co-ladder.py check <campaign>      # validate the table
  scripts/co-ladder.py show <campaign>       # the table, one line per rung
  scripts/co-ladder.py migrate <campaign>    # emit [[rung]] from the prose, to STDOUT
"""
import os
import re
import subprocess
import sys
import tomllib

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STATUSES = ("done", "partial", "open", "descoped")


def _path(cid):
    return os.path.join(REPO, "quality", "campaigns", f"{cid}.toml")


def _load(cid):
    p = _path(cid)
    if not os.path.exists(p):
        sys.exit(f"co-ladder: no {p}")
    with open(p, "rb") as fh:
        return tomllib.load(fh), open(p, encoding="utf-8").read()


def _git_has(sha):
    """True / False / None — None means git could not answer (could-not-judge)."""
    try:
        r = subprocess.run(["git", "-C", REPO, "cat-file", "-e", f"{sha}^{{commit}}"],
                           capture_output=True, timeout=10)
        return r.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return None


def cmd_check(cid):
    data, _ = _load(cid)
    rungs = data.get("rung", [])
    if not rungs:
        print(f"co-ladder {cid}: NEVER-RAN — no [[rung]] table.")
        print(f"  The ladder is still prose. `co-ladder.py migrate {cid}` drafts one.")
        return 3
    problems, unknown, seen = [], [], {}
    for i, r in enumerate(rungs):
        who = r.get("id") or f"#{i}"
        for field in ("id", "title", "status"):
            if not r.get(field):
                problems.append(f"{who}: missing `{field}`")
        st = r.get("status")
        if st and st not in STATUSES:
            problems.append(f"{who}: status {st!r} not one of {'/'.join(STATUSES)}")
        # Ids repeat across phases in real ladders (two `4a` rungs on cw-lift),
        # so uniqueness is checked on (id, title), and a true duplicate is a
        # copy-paste rather than a phase collision.
        key = (r.get("id"), r.get("title"))
        if key in seen:
            problems.append(f"{who}: duplicate rung (same id AND title as row {seen[key]})")
        seen[key] = i
        shas = r.get("sha") or []
        if st in ("done", "partial") and not shas:
            problems.append(f"{who}: status {st} with no sha — an unevidenced landing")
        for sha in shas:
            ok = _git_has(sha)
            if ok is False:
                problems.append(f"{who}: sha {sha} does not resolve in this repo")
            elif ok is None:
                unknown.append(f"{who}: sha {sha} — git could not answer")
    # THE TWO-READERS CHECK, and the reason this file exists. The comment
    # ladder is still read by cw-net-deletion's instrument (`grep DONE`), so
    # until that instrument moves there are two readers of one fact (§10.6).
    # That is survivable ONLY while they agree, and cw-lift's own history says
    # they have not: two corrections for stale shas, plus a 2026-09-09 entry
    # recording seven shas that do not resolve. This makes the disagreement
    # loud instead of silent.
    disagree = []
    _, text = _load(cid)
    lines = text.splitlines()
    begin = next((i for i, l in enumerate(lines) if "── LADDER ─" in l), None)
    if begin is not None:
        stop = next((i for i in range(begin + 1, len(lines))
                     if re.match(r"^\s*(\[|[A-Za-z_][\w-]*\s*=)", lines[i])), len(lines))
        prose = {}
        marks = [(i, m.group(1)) for i in range(begin + 1, stop)
                 for m in [re.match(r"^#\s{2,4}([A-Za-z]?-?\d+[a-z]?)\s\s+(\S.*?)\s*$", lines[i])]
                 if m and not m.group(2).startswith("──")
                 and not re.match(r"^([0-9a-f]{7,9}|[+-]?[\d,]+)\b", m.group(2))]
        for k, (i, rid) in enumerate(marks):
            end_i = marks[k + 1][0] if k + 1 < len(marks) else stop
            blob = " ".join(lines[i:end_i])
            prose[rid] = ("descoped" if "DESCOPED" in blob else
                          "partial" if "PARTIAL" in blob else
                          "done" if "DONE" in blob else "open")
        for r in rungs:
            pv = prose.get(r.get("id"))
            if pv and pv != r.get("status"):
                disagree.append(f"{r['id']}: table says {r['status']}, "
                                f"the comment ladder reads {pv}")

    print(f"co-ladder check {cid}: {len(rungs)} rung(s)")
    for d in disagree:
        print(f"  DISAGREE  {d}")
    if disagree:
        print("            cw-net-deletion greps the comment block, so a rung the")
        print("            table calls done and the prose does not is a rung that")
        print("            bar is not counting. Fix the PROSE (add the sha to the")
        print("            rung's own row) — the table is the record.")
    for p in problems:
        print(f"  FAIL  {p}")
    for u in unknown:
        print(f"  COULD-NOT-JUDGE  {u}")
    if not problems and not unknown:
        by = {s: sum(1 for r in rungs if r.get("status") == s) for s in STATUSES}
        print("  PASS  " + ", ".join(f"{v} {k}" for k, v in by.items() if v))
        nodemo = [r["id"] for r in rungs if not r.get("demo")]
        if nodemo:
            print(f"  nudge: {len(nodemo)} rung(s) name no demo: {', '.join(nodemo[:8])}")
    return 1 if problems else 0


def cmd_show(cid):
    data, _ = _load(cid)
    rungs = data.get("rung", [])
    if not rungs:
        print(f"co-ladder {cid}: no [[rung]] table (the ladder is still prose)")
        return 3
    for r in rungs:
        st = (r.get("status") or "?").upper()
        sha = (r.get("sha") or [""])[0]
        print(f"  {st:<9} {r.get('id',''):<5} {(r.get('demo') or '-'):<5} "
              f"{sha:<10} {(r.get('title') or '')[:56]}")
    return 0


def cmd_migrate(cid):
    """Draft a [[rung]] table from the prose block. STDOUT, never written.

    Best-effort BY DESIGN and reviewed by a human before it lands: this is a
    one-time conversion, not a runtime parser, which is the whole point — the
    guessing happens once, in front of someone who can correct it, instead of
    on every read.
    """
    _, text = _load(cid)
    lines = text.splitlines()
    begin = next((i for i, l in enumerate(lines) if "── LADDER ─" in l), None)
    if begin is None:
        sys.exit(f"co-ladder: no '── LADDER ─' banner in {_path(cid)} — convert by hand")
    # TWO PASSES. A fixed line window bled the NEXT rungs' shas and verdicts
    # into every row (watched: rung -1 acquired four shas, three of them its
    # successors'). A rung's block runs from its own row to the next rung's
    # row, which is the file's actual structure.
    # The block ends where real TOML begins, NOT at the next banner: banners
    # like "# ── PHASE 5 · THE WORK PLANE ──" are section headers INSIDE the
    # ladder, and stopping at one silently dropped every phase-5 rung —
    # exactly the kind of partial reading presented as a whole one this
    # schema exists to end (§18.3). Comments and blanks continue; a line that
    # is a table header or a key/value pair is the boundary.
    end = next((i for i in range(begin + 1, len(lines))
                if re.match(r"^\s*(\[|[A-Za-z_][\w-]*\s*=)", lines[i])), len(lines))
    marks = []
    for i in range(begin + 1, end):
        m = re.match(r"^#\s{2,4}([A-Za-z]?-?\d+[a-z]?)\s\s+(\S.*?)\s*$", lines[i])
        if not m or m.group(2).startswith("──"):
            continue
        ttl = m.group(2)
        if re.match(r"^[0-9a-f]{7,9}\b", ttl) or re.match(r"^[+-]?[\d,]+\b", ttl):
            continue
        marks.append((i, m.group(1), ttl))
    out, n = [], 0
    print(f"# ── RUNG TABLE (migrated {cid}) ─────────────────────────────────────")
    print("# THE TABLE OWNS STATUS. The comment ladder above keeps the rationale —")
    print("# what each rung learned, which is worth more than a status line and")
    print("# which no schema should hold. When they disagree, the table is right")
    print("# about `status` and the prose is right about everything else.")
    print("# Validate with `scripts/co-ladder.py check %s`." % cid)
    for k, (i, rid, title) in enumerate(marks):
        stop = marks[k + 1][0] if k + 1 < len(marks) else end
        blob = " ".join(lines[i:stop])
        m = type("M", (), {"group": staticmethod(lambda n, _r=rid: _r)})
        status = ("descoped" if "DESCOPED" in blob else
                  "partial" if "PARTIAL" in blob else
                  "done" if "DONE" in blob else "open")
        shas = re.findall(r"\b([0-9a-f]{9})\b", blob)
        title = re.sub(r"\s{2,}[0-9a-f]{9}.*$", "", title).strip()
        title = re.sub(r"\s*(DONE|PARTIAL|OPEN)\b.*$", "", title).strip()
        n += 1
        out.append(f'\n[[rung]]\nid     = "{m.group(1)}"\n'
                   f'title  = "{title.replace(chr(34), chr(39))}"\n'
                   f'status = "{status}"\n'
                   f'sha    = [{", ".join(chr(34) + s + chr(34) for s in dict.fromkeys(shas))}]\n'
                   f'demo   = ""   # REVIEW: which demo does this rung move?')
    print("\n".join(out))
    print(f"\n# {n} rung(s) drafted. REVIEW EVERY ONE before appending — the parse is")
    print("# best-effort over prose, which is exactly why it happens once.",
          file=sys.stderr)
    return 0


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.rsplit("  scripts/co-ladder.py check", 1)[0].strip()
                 + "\n\n  check | show | migrate  <campaign>")
    cmd, cid = sys.argv[1], sys.argv[2]
    fn = {"check": cmd_check, "show": cmd_show, "migrate": cmd_migrate}.get(cmd)
    if not fn:
        sys.exit(f"co-ladder: unknown command {cmd!r}")
    sys.exit(fn(cid))


if __name__ == "__main__":
    main()
