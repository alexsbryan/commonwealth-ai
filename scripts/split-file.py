#!/usr/bin/env python3
"""File-split factory — the arch-gate burn-down, mechanised.

`cargo xtask arch-gate` names N files over ARCH §3.1's 1200-line ceiling. Each
one is then split by hand, and the hand work turned out to be the SAME four
passes every time (measured over doctor_cmd, enrich_cmd/eval, document_asset —
3042 / 3811 / 4073 lines, ~35 min each, all four passes identical):

  1. find the seams          banner comments, top-level items, impl methods
  2. walk boundaries back    a cut between a `///` and its item does not compile
  3. extract + re-import     one file per concern, `use super::*` for the unit
  4. widen visibility        top-level items, struct fields, INHERENT impl
                             methods (never trait impls) -> pub(super)

This runs those passes. It does not decide anything an agent should decide:
`plan` proposes and prints, a human or agent edits the plan, `apply` executes
exactly what the plan says.

WHY A TOOL AND NOT A CHECKLIST. The ledger's own cost model (quality/
REFACTOR_LEDGER.md, "Why this exists"): discovery is expensive and uncached, so
a checklist is re-derived every session and about ten items get done before the
session runs out of room. The plan below is the ledger's `order.md` for this
detector — self-contained, so the agent burning it makes no exploratory reads.

CLOSURE IS AN ABSENCE (ledger, "Closure is an absence, not a record"). Nothing
here writes progress. A file is open iff arch-gate still names it; the burn-down
is `cargo xtask arch-gate`, measured fresh, and this script never touches a
baseline. There is no state to reconcile after a crash because none is written.

  split-file.py plan   <path.rs> [--limit 1200] [--target 850]
  split-file.py apply  <plan.toml>
  split-file.py verify <path.rs>            # after apply: did anything vanish?
  split-file.py verify --self-test          # prove verify can FAIL

THE NEGATIVE CONTROL (ledger interlock 2; ARCH §18.1). The fake closure for
this detector is a split that drops code: the file count falls under 1200, the
gate goes green, and a function is gone. `verify` compares the top-level `fn`
set and the test-attribute count against git HEAD and exits non-zero on any
difference. `--self-test` deletes a function from a scratch copy and asserts
verify catches it — a check with no failing input you can name is not a check.
"""
from __future__ import annotations
import argparse, os, re, shutil, subprocess, sys, tempfile

try:
    import tomllib
except ModuleNotFoundError:                     # py<3.11
    tomllib = None

# ── Rust surface recognition ────────────────────────────────────────────────
# Brace-depth tracking at column 0. Not a parser: it does not need to be, because
# every construct it must find is unambiguous at column 0 in rustfmt'd code, and
# rustfmt is a pre-push gate here so the input is always formatted.

ITEM = re.compile(
    r'^(?:pub(?:\([a-z_]+\))?\s+)?'
    r'(?:default\s+|const\s+|async\s+|unsafe\s+|extern\s+"[^"]*"\s+)*'
    r'(fn|struct|enum|trait|impl|const|static|type|mod|macro_rules!)\b')
BANNER = re.compile(r'^//\s*[─=-]{3,}|^//\s*──')
STRUCT_HDR = re.compile(r'^(?:pub(?:\([a-z_]+\))?\s+)?struct\s+[A-Za-z_]')
IMPL_HDR = re.compile(r'^(?:pub(?:\([a-z_]+\))?\s+)?impl[\s<]')
TOP_FN = re.compile(r'^(?:async\s+)?fn\s+[a-z_0-9]+')
TOP_TY = re.compile(r'^(?:struct|enum|const|static|type)\s+')
METHOD = re.compile(r'^    (?:async\s+)?fn\s+[a-z_][a-z_0-9]*')
FIELD = re.compile(r'^    [a-z_][a-z_0-9]*\s*:\s')
CFG_TEST = re.compile(r'^\s*#\[cfg\(test\)\]')
TEST_ATTR = re.compile(r'^\s*#\[(?:tokio::)?test\]')


# Brace counting that ignores braces inside strings and comments.
#
# Not paranoia: `session_cmd.rs` holds prompt templates full of JSON, and a
# naive `line.count("{")` reported one 18-line function as 2,636 lines — a plan
# built on that would cut in the middle of a string literal. Raw strings
# (`r#"..."#`) span lines, so the scanner carries state across them.

