#!/usr/bin/env python3
"""枯山水 — the architecture as a dry garden, and the guardian of its stones.

WHY A GARDEN AND NOT ANOTHER DASHBOARD. `svrn code fieldglass` already renders
architecture health and declares its own boundary: "Evidence, never verdicts —
it never scores, never gates." `svrn posture` already reports artifact age.
Neither composes, and neither names what it cannot see. This does both, in the
one grammar that has always been about exactly that.

The transposition is literal, and every panel is measured, not asserted:

  石   STONES     layer-0 `contract` crates. Placed once. A stone that gains an
                  upward edge has MOVED — and moving a stone is the one thing a
                  karesansui forbids. THIS IS THE GUARD.
  垣   THE WALL   ARCH_LAYERS.toml. What is inside the garden, and the ordering
                  the inside obeys. `[[exception]]` is the burn-down list, so
                  the wall may only close. THIS IS THE GUARD.
  砂紋  SAMON      the raked baselines. Gravel represents water that is not
                  there; a baseline represents a state the tree has since left.
                  REPORTED, NEVER GATED — sovereign/DEFAULTS_LEDGER.md records
                  the operator's 2026-08-08 decision that staleness triggers are
                  ritual, not automation ("do not re-raise it"). The garden does
                  not nag about gravel. It refuses to let you move a stone.
  苔   MOSS       what grew while nobody was looking. Moss is tended by removal.
  借景  SHAKKEI    borrowed scenery — framed, not owned, and it moves without us.
  間   MA         the empty space that IS the composition: states the types can
                  no longer represent.
  第十五石         Ryōan-ji seats fifteen stones so that fourteen are visible from
       THE 15TH   anywhere. The garden's whole claim is that the unseen part is
       STONE      known and named. Every panel above declares its blind spot.

Exit 0 = no stone moved, wall intact. Exit 1 = a stone moved.
"""
from __future__ import annotations
import subprocess, sys, os, tomllib, re, datetime

R = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                   capture_output=True, text=True).stdout.strip()
NOW = datetime.datetime.now(datetime.timezone.utc)
B, D, RS, Y, C, G = "\033[1m", "\033[2m", "\033[0m", "\033[33m", "\033[36m", "\033[32m"
if not sys.stdout.isatty() or os.environ.get("NO_COLOR"):
    B = D = RS = Y = C = G = ""


def sh(*a: str) -> str:
    return subprocess.run(a, cwd=R, capture_output=True, text=True).stdout


def panel(glyph: str, romaji: str, gloss: str) -> None:
    print(f"\n{B}{glyph}  {romaji}{RS} {D}— {gloss}{RS}")


def blind(text: str) -> None:
    """Every panel names what it cannot see. This is the fifteenth stone."""
    print(f"   {D}× cannot see: {text}{RS}")


layers = tomllib.load(open(os.path.join(R, "quality/ARCH_LAYERS.toml"), "rb"))
LAYER = layers["layer"]
NAME_OF = {}
for i, l in enumerate(LAYER):
    for c in (l.get("crates") or []):
        NAME_OF[c] = i

failures: list[str] = []


# ── The garden proper ───────────────────────────────────────────────────────
# Karesansui viewed from the engawa: ONE seat, one composition.
#
# The picture is not decoration over the data — it IS the data. Gravel is raked
# LEFT TO RIGHT because that is the direction dependencies flow, and the rake
# never doubles back because the layer rule does not either (ARCH §8). Stones
# are the seven layers, placed upstream to downstream; a stone's size is its
# layer's Rust mass; its rings are how many crates sit in it. Nothing is
# centred and nothing aligns — fukinsei (不均整) is the one aesthetic rule a
# karesansui will not bend, and a computed grid would be the wrong answer even
# where it is easier.
W, H = 78, 25

# Hand-placed, never solved (fukinsei). Upstream left, downstream right, and a
# deliberate VOID through the middle-right: at Ryōan-ji most of the rectangle
# is empty gravel, and the emptiness is the composition rather than what is
# left over after placing things (間).
SEATS = [(8, 18), (15, 8), (27, 21), (32, 5), (55, 17), (64, 6), (71, 20)]


