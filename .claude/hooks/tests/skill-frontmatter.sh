#!/bin/bash
# Every SKILL.md frontmatter must parse under a STRICT YAML parser.
#
# Why this exists: Claude Code's frontmatter parser is lenient, pi's (the `yaml`
# package, per the Agent Skills standard) is strict, and pi's documented rule is
# "skills with missing description are not loaded". So a description containing
# an unquoted `: ` parses fine here and makes the skill VANISH there — no error,
# no warning, the skill is simply absent from the system prompt.
#
# Measured 2026-08-18: `.claude/skills/comaintainer` carried `M0: every
# directive ...` unquoted. Under pi the resolved skill list was
# ["fieldglass","fleet-report","pi-subagents"] — the seat, the whole point of
# the directory, was the one skill missing. Quoting the scalar fixed it.
#
# The suite validates the standard's own limits too (name charset/length,
# description length), because those are what every conforming harness checks.
#
#   bash .claude/hooks/tests/skill-frontmatter.sh
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1

if ! python3 -c "import yaml" 2>/dev/null; then
    echo "  COULD-NOT-JUDGE: python3 yaml module absent — install pyyaml to run this gate"
    exit 2
fi

python3 - <<'PY'
import glob, re, sys, yaml

NAME_RE = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*$")
pass_n = fail_n = 0

def check(label, ok, detail=""):
    global pass_n, fail_n
    if ok:
        print(f"  ok   {label}"); pass_n += 1
    else:
        print(f"  FAIL {label}: {detail}"); fail_n += 1

def frontmatter(text):
    """Return the raw frontmatter block, or None when absent."""
    m = re.match(r"^---\n(.*?)\n---\n", text, re.S)
    return m.group(1) if m else None

paths = sorted(glob.glob(".claude/skills/*/SKILL.md"))
check("skills directory is non-empty", bool(paths), "no SKILL.md found")

for path in paths:
    name = path.split("/")[2]
    raw = frontmatter(open(path, encoding="utf-8").read())
    if raw is None:
        check(f"{name}: has frontmatter", False, "no leading --- block")
        continue
    try:
        meta = yaml.safe_load(raw)
    except yaml.YAMLError as e:
        # THE failure this suite exists for. Name the fix in the message —
        # whoever trips it is usually not thinking about a second harness.
        check(f"{name}: frontmatter parses under strict YAML", False,
              f"{str(e).splitlines()[0]} — quote the scalar (a bare ': ' in "
              f"description breaks it; pi then drops the skill silently)")
        continue
    check(f"{name}: frontmatter parses under strict YAML", True)

    if not isinstance(meta, dict):
        check(f"{name}: frontmatter is a mapping", False, type(meta).__name__)
        continue
    n, d = meta.get("name"), meta.get("description")
    check(f"{name}: has name", bool(n), "missing")
    check(f"{name}: has description", bool(d),
          "missing — a harness that requires it will not load this skill")
    if n:
        check(f"{name}: name charset/length", bool(NAME_RE.match(n)) and len(n) <= 64, repr(n))
    if d:
        check(f"{name}: description <= 1024 chars", len(d) <= 1024, f"{len(d)} chars")

# Negative control (ARCH_PRINCIPLES §18.1 — a check you have not watched fail
# is not a gate). The exact shape that shipped broken must still be rejected.
try:
    yaml.safe_load('description: Draft first. M0: every directive is a draft.')
    check("negative control: unquoted ': ' is rejected", False,
          "strict parser ACCEPTED the known-bad frontmatter — this gate is blind")
except yaml.YAMLError:
    check("negative control: unquoted ': ' is rejected", True)

print(f"\npass: {pass_n} fail: {fail_n}")
sys.exit(1 if fail_n else 0)
PY