def brace_delta(line, state):
    """(delta, new_state). `state` is None, ('raw', hashes) or ('str',)."""
    d, i, n = 0, 0, len(line)
    if state and state[0] == "raw":
        close = '"' + "#" * state[1]
        k = line.find(close)
        if k < 0:
            return 0, state
        i, state = k + len(close), None
    elif state and state[0] == "str":
        while i < n:
            if line[i] == "\\":
                i += 2; continue
            if line[i] == '"':
                i += 1; state = None; break
            i += 1
        else:
            return 0, state
    while i < n:
        c = line[i]
        if c == "/" and i + 1 < n and line[i + 1] == "/":
            break
        if c == "'":                      # char literal or lifetime
            if i + 2 < n and line[i + 1] == "\\":
                i += 4; continue
            if i + 2 < n and line[i + 2] == "'":
                i += 3; continue
            i += 1; continue
        if c == "r" and i + 1 < n and line[i + 1] in '#"':
            j = i + 1; h = 0
            while j < n and line[j] == "#":
                h += 1; j += 1
            if j < n and line[j] == '"':
                close = '"' + "#" * h
                k = line.find(close, j + 1)
                if k < 0:
                    return d, ("raw", h)
                i = k + len(close); continue
        if c == '"':
            i += 1
            while i < n:
                if line[i] == "\\":
                    i += 2; continue
                if line[i] == '"':
                    i += 1; break
                i += 1
            else:
                return d, ("str",)
            continue
        if c == "{":
            d += 1
        elif c == "}":
            d -= 1
        i += 1
    return d, state


def read(p):
    with open(p, encoding="utf-8") as f:
        return f.read().split("\n")


def walk_back(lines, n):
    """A cut at line n (1-indexed) must not orphan the doc comment or attribute
    block that belongs to the item there. Returns the true boundary."""
    i = n - 1
    while i - 1 >= 0 and lines[i - 1].lstrip().startswith(("///", "//!", "#[", "//")):
        i -= 1
    return i + 1


def top_level_items(lines):
    """[(start_line, kind, name, end_line)] for every column-0 item, 1-indexed
    inclusive. start_line is the item keyword, NOT its doc comment."""
    out, depth, i, st = [], 0, 0, None
    while i < len(lines):
        l = lines[i]
        if depth == 0 and st is None:
            m = ITEM.match(l)
            if m:
                kind = m.group(1)
                name = ""
                nm = re.search(r'\b(?:fn|struct|enum|trait|type|const|static|mod)\s+([A-Za-z_][A-Za-z_0-9]*)', l)
                if nm:
                    name = nm.group(1)
                elif kind == "impl":
                    im = re.search(r'impl(?:<[^>]*>)?\s+(?:([A-Za-z_][\w:]*)\s+for\s+)?([A-Za-z_][\w:]*)', l)
                    name = (im.group(2) if im else "impl")
                start = i + 1
                d = 0
                j = i
                # an item ends at the line where its braces close, or at the
                # first `;` line for brace-less items (type/const/use)
                opened, jst = False, st
                while j < len(lines):
                    dd, jst = brace_delta(lines[j], jst)
                    d += dd
                    if dd > 0 or "{" in lines[j]:
                        opened = opened or dd > 0
                    if opened and d <= 0:
                        break
                    if not opened and jst is None and lines[j].rstrip().endswith(";"):
                        break
                    j += 1
                out.append((start, kind, name, j + 1))
                st = jst
                i = j + 1
                continue
        dd, st = brace_delta(l, st)
        depth += dd
        if depth < 0:
            depth = 0
        i += 1
    return out


def cfg_test_start(lines):
    for i, l in enumerate(lines):
        if CFG_TEST.match(l):
            return i + 1
    return None


# ── plan ────────────────────────────────────────────────────────────────────

def slug(text, fallback):
    s = re.sub(r'[^a-z0-9]+', '_', text.lower()).strip('_')
    s = re.sub(r'^(the|a|an)_', '', s)
    return s[:28] or fallback