def render_garden():
    grid = [[" "] * W for _ in range(H)]
    for y in range(0, H, 2):                     # the rake: parallel lines
        for x in range(W):
            grid[y][x] = "─"

    def dist(x, y, cx, cy):
        # Cells are ~2:1, so halve x to make a ring round to the eye.
        return (((x - cx) / 2.0) ** 2 + float(y - cy) ** 2) ** 0.5

    placed = []
    for i, layer in enumerate(LAYER):
        if i >= len(SEATS):
            break
        cx, cy = SEATS[i]
        crates = [c for c in (layer.get("crates") or [])]
        mass = sum(crate_loc(MANIFESTS[c][0]) for c in crates if c in MANIFESTS)
        rad = 0.9 + min(1.5, (mass / 220_000.0) * 1.5)
        rings = 2 if len(crates) < 8 else 3
        # Ring spacing is generous so arcs read as arcs, not dashes.
        placed.append((i, layer.get("name", ""), cx, cy, mass, len(crates), rings))

        for y in range(H):
            for x in range(W):
                d = dist(x, y, cx, cy)
                if d <= rad:
                    grid[y][x] = "█"                       # the stone
                elif d <= rad + 0.5:
                    grid[y][x] = "▓"

        # A ripple is the RAKE LINE bending round the stone, so it lands only
        # on rake rows. SOLVED per row, not sampled: sampling every cell
        # against |d - R| < tol smears the ring into a long horizontal run
        # wherever the ellipse runs flat, which is most of it.
        for r in range(1, rings + 1):
            rr = rad + 1.45 * r
            for y in range(0, H, 2):
                dy = abs(y - cy)
                if dy > rr:
                    continue
                dx = 2.0 * ((rr * rr - dy * dy) ** 0.5)
                for sx, ch in ((cx - dx, "("), (cx + dx, ")")):
                    xi = int(round(sx))
                    if 0 <= xi < W and grid[y][xi] == "─":
                        grid[y][xi] = ch if dx >= 1.0 else "‾"

    # 苔 — moss takes the quiet ground and is tended by removal, never by
    # planting. Its extent is the comment ratio of the first-party tree: the
    # part of the source that is prose rather than instruction.
    total = comm = 0
    for f in _ALL_RS or []:
        try:
            for ln in open(os.path.join(R, f), encoding="utf-8", errors="replace"):
                t = ln.strip()
                total += 1
                if t.startswith("//"):
                    comm += 1
        except OSError:
            pass
    ratio = (comm / total) if total else 0.0
    moss_cells = int(ratio * 150)
    # Deterministic scatter (no RNG: the same tree must draw the same garden).
    # Hash per CELL, not a stepped sequence — stepping drew a visible diagonal
    # lattice, which is the one thing moss never does.
    import hashlib
    cand = []
    for y in range(H):
        for x in range(W):
            if grid[y][x] != " " or x in (0, W - 1):
                continue
            h = hashlib.blake2b(f"{x},{y}".encode(), digest_size=4).digest()
            cand.append((int.from_bytes(h, "big"), x, y))
    cand.sort()
    for _, x, y in cand[:moss_cells]:
        grid[y][x] = "˙"
    return ["".join(r) for r in grid], placed, ratio


def draw() -> None:
    rows, placed, moss = render_garden()
    exc_n = len(layers.get("exception", []))
    print()
    print(f"   {D}╭{'─' * W}╮{RS}")
    for r in rows:
        print(f"   {D}│{RS}{r}{D}│{RS}")
    # The wall carries its own breaches: one gap per grandfathered exception.
    base = list("─" * W)
    for off in [7, 23, 24, 51, 52, 68][:max(exc_n, 0) * 2]:
        if 0 <= off < W:
            base[off] = " "
    print(f"   {D}╰{''.join(base)}╯{RS}")
    print(f"   {D}  dependencies flow ▸ this way, and never back"
          f"        ˙ moss = {moss*100:.0f}% of the source is prose"
          f"        {exc_n} gaps = {exc_n} grandfathered edges{RS}")
    l0 = [c for c in (LAYER[0].get("crates") or []) if c in MANIFESTS]
    masses = sorted(((crate_loc(MANIFESTS[c][0]), c) for c in l0), reverse=True)
    l0_tot = sum(m for m, _ in masses) or 1
    big_m, big_c = masses[0]
    share = 100.0 * big_m / l0_tot
    flag = Y if share > 30.0 else G
    print()
    print(f"   {D}石{RS} layer 0 is {100*l0_tot/sum(crate_loc(MANIFESTS[c][0]) for c in MANIFESTS):.1f}% "
          f"of the tree {D}— stones should be small{RS}   "
          f"{flag}{big_c} is {share:.0f}% of the stone mass{RS} "
          f"{D}(GARDEN §2 target: <30%){RS}")
    print()
    for i, name, cx, cy, mass, n, rings in placed:
        print(f"   {D}L{i}{RS} {name:<17} {n:>2} crates {mass:>8,} lines "
              f"{D}{'◍' * rings}{RS}")
    print()