def plan(path, limit, target):
    lines = read(path)
    n = len(lines)
    items = top_level_items(lines)
    banners = [i + 1 for i, l in enumerate(lines) if BANNER.match(l)]
    tstart = cfg_test_start(lines)
    body_end = (tstart - 1) if tstart else n

    header_end = items[0][0] - 1 if items else n
    header_end = min(header_end, body_end)

    # Preferred cut points: banner lines, else item starts. Both walked back.
    cuts = sorted({walk_back(lines, b) for b in banners if header_end < b < body_end}
                  | {walk_back(lines, s) for (s, _, _, _) in items if header_end < s < body_end})

    # Greedy: accumulate until the next cut would exceed `target`.
    groups, cur = [], header_end + 1
    for c in cuts:
        if c - cur >= target:
            groups.append((cur, c - 1))
            cur = c
    if cur <= body_end:
        groups.append((cur, body_end))

    print(f"# split plan for {path}")
    print(f"# {n} lines | limit {limit} | {len(items)} top-level items | "
          f"{len(banners)} banner(s)" + (f" | tests at {tstart}" if tstart else ""))
    if tstart:
        tl = n - tstart + 1
        print(f"#\n# NOTE: {tl} lines ({100*tl//n}%) are the trailing #[cfg(test)] module.")
        print("# ARCH §3.2 rule 4 says MOVE THE TESTS WITH THE CODE. This plan leaves")
        print("# them in mod.rs; if a group owns most of them, set `tests = true` on it")
        print("# and move the relevant tests by hand. Sweeping tests into a bucket to")
        print("# make a number go down is raked gravel, not a split.")
    print(f'\nfile = "{path}"')
    print(f"header_end = {header_end}   # module doc + imports stay in mod.rs")
    if tstart:
        print(f"tests_start = {tstart}")
    over = []
    for (a, b) in groups:
        size = b - a + 1
        label = ""
        for i in range(a - 1, min(a + 6, b)):
            if BANNER.match(lines[i]):
                label = re.sub(r'^//\s*[─=-]*\s*|\s*[─=-]*\s*$', '', lines[i]).strip()
                break
        if not label:
            for (s, k, nm, _) in items:
                if s >= a:
                    label = nm or k
                    break
        name = slug(label, f"part_{a}")
        flag = "   # OVER LIMIT — needs an inner cut (see below)" if size > limit else ""
        print(f'\n[[group]]\nname = "{name}"\nrange = [{a}, {b}]   # {size} lines{flag}')
        print(f'doc = "TODO: one line saying what concern this module holds."')
        if size > limit:
            over.append((a, b, name))

    for (a, b, name) in over:
        inner = [(s, k, nm, e) for (s, k, nm, e) in items if a <= s <= b]
        big = [(s, k, nm, e) for (s, k, nm, e) in inner if e - s + 1 > limit and k == "impl"]
        print(f"\n# ── group '{name}' is over the limit ──")
        if big:
            for (s, k, nm, e) in big:
                print(f"#   `impl {nm}` spans {s}-{e} ({e-s+1} lines). Inherent impls MAY span")
                print(f"#   modules within a crate, so cut it at method boundaries and give each")
                print(f"#   part `wrap = \"{nm}\"`. Methods:")
                d, j = 0, s - 1
                for j in range(s - 1, e):
                    if METHOD.match(lines[j]) and (lines[j].count("{") or True):
                        d = sum(lines[x].count("{") - lines[x].count("}") for x in range(s - 1, j))
                        if d == 1:
                            print(f"#     {j+1:>6}  {lines[j].strip()[:76]}")
        else:
            print("#   Cut at one of these top-level item starts:")
            for (s, k, nm, e) in inner:
                print(f"#     {walk_back(lines, s):>6}  {k} {nm}  ({e-s+1} lines)")
    return 0


# ── apply ───────────────────────────────────────────────────────────────────

def pubify_top(text):
    return "\n".join(("pub(super) " + l) if (TOP_FN.match(l) or TOP_TY.match(l)) else l
                     for l in text.split("\n"))


def pubify_members(lines):
    """Struct fields and INHERENT impl methods -> pub(super).

    Two traps this exists to avoid, both hit by hand on the first two splits:
      * multi-line fn PARAMETERS are indented exactly like struct fields, so a
        bare `^    ident: ` rule rewrites function signatures into syntax errors.
        Hence the struct-body tracking.
      * a TRAIT impl's methods may not carry a visibility modifier at all, so
        `impl X for Y` is skipped entirely.
    """
    out, depth, in_s, in_i, is_trait = [], 0, False, False, False
    st = None
    nf = nm = 0
    for l in lines:
        if depth == 0 and st is None:
            if STRUCT_HDR.match(l):
                in_s, in_i = l.rstrip().endswith("{"), False
            elif IMPL_HDR.match(l):
                in_i, is_trait, in_s = True, (" for " in l), False
        if depth == 1 and in_s and FIELD.match(l) and "pub" not in l.split(":")[0]:
            l = "    pub(super) " + l[4:]
            nf += 1
        elif depth == 1 and in_i and not is_trait and METHOD.match(l) and "pub" not in l.split("fn")[0]:
            l = "    pub(super) " + l[4:]
            nm += 1
        out.append(l)
        dd, st = brace_delta(l, st)
        depth += dd
        if depth <= 0:
            depth, in_s, in_i, is_trait = 0, False, False, False
    return out, nf, nm