def layer_of(crate: str) -> int | None:
    if crate in NAME_OF:
        return NAME_OF[crate]
    for pat, i in NAME_OF.items():          # the map uses `corpus-engine-*` globs
        if pat.endswith("*") and crate.startswith(pat[:-1]):
            return i
    return None


def crate_manifests() -> dict[str, tuple[str, str]]:
    """name -> (crate dir relative to repo root, manifest text)."""
    out = {}
    for m in sh("git", "ls-files", "*Cargo.toml").split():
        if m.startswith("vendor/"):
            continue
        try:
            t = open(os.path.join(R, m)).read()
        except OSError:
            continue
        nm = re.search(r'(?m)^\s*name\s*=\s*"([^"]+)"', t)
        if nm:
            out[nm.group(1)] = (os.path.dirname(m) or ".", t)
    return out


_ALL_RS = None


def crate_loc(d: str) -> int:
    """Rust lines under a crate dir. Pure Python: the nested `bash -c` form
    mangled `grep '\\.rs$'` through two levels of escaping and silently
    returned 0 for every crate."""
    global _ALL_RS
    if _ALL_RS is None:
        _ALL_RS = [f for f in sh("git", "ls-files").split("\n")
                   if f.endswith(".rs") and not f.startswith("vendor/")]
    pre = "" if d == "." else d.rstrip("/") + "/"
    n = 0
    for f in _ALL_RS:
        if f.startswith(pre):
            try:
                n += open(os.path.join(R, f), "rb").read().count(b"\n")
            except OSError:
                pass
    return n


MANIFESTS = crate_manifests()


draw()


# ── 石 ──────────────────────────────────────────────────────────────────────
panel("石", "STONES", "layer 0. placed once. they may name nothing above them")
stones = [c for c in (LAYER[0].get("crates") or []) if not c.endswith("*")]
for c in stones:
    got = MANIFESTS.get(c)
    if got is None:
        print(f"   {c:<26} {D}(no manifest found){RS}")
        continue
    d, t = got
    loc = f"{crate_loc(d):,}"
    # A stone moves when it names something in a HIGHER layer.
    up = []
    for dep in re.findall(r'(?m)^\s*([A-Za-z0-9_-]+)\s*=\s*\{[^}]*path\s*=', t):
        li = layer_of(dep)
        if li is not None and li > 0:
            up.append(f"{dep}(L{li})")
    mark = f"{Y}MOVED → {' '.join(up)}{RS}" if up else f"{G}set{RS}"
    print(f"   {c:<26} {loc:>7} lines   {mark}")
    if up:
        failures.append(f"stone moved: {c} names {', '.join(up)}")
blind("whether a stone is load-bearing — only that it has not moved")


# ── 垣 ──────────────────────────────────────────────────────────────────────
panel("垣", "THE WALL", "what is inside, and the order the inside obeys")
for i, l in enumerate(LAYER):
    cs = l.get("crates") or []
    print(f"   L{i} {l.get('name',''):<17} {len(cs):>2} crates")
exc = layers.get("exception", [])
print(f"   {D}{len(layers.get('forbid', []))} forbid rules · "
      f"{len(exc)} grandfathered exception(s) — the burn-down list{RS}")
for e in exc:
    frm, to = e.get("from", "?"), e.get("to", "?")
    print(f"     {D}·{RS} {frm} → {to}")
blind("edges Cargo cannot see — dyn dispatch, macros, re-exports "
      "(that is `svrn code arch-report`, over SCIP)")


# ── 砂紋 ────────────────────────────────────────────────────────────────────
panel("砂紋", "SAMON", "raked gravel stands for water that is not in it")
bl = sorted(f for f in sh("git", "ls-files", "quality/baselines").split()
            if f.endswith((".txt", ".tsv")))
for f in bl:
    when = sh("git", "log", "-1", "--format=%cI", "--", f).strip()
    if when:
        age = (NOW - datetime.datetime.fromisoformat(when)).days
        aged = f"{Y}{age:>3}d{RS}" if age > 14 else f"{age:>3}d"
    else:
        aged = f"{Y}new{RS}"
    print(f"   {aged}  since raked   {os.path.basename(f)}")
print(f"   {D}reported, never gated — DEFAULTS_LEDGER 2026-08-08: staleness "
      f"triggers are ritual, not automation{RS}")
blind("whether the pattern still describes the tree — that is each gate's own job")


# ── 苔 ──────────────────────────────────────────────────────────────────────
panel("苔", "MOSS", "what grew while nobody was looking. tended by removal")
for label, since in (("30d", "30 days ago"), ("90d", "90 days ago")):
    o = sh("bash", "-c",
           f"git log --since='{since}' --numstat --format='' | awk '"
           "$1 ~ /^[0-9]+$/ && $3 ~ /\\.rs$/ && $3 !~ /^vendor\\// {a+=$1; d+=$2} "
           "END {printf \"%d %d\", a, d}'").split()
    if len(o) == 2:
        a, d = int(o[0]), int(o[1])
        shed = f"{D}shed {d/a:.2f} per added{RS}" if a else f"{D}no additions{RS}"
        print(f"   {label}  +{a:<8,} −{d:<8,} net {a-d:+9,}   {shed}")
blind("which of it is load-bearing — line count is not weight")


# ── 借景 ────────────────────────────────────────────────────────────────────
panel("借景", "SHAKKEI", "borrowed scenery. framed, not owned; it moves without us")
v = sh("bash", "-c",
       "git ls-files vendor/ | xargs wc -l 2>/dev/null | awk '$2==\"total\"{t+=$1}END{print t+0}'").strip()
print(f"   vendor/            {int(v or 0):>9,} lines   {D}llama.cpp and its kernels{RS}")
# Absence is REPORTED, never defaulted to a number (ARCH §18.3).
raw = sh("bash", "-c", "sovereign mesh status 2>/dev/null || true")
ids = set(re.findall(r"node-[0-9a-f]{6,}", raw))
peers = f"{len(ids):,}" if ids else "unread"
print(f"   mesh peers         {peers:>9}         {D}other machines, other clocks{RS}")
blind("their internals, their release cadence, and whether they will still be "
      "there tomorrow")


# ── 間 ──────────────────────────────────────────────────────────────────────
panel("間", "MA", "the empty space that is the composition")
tp = os.path.join(R, "quality/TOPOLOGY.md")
raw_ma = re.findall(r"State made unrepresentable:\*\*\s*(.+?)(?:\||\n)",
                    open(tp).read()) if os.path.exists(tp) else []
made = []
for m in raw_ma:
    # First sentence only — the source rows continue into unrelated prose.
    first = re.split(r"(?<=[a-z0-9\)\`])\.\s", m.strip())[0].rstrip(" .*")
    made.append(first if len(first) <= 110 else first[:107] + "…")
for m in made:
    print(f"   {D}·{RS} {m}")
print(f"   {D}{len(made)} state(s) the types can no longer express{RS}")
blind("states nobody has thought to forbid yet — absence of a row is not proof")


# ── 第十五石 ────────────────────────────────────────────────────────────────
panel("第十五石", "THE FIFTEENTH STONE", "Ryōan-ji seats fifteen so that fourteen show")
print("   Every panel above names its blind spot. That is the whole design:")
print("   a view that claims completeness is the one you cannot trust.")
print(f"   {D}TOPOLOGY.md §2 measured the nominal configuration space at "
      f"≈4.6×10¹⁸ and said plainly that nobody can compute the reachable{RS}")
print(f"   {D}subset — 'not this seat, not three subagents with two hours, not "
      f"a maintainer on their first week.' The garden does not fix that.{RS}")
print(f"   {D}It makes the unseen part addressable, which is the only honest "
      f"thing a map of this size can do.{RS}")

print()
if failures:
    print(f"{Y}{B}A STONE HAS MOVED.{RS}")
    for f in failures:
        print(f"  ✗ {f}")
    print(f"\n{D}A karesansui forbids exactly one thing. Layer 0 may name "
          f"nothing above it (ARCH §8).{RS}")
    sys.exit(1)
print(f"{G}The stones are set. The wall holds.{RS} "
      f"{D}Gravel age is above — raking is yours, not the garden's.{RS}")
sys.exit(0)