USE_SUPER = (
    "\n// One cooperating unit split for size (ARCH §3.2), not independent\n"
    "// modules: these files name each other's types. The import surface stays\n"
    "// in `mod.rs` rather than being duplicated across every part.\n"
    "use super::*;\n\n")


def apply(plan_path):
    if tomllib is None:
        sys.exit("apply needs python >= 3.11 (tomllib)")
    with open(plan_path, "rb") as f:
        cfg = tomllib.load(f)
    src = cfg["file"]
    lines = read(src)
    out_dir = src[:-3]
    groups = cfg["group"]
    header_end = cfg["header_end"]
    tests_start = cfg.get("tests_start")

    os.makedirs(out_dir, exist_ok=True)
    spdx = "// SPDX-License-Identifier: AGPL-3.0-or-later\n"
    used = set()
    for g in groups:
        a, b = g["range"]
        a, b = walk_back(lines, a), walk_back(lines, b + 1) - 1
        body = pubify_top("\n".join(lines[a - 1:b])).strip("\n")
        if g.get("wrap"):
            body = f'impl {g["wrap"]} {{\n{body}\n}}'
        if g.get("close_impl"):
            body += "\n}"
        doc = g.get("doc", "TODO: what concern this module holds.")
        text = spdx + "//! " + doc.replace("\n", "\n//! ") + "\n" + USE_SUPER + body + "\n"
        text_lines, nf, nm = pubify_members(text.split("\n"))
        p = os.path.join(out_dir, g["name"] + ".rs")
        with open(p, "w", encoding="utf-8") as f:
            f.write("\n".join(text_lines))
        used.add((a, b))
        print(f"  {g['name'] + '.rs':<26} {b-a+1:>5} lines ({a}-{b})  +{nf} fields +{nm} methods")

    covered = set()
    for (a, b) in used:
        covered |= set(range(a, b + 1))
    leftover = [i for i in range(header_end + 1, (tests_start or len(lines) + 1))
                if i not in covered]

    mod = ["\n".join(lines[:header_end])]
    mod += [f"mod {g['name']};" for g in sorted(groups, key=lambda g: g["name"])]
    mod.append("")
    mod.append("// Siblings reach each other through here. Narrow each re-export to what is")
    mod.append("// actually consumed from outside this module: a `pub(crate) use` that")
    mod.append("// re-exports nothing public enough is a compiler warning, and one that")
    mod.append("// re-exports more than any caller wants is a surface nobody asked for.")
    mod += [f"use {g['name']}::*;" for g in sorted(groups, key=lambda g: g["name"])]
    mod.append("")
    if leftover:
        runs, s = [], leftover[0]
        for x, y in zip(leftover, leftover[1:] + [None]):
            if y != (x + 1):
                runs.append((s, x))
                s = y
        for (a, b) in runs:
            mod.append("\n".join(lines[a - 1:b]))
    if tests_start:
        mod.append("\n".join(lines[tests_start - 1:]))
    with open(os.path.join(out_dir, "mod.rs"), "w", encoding="utf-8") as f:
        f.write("\n".join(mod).rstrip("\n") + "\n")
    print(f"  {'mod.rs':<26} {len('\n'.join(mod).split(chr(10)))    :>5} lines")
    os.remove(src)
    print(f"removed {src}")
    print("\nNEXT: `cargo build -p <crate>` and fix, in this order —")
    print("  1. unresolved names          -> a sibling needs re-exporting in mod.rs")
    print("  2. private field / method    -> re-run is unnecessary; pubify_members")
    print("                                  already ran, so these are cross-IMPL uses")
    print("  3. 'glob import doesn't reexport anything with visibility pub(crate)'")
    print("                               -> narrow that line to plain `use`")
    print("  4. unused import             -> narrow the re-export to real consumers")
    print("Then: cargo fmt -p <crate> && scripts/split-file.py verify " + out_dir)
    return 0


# ── verify — the negative control ───────────────────────────────────────────

def surface(text):
    fns, tests = set(), 0
    for l in text.split("\n"):
        m = re.match(r'^(?:pub(?:\([a-z_]+\))?\s+)?(?:async\s+)?fn\s+([a-z_0-9]+)', l)
        if m:
            fns.add(m.group(1))
        if TEST_ATTR.match(l):
            tests += 1
    return fns, tests


def verify(target, quiet=False):
    """Compare the split tree against the single file at git HEAD."""
    if os.path.isdir(target):
        src = target + ".rs"
        after = "\n".join("\n".join(read(os.path.join(target, f)))
                          for f in sorted(os.listdir(target)) if f.endswith(".rs"))
    else:
        src = target
        after = "\n".join(read(target))
    head = subprocess.run(["git", "show", f"HEAD:{src}"], capture_output=True, text=True)
    if head.returncode != 0:
        print(f"COULD-NOT-JUDGE — {src} is not at git HEAD, so there is nothing to")
        print("compare against. This is not a pass (ARCH §18.2).")
        return 3
    bf, bt = surface(head.stdout)
    af, at = surface(after)
    ok = True
    lost, gained = sorted(bf - af), sorted(af - bf)
    if lost:
        print(f"FAIL  {len(lost)} top-level fn(s) VANISHED: {', '.join(lost[:8])}")
        ok = False
    if gained:
        print(f"WARN  {len(gained)} fn(s) appeared: {', '.join(gained[:8])}")
    if bt != at:
        print(f"FAIL  test count changed: {bt} -> {at}")
        ok = False
    if ok and not quiet:
        print(f"PASS  {len(bf)} top-level fns and {bt} tests preserved exactly")
    return 0 if ok else 1


def self_test():
    """Prove verify FAILS on a split that drops code (ledger interlock 2)."""
    root = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                          capture_output=True, text=True).stdout.strip()
    cand = subprocess.run(["git", "ls-files", "*.rs"], cwd=root,
                          capture_output=True, text=True).stdout.split()
    victim = next((c for c in cand if len(read(os.path.join(root, c))) > 200
                   and surface("\n".join(read(os.path.join(root, c))))[0]), None)
    if not victim:
        print("COULD-NOT-JUDGE — no suitable file to mutate")
        return 3
    tmp = tempfile.mkdtemp()
    d = os.path.join(tmp, os.path.basename(victim)[:-3])
    os.makedirs(d)
    text = "\n".join(read(os.path.join(root, victim)))
    # drop the first top-level fn and everything to the next column-0 item
    lines = text.split("\n")
    items = top_level_items(lines)
    fn = next((it for it in items if it[1] == "fn"), None)
    if not fn:
        print("COULD-NOT-JUDGE — victim has no top-level fn")
        return 3
    s, _, name, e = fn
    mutated = lines[:s - 1] + lines[e:]
    with open(os.path.join(d, "mod.rs"), "w", encoding="utf-8") as f:
        f.write("\n".join(mutated))
    os.chdir(root)
    print(f"self-test: dropped `fn {name}` from a scratch copy of {victim}")
    # verify against HEAD of the real path
    saved = os.getcwd()
    rc = _verify_against(victim, "\n".join(mutated))
    shutil.rmtree(tmp, ignore_errors=True)
    os.chdir(saved)
    if rc == 1:
        print("PASS  the control fired — verify catches a dropped function.")
        return 0
    print("FAIL  verify did NOT catch a dropped function. It is not a check.")
    return 1


def _verify_against(src, after_text):
    head = subprocess.run(["git", "show", f"HEAD:{src}"], capture_output=True, text=True)
    if head.returncode != 0:
        return 3
    bf, bt = surface(head.stdout)
    af, at = surface(after_text)
    lost = sorted(bf - af)
    if lost:
        print(f"  verify says: FAIL — {len(lost)} fn(s) vanished ({', '.join(lost[:4])})")
        return 1
    if bt != at:
        print(f"  verify says: FAIL — test count {bt} -> {at}")
        return 1
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plan"); p.add_argument("path")
    p.add_argument("--limit", type=int, default=1200)
    p.add_argument("--target", type=int, default=850)
    a = sub.add_parser("apply"); a.add_argument("plan")
    v = sub.add_parser("verify"); v.add_argument("path", nargs="?")
    v.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.cmd == "plan":
        return plan(args.path, args.limit, args.target)
    if args.cmd == "apply":
        return apply(args.plan)
    if args.cmd == "verify":
        return self_test() if args.self_test else verify(args.path)


if __name__ == "__main__":
    sys.exit(main())
