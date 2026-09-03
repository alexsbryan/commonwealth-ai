#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""
EVIDENCE VERDICT — does the test a commit cites actually see the change?

A commit body that says "Verified: `some_test` passes" is citing that test as
EVIDENCE for the change. The sentence can be true and the citation still
empty: commit 1c5de2d52 cites a bundled-recipe smoke test as proof that a
`[parameters.ticker]` block parses, and reverting every source file the commit
touched leaves that test green in 0.01s — the test cannot see the change it is
cited for. A careful human adjudicator (probe A, note e911132b) had scored
that commit a full argument. This script is the mechanism that caught it.

THE PROTOCOL (R2, note 450cf611), generalised and run at the cited commit:

  1. Resolve the tests the body NAMES: a fn that is `#[test]`-shaped at that
     commit, a `*.rs` file under a tests/ dir, or `scripts/x.py --self-test`.
  2. In a worktree at the commit — never at HEAD; hunks apply cleanly only
     against their own parent — run the named tests. POSITIVE CONTROL.
  3. Reverse-apply the commit's SOURCE hunks, keeping its TEST hunks (files
     under tests/, `#[cfg(test)]` regions, fixtures, and Cargo manifests).
     Require it to compile. Re-run the named tests.

  VALIDATED        control passed, mutant FAILED — the test sees the change.
  UNSUPPORTED      control passed, mutant PASSED — the citation is empty.
  COULD-NOT-JUDGE  control failed, or no compiling mutant could be built.
  NEVER-RAN        the citation did not resolve to a test that ran.

Never two verdicts where four are needed (ARCH §18.1-18.3): an UNSUPPORTED is
an accusation, and it is only ever made when the control ran green and the
mutant built. Everything else is reported as the limit it is.

THE API RETREAT. All four of R2's could-not-judges were E0609/E0599/E0425/
E0560: the kept test calls API the same commit introduced, so reverting the
source cannot compile. That is what red-first development looks like, and it
is the common case here (8 of 10 test-naming commits claim red-first). The
escape R2 built by hand — revert the BEHAVIOUR, keep the API — is mechanised:
when the mutant does not build, the identifiers rustc names are looked up in
the reverted hunks, the hunks that INTRODUCE them are restored, and the build
is retried. Each retry costs one failed build; the loop is bounded. When every
source hunk has been restored there is no behaviour left to revert, and that
is reported as COULD-NOT-JUDGE, not laundered into either verdict. The
restored hunks are named in the record.

COST. One worktree, reused across commits so the target dir stays warm;
`-p <crate>` scoping so a verdict costs the crate's chain, not 164 test
binaries. Concurrency comes from scripts/lib/cargo-jobs.sh, the one decider.

  scripts/evidence-verdict.py --candidates --range HEAD~300..HEAD > cands.jsonl
  scripts/evidence-verdict.py --static --from cands.jsonl          # seconds, no build
  scripts/evidence-verdict.py 1c5de2d52 ee157d61f --jsonl out.jsonl
  scripts/evidence-verdict.py --self-test
"""
import argparse, ast, json, os, re, shlex, shutil, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Own config, own profile (see the file's header for why the repo's cannot be
# reused): junit on, retries OFF, fail-fast off. Passed explicitly so every
# commit, however old, runs under the same profile.
NEXTEST_CONFIG = ROOT / "scripts" / "lib" / "evidence-nextest.toml"
PROFILE = "evidence"
CARGO_TIMEOUT = 90 * 60   # a cold crate chain at 2 jobs on a loaded box; a hang still ends
MAX_RETREAT = 8

VALIDATED, UNSUPPORTED, CNJ, NEVER_RAN = ("VALIDATED", "UNSUPPORTED",
                                         "COULD-NOT-JUDGE", "NEVER-RAN")


def log(msg):
    print(time.strftime("%H:%M:%S ") + msg, file=sys.stderr, flush=True)


def git(*args, cwd=None, check=True) -> str:
    r = subprocess.run(["git", *args], cwd=cwd or ROOT, capture_output=True,
                       text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {r.stderr.strip()[:400]}")
    return r.stdout


# ── citations ──────────────────────────────────────────────────────────────
IDENT_RE = re.compile(r"\b([a-z][a-z0-9]*(?:_[a-z0-9]+){2,})\b")
RS_FILE_RE = re.compile(r"\b([A-Za-z0-9_./-]+\.rs)\b")
SELFTEST_RE = re.compile(r"\b(scripts/[A-Za-z0-9_.-]+\.py)\s+--self-test\b")
TEST_ATTR_RE = re.compile(
    r"#\[\s*(?:[a-z_]+::)*(?:test|rstest|test_case|tokio::test)\b")


def _attr_run_above(text: str, pos: int) -> list:
    """The run of attribute/comment/blank lines DIRECTLY above the fn at
    `pos`, nearest first. Not "the N lines above": that reads across the
    previous fn and made every fn after a test look like one, and made the
    helpers in tests/common/mod.rs (`model_id_for`) count as tests."""
    run = []
    for l in reversed(text[:pos].splitlines()):
        s = l.strip()
        if s and not (s.startswith("//") or s.startswith("#[") or s.startswith("#![")
                      or s.endswith(")]") or s.endswith(",")):
            break
        run.append(s)
    return run


def _test_attr_above(text: str, pos: int) -> bool:
    return any(TEST_ATTR_RE.search(s) for s in _attr_run_above(text, pos))


FN_RE = r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn {}\s*[(<]"


def is_test_fn(text: str, name: str) -> bool:
    m = re.search(FN_RE.format(re.escape(name)), text, re.M)
    return bool(m) and _test_attr_above(text, m.start())


def test_fns_in(text: str) -> list:
    return [m.group(1) for m in re.finditer(FN_RE.format("([a-z_][a-z0-9_]*)"), text, re.M)
            if _test_attr_above(text, m.start())]


def presented_test_fns(commit: str) -> list:
    """(name, path, how) for every test fn the commit ADDED or whose body it
    MODIFIED — the tests a commit presents as its own evidence even when the
    body never spells their names."""
    out = []
    for line in git("diff", "--name-status", "--no-renames", f"{commit}^", commit).splitlines():
        st, path = line.split("\t", 1)
        if st not in ("A", "M") or not path.endswith(".rs"):
            continue
        post = file_at(commit, path) or ""
        pre = set(test_fns_in(file_at(f"{commit}^", path) or ""))
        _, hunks = parse_hunks(git("diff", "-U0", "--no-color", "--no-renames",
                                   f"{commit}^", commit, "--", path))
        for name, a, b in test_fn_regions(post):
            if name not in pre:
                out.append((name, path, "added"))
            elif any(a <= h["new_start"] + max(h["new_len"], 1) - 1 and h["new_start"] <= b
                     for h in hunks):
                out.append((name, path, "modified"))
    return out


TEST_CLAIM_RE = re.compile(r"\b(?:tests?|pins?|red-first|goldens?|watched|negative control)\b", re.I)

# A test NAMED in a body is not always CITED as evidence. Measured on the first
# static triage (2026-09-01, 15 suspects): six named a known flake or an
# unrelated failure — "1 unrelated environmental flake (`x` ...; passes
# alone)", "NOT this diff", "passed 2/2 in isolation", "the sibling x is
# untouched". The disclaimer wins over the evidence words in the same sentence
# because it is the author saying, in so many words, that the test is not
# evidence for this change.
MENTION_RE = re.compile(
    r"\b(?:flak(?:e|y|es|ed)|deflake|unrelated|untouched|pre-existing|environmental|"
    r"in isolation|passes alone|not this diff|known (?:contention|flake|red)|"
    r"was (?:already )?red|still red|is not green|failed once|load-induced|ignore[d]?\b|#\[ignore\]|"
    r"\d+\s*pass\w*\s*/\s*\d+\s*fail)", re.I)   # "9454 pass / 3 fail — the three are ..." lists reds
EVIDENCE_RE = re.compile(
    r"\b(?:verified|passes|pass\b|green|pins?|pinned|re-pinned|red-first|proves?|"
    r"fails? without|watched|asserts?|golden|negative control|reads `?left)", re.I)
SENTENCE_SPLIT = re.compile(r"(?<=[.!?])\s+|\n\s*\n")


def citation_role(body: str, needle: str) -> tuple:
    """(role, sentence) for the sentence of `body` that names `needle`."""
    for s in SENTENCE_SPLIT.split(body):
        if needle in s:
            sent = " ".join(s.split())
            role = ("mention" if MENTION_RE.search(sent) else
                    "evidence" if EVIDENCE_RE.search(sent) else "unclear")
            return role, sent[:240]
    return "unclear", ""


def file_at(commit: str, path: str):
    r = subprocess.run(["git", "show", f"{commit}:{path}"], cwd=ROOT,
                       capture_output=True, text=True, errors="replace")
    return r.stdout if r.returncode == 0 else None


_CRATE = {}


def crate_of(commit: str, path: str):
    """(package name, manifest dir) of the crate owning `path` at `commit`."""
    parts = Path(path).parts
    for i in range(len(parts) - 1, 0, -1):
        d = "/".join(parts[:i])
        key = (commit, d)
        if key not in _CRATE:
            t = file_at(commit, f"{d}/Cargo.toml")
            name = None
            if t and "[package]" in t:
                m = re.search(r"^\s*name\s*=\s*\"([^\"]+)\"", t.split("[package]", 1)[1], re.M)
                name = m.group(1) if m else None
            _CRATE[key] = name
        if _CRATE[key]:
            return _CRATE[key], d
    return None, None


def cited_tests(commit: str, body: str) -> list:
    """Every test the body names, resolved AT THE COMMIT. A name that is a fn
    but not a test is not a citation of evidence and is dropped.

    Three explicit forms: a test fn's name, a `*.rs` file under tests/, and
    `scripts/x.py --self-test` (or a bare `--self-test` when the commit
    touches exactly the scripts that have one). One INFERRED form, flagged
    `inferred` in the record: when the body claims a test and the commit ADDS
    test fns, those fns are the evidence it is presenting. R2's ae94f300a
    ("Pins: expired-survives-sweep; ...") and eb726eea8 ("integration test
    pins the fold") name their tests in prose no grep resolves; the inferred
    form reaches both. An inferred citation is judged only when there is no
    explicit one, so it can never mask an empty explicit citation — the
    founding-datum class — with a new test that happens to fail."""
    cites, seen = [], set()
    tracked = None

    def ls():
        nonlocal tracked
        if tracked is None:
            tracked = git("ls-tree", "-r", "--name-only", commit).splitlines()
        return tracked

    for name in sorted(set(IDENT_RE.findall(body))):
        hits = git("grep", "-l", "-F", f"fn {name}", commit, "--", "*.rs",
                   check=False).strip().splitlines()
        for h in hits:
            path = h.split(":", 1)[1]
            text = file_at(commit, path) or ""
            if is_test_fn(text, name):
                pkg, _ = crate_of(commit, path)
                key = ("rust", name, path)
                if pkg and key not in seen:
                    seen.add(key)
                    role, sent = citation_role(body, name)
                    cites.append({**citation(name, path, pkg), "role": role, "context": sent})
    for f in sorted(set(RS_FILE_RE.findall(body))):
        base = f.split("/")[-1]
        for p in ls():
            if p.endswith("/" + base) and "/tests/" in p and f in p:
                text = file_at(commit, p) or ""
                pkg, _ = crate_of(commit, p)
                role, sent = citation_role(body, f)
                for name in test_fns_in(text):
                    key = ("rust", name, p)
                    if pkg and key not in seen:
                        seen.add(key)
                        cites.append({**citation(name, p, pkg), "via_file": f, "role": role,
                                      "context": sent})
    selftests = set(SELFTEST_RE.findall(body))
    if not selftests and "--self-test" in body:
        changed = git("diff", "--name-only", f"{commit}^", commit).splitlines()
        selftests = {p for p in changed if p.startswith("scripts/") and p.endswith(".py")
                     and "--self-test" in (file_at(commit, p) or "")}
    for s in sorted(selftests):
        if s in ls() and ("py", s) not in seen:
            seen.add(("py", s))
            role, sent = citation_role(body, "--self-test")
            cites.append({"kind": "py", "name": "--self-test", "file": s, "crate": None,
                          "role": role, "context": sent})
    if TEST_CLAIM_RE.search(body):
        for name, path, how in presented_test_fns(commit):
            key = ("rust", name, path)
            pkg, _ = crate_of(commit, path)
            if pkg and key not in seen:
                seen.add(key)
                cites.append({**citation(name, path, pkg), "inferred": f"{how} by this commit"})
    return cites


def citation(name: str, path: str, pkg: str) -> dict:
    """THE JOIN KEY IS THE SILENT FAILURE (note 33066b57): 99 fn names are
    duplicated across this workspace. A citation is therefore pinned to its
    crate always, and to its nextest binary id when the file is an
    integration test (`<crate>/tests/<stem>.rs` -> `<crate>::<stem>`), so the
    filter and the report join on the narrowest key the file path gives."""
    c = {"kind": "rust", "name": name, "file": path, "crate": pkg}
    parts = Path(path).parts
    if len(parts) >= 2 and parts[-2] == "tests" and path.endswith(".rs"):
        c["binary"] = f"{pkg}::{Path(path).stem}"
    return c


# ── hunks and their roles ──────────────────────────────────────────────────
TEST, SOURCE, KEEP = "TEST", "SOURCE", "KEEP"
FIXTURE_DIRS = ("/tests/", "/test-fixtures/", "/fixtures/", "/goldens/", "/testdata/",
                "/snapshots/", "/__snapshots__/")
FIXTURE_EXT = (".golden", ".snap", ".expected")
KEEP_FILES = ("Cargo.toml", "Cargo.lock")


def file_role(path: str):
    p = f"/{path}"
    name = path.split("/")[-1]
    if name in KEEP_FILES:
        return KEEP
    if any(d in p for d in FIXTURE_DIRS) or name.endswith(FIXTURE_EXT):
        return TEST
    return None  # decided per hunk


def block_end(text: str, i: int) -> int:
    """Index of the `}` matching the `{` at `i`, skipping string literals and
    comments; -1 when unbalanced. Good enough for `mod tests {` and fn bodies."""
    depth, j, n = 0, i, len(text)
    while j < n:
        c = text[j]
        if c == "/" and text.startswith("//", j):
            j = text.find("\n", j)
            j = n if j < 0 else j
            continue
        if c == "/" and text.startswith("/*", j):
            k = text.find("*/", j + 2)
            j = n if k < 0 else k + 2
            continue
        if c == '"':
            k = j + 1
            while k < n and text[k] != '"':
                k += 2 if text[k] == "\\" else 1
            j = k + 1
            continue
        if c == "'" and j + 2 < n and text[j + 2] == "'" and text[j + 1] != "\\":
            j += 3
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return j
        j += 1
    return -1


def brace_regions(text: str, opener_re) -> list:
    """(start_line, end_line) of every `opener {...}` block."""
    regions = []
    for m in opener_re.finditer(text):
        i = text.find("{", m.end())
        j = block_end(text, i) if i >= 0 else -1
        if j >= 0:
            regions.append((text.count("\n", 0, m.start()) + 1, text.count("\n", 0, j) + 1))
    return regions


def test_fn_regions(text: str) -> list:
    """(name, start_line, end_line) of every test fn, attribute lines included."""
    out = []
    for m in re.finditer(FN_RE.format("([a-z_][a-z0-9_]*)"), text, re.M):
        run = _attr_run_above(text, m.start())
        if not any(TEST_ATTR_RE.search(s) for s in run):
            continue
        i = text.find("{", m.end())
        j = block_end(text, i) if i >= 0 else -1
        if j >= 0:
            fn_line = text.count("\n", 0, m.start()) + 1
            out.append((m.group(1), fn_line - len(run), text.count("\n", 0, j) + 1))
    return out


CFG_TEST_MOD = re.compile(r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*")
TEST_FN_RS = re.compile(r"#\[\s*(?:[a-z_]+::)*(?:test|rstest|tokio::test)\b[^\n]*\n(?:[ \t]*#\[[^\n]*\n)*[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+[A-Za-z_][A-Za-z0-9_]*\s*(?:<[^>]*>)?\s*\([^)]*\)[^{]*")


def test_regions_rs(text: str) -> list:
    return brace_regions(text, CFG_TEST_MOD) + brace_regions(text, TEST_FN_RS)


PY_TEST_NAME = re.compile(r"self_?test|^_?test|battery|fixture|golden", re.I)


def test_regions_py(text: str) -> list:
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return []
    out = []
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and PY_TEST_NAME.search(node.name):
            out.append((node.lineno, node.end_lineno))
        elif isinstance(node, ast.Assign):
            names = [t.id for t in node.targets if isinstance(t, ast.Name)]
            if any(PY_TEST_NAME.search(n) for n in names):
                out.append((node.lineno, node.end_lineno))
    return out


HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@", re.M)


def parse_hunks(diff: str):
    """-> (header, [hunk]) for one file's `git diff -U0` output."""
    first = HUNK_RE.search(diff)
    if not first:
        return diff, []
    header = diff[:first.start()]
    hunks, ms = [], list(HUNK_RE.finditer(diff))
    for k, m in enumerate(ms):
        end = ms[k + 1].start() if k + 1 < len(ms) else len(diff)
        body = diff[m.start():end]
        new_start, new_len = int(m.group(3)), int(m.group(4) or "1")
        added = "\n".join(l[1:] for l in body.splitlines()[1:] if l.startswith("+"))
        removed = "\n".join(l[1:] for l in body.splitlines()[1:] if l.startswith("-"))
        hunks.append({"text": body, "new_start": new_start, "new_len": new_len,
                      "added": added, "removed": removed})
    return header, hunks


def inside(hunk, regions) -> bool:
    lo = hunk["new_start"]
    hi = lo + max(hunk["new_len"], 1) - 1
    return any(a <= lo and hi <= b for a, b in regions)


def classify(commit: str) -> list:
    """Every file the commit changed, with a role per hunk."""
    parent = f"{commit}^"
    out = []
    status = git("diff", "--name-status", "--no-renames", parent, commit)
    for line in status.splitlines():
        st, path = line.split("\t", 1)
        diff = git("diff", "-U0", "--no-color", "--no-renames", parent, commit, "--", path)
        header, hunks = parse_hunks(diff)
        binary = "Binary files" in header or "GIT binary patch" in diff
        role = file_role(path)
        if binary:
            role = KEEP
        if role is None:
            post = file_at(commit, path) or ""
            regions = (test_regions_rs(post) if path.endswith(".rs")
                       else test_regions_py(post) if path.endswith(".py") else [])
            for h in hunks:
                h["role"] = TEST if regions and inside(h, regions) else SOURCE
        else:
            for h in hunks:
                h["role"] = role
        for h in hunks:
            h["id"] = f"{path}@{h['new_start']}"
        out.append({"path": path, "status": st, "header": header, "hunks": hunks,
                    "binary": binary})
    return out


# ── static overlap: the pre-filter, never a verdict ────────────────────────
# The founding datum was spottable by grep: the cited test's file contains
# nothing the commit changed. Generalised, per re-registered bar (note
# "RE-REGISTRATION — intent-model MOVE 1 bar", 2026-09-01): the identifiers a
# commit's SOURCE hunks touch, against the cited test's fn body and file.
#
#   suspect  nothing in the whole test FILE names anything the change touched
#   weak     the file does, the cited fn's body does not
#   reaches  the fn body names something the change touched
#
# It cannot see a test that calls the changed fn and asserts nothing about the
# changed behaviour; only the build-based gate can. So it names candidates to
# build, and its precision is measured, not assumed.
STATIC_TOKEN = re.compile(r"\b(?:[a-z][a-z0-9]*(?:_[a-z0-9]+)+|[A-Z][a-z0-9]+(?:[A-Z][a-z0-9]+)+)\b")
STATIC_NOISE = {
    "to_string", "to_owned", "as_str", "as_ref", "as_mut", "is_some", "is_none", "is_empty",
    "is_ok", "is_err", "into_iter", "iter_mut", "unwrap_or", "unwrap_err", "unwrap_or_else",
    "unwrap_or_default", "map_err", "and_then", "ok_or", "from_str", "to_vec", "sort_by",
    "starts_with", "ends_with", "with_context", "read_to_string", "write_all", "from_utf8",
    "assert_eq", "assert_ne", "cfg_attr", "temp_dir", "current_dir", "manifest_dir",
    "PathBuf", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "TempDir", "VecDeque",
    "Serialize", "Deserialize", "PartialEq", "PartialOrd", "SocketAddr", "IpAddr",
    "TcpListener", "OnceCell", "OnceLock", "LazyLock", "NonZero", "Utc", "DateTime",
}


def tokens(text: str) -> set:
    return {t for t in STATIC_TOKEN.findall(text) if len(t) >= 6 and t not in STATIC_NOISE}


def source_surface(files: list) -> tuple:
    """(tokens, stems) the commit's SOURCE hunks touch. Docs are excluded: a
    .md hunk describing the change names everything and executes nothing.
    Stems count only when distinctive (>=8 chars with `-` or `_`): `recipe`
    and `registry` would match every test that loads any recipe."""
    src, stems = [], set()
    for f in files:
        if f["path"].endswith((".md", ".txt")):
            continue
        hs = [h for h in f["hunks"] if h["role"] == SOURCE]
        if not hs:
            continue
        src += [h["added"] + "\n" + h["removed"] for h in hs]
        stem = Path(f["path"]).stem
        if len(stem) >= 8 and ("-" in stem or "_" in stem):
            stems.add(stem)
    return tokens("\n".join(src)), stems


def overlap_class(touched: set, stems: set, file_text: str, fn_body: str) -> tuple:
    fo = sorted((touched & tokens(file_text)) | {s for s in stems if s in file_text})
    no = sorted((touched & tokens(fn_body)) | {s for s in stems if s in fn_body})
    return ("suspect" if not fo else "weak" if not no else "reaches"), fo, no


def static_overlap(commit: str, cites: list) -> dict:
    """Commit-level class over the explicit Rust citations whose role is not
    `mention`. `no-source` when the commit's source hunks carry no token at
    all (a test-only commit): nothing to overlap with, nothing to revert."""
    files = classify(commit)
    touched, stems = source_surface(files)
    per, mentions = [], []
    for c in cites:
        if c["kind"] != "rust" or c.get("inferred"):
            continue
        if c.get("role") == "mention":
            mentions.append({"name": c["name"], "context": c.get("context", "")})
            continue
        text = file_at(commit, c["file"]) or ""
        region = next(((a, b) for n, a, b in test_fn_regions(text) if n == c["name"]), None)
        body = "\n".join(text.splitlines()[region[0] - 1:region[1]]) if region else ""
        cls, fo, no = overlap_class(touched, stems, text, body)
        per.append({"name": c["name"], "file": c["file"], "class": cls, "role": c.get("role"),
                    "context": c.get("context", ""), "file_overlap": fo[:15], "fn_overlap": no[:15]})
    classes = [p["class"] for p in per]
    cls = ("unjudged" if not per else "no-source" if not touched
           else "suspect" if "suspect" in classes
           else "weak" if "weak" in classes else "reaches")
    return {"static": cls, "touched": len(touched), "stems": sorted(stems),
            "source_hunks": sum(h["role"] == SOURCE for f in files for h in f["hunks"]),
            "citations": per, "mentions": mentions}


# ── worktree ───────────────────────────────────────────────────────────────
def ensure_worktree(wt: Path, commit: str):
    if not (wt / ".git").exists():
        log(f"creating worktree {wt}")
        git("worktree", "add", "--detach", str(wt), commit)
    git("checkout", "-q", "-f", "--detach", commit, cwd=wt)
    # -x is deliberately absent: target/ is ignored and warm, and the point of
    # one reused worktree is to keep it that way.
    git("clean", "-fdq", cwd=wt)


def apply_mutant(wt: Path, commit: str, files: list, restored: set) -> list:
    """Reverse-apply every SOURCE hunk not in `restored`. -> reverted hunk ids."""
    git("checkout", "-q", "-f", commit, "--", ".", cwd=wt)
    git("clean", "-fdq", cwd=wt)
    reverted = []
    for f in files:
        chosen = [h for h in f["hunks"] if h["role"] == SOURCE and h["id"] not in restored]
        if not chosen:
            continue
        patch = f["header"] + "".join(h["text"] for h in chosen)
        r = subprocess.run(["git", "apply", "-R", "--unidiff-zero", "--whitespace=nowarn", "-"],
                           cwd=wt, input=patch, capture_output=True, text=True)
        if r.returncode != 0:
            raise RuntimeError(f"reverse-apply failed for {f['path']}: {r.stderr.strip()[:300]}")
        reverted += [h["id"] for h in chosen]
    return reverted


# ── runners ────────────────────────────────────────────────────────────────
def jobs_budget(override):
    r = subprocess.run(["bash", "-c", 'source "$1"; resolve_cargo_jobs "$2" || exit 2; '
                        'echo "$CARGO_JOBS|$CARGO_JOBS_REASON"', "_",
                        str(ROOT / "scripts/lib/cargo-jobs.sh"), override or ""],
                       capture_output=True, text=True)
    n, _, why = r.stdout.strip().partition("|")
    return (int(n) if n.isdigit() else 0), why


def resolve_features(wt: Path, pkg: str):
    """The gate's feature contract for `pkg`, from the ONE decider
    (scripts/lib/cargo-scope.sh), filtered to what cargo will accept for a
    `-p` build: a `dep/feat` flag is legal only when dep is the selected
    package or a DIRECT dependency of it. cargo-scope walks the transitive
    closure and emits illegal flags for six crates (harness-defect.jsonl,
    2026-09-01, not fixed there); the dropped flags are REPORTED, so a verdict
    on such a crate names the under-coverage instead of hiding it."""
    r = subprocess.run(["bash", "-c", 'REPO_ROOT="$1"; source "$2"; resolve_features "$3"', "_",
                        str(wt), str(ROOT / "scripts/lib/cargo-scope.sh"), pkg],
                       capture_output=True, text=True)
    want = [f for f in r.stdout.strip().split(",") if f]
    meta = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                          cwd=wt, capture_output=True, text=True)
    if meta.returncode != 0:
        return [], want, "cargo metadata failed at this commit"
    packages = {p["name"]: p for p in json.loads(meta.stdout)["packages"]}
    if pkg not in packages:
        return [], want, f"{pkg} is not a workspace member at this commit"
    direct = {d["name"] for d in packages[pkg]["dependencies"]}
    legal, dropped = [], []
    for f in want:
        dep, _, feat = f.partition("/")
        if dep == pkg:
            if feat in packages[pkg].get("features", {}):
                legal.append(feat)
            else:
                dropped.append(f)
        elif dep in direct:
            legal.append(f)
        else:
            dropped.append(f)
    return legal, dropped, None


def junit_results(text: str) -> dict:
    """{`<binary id>::<test name>`: passed}. Skipped cases are absent.
    Mirrors scripts/sabotage.py::run_suite's reader; the two should fold
    into one lib once that file is not mid-edit by another session."""
    out = {}
    for chunk in text.split("<testcase ")[1:]:
        head, _, body = chunk.partition(">")
        name = re.search(r'name="([^"]*)"', head)
        cls = re.search(r'classname="([^"]*)"', head)
        if not (name and cls):
            continue
        case = body.split("</testcase>")[0]
        if "<skipped" in case:
            continue
        out[f"{cls.group(1)}::{name.group(1)}"] = not ("<failure" in case or "<error" in case)
    return out


MIN_FREE_GB = 8


def test_filter(c: dict) -> str:
    f = f"test(/(^|::){re.escape(c['name'])}$/)"
    return f"({f} & binary_id(={c['binary']}))" if c.get("binary") else f


def run_rust(wt: Path, pkg: str, cites: list, jobs_override) -> dict:
    """One nextest run of the cited tests in `pkg`. Build failure is a
    distinct outcome, never an empty pass."""
    features, dropped, err = resolve_features(wt, pkg)
    free_gb = shutil.disk_usage(wt).free / 2**30
    if free_gb < MIN_FREE_GB:
        # A build that fills the disk takes every other job on the box down
        # with it. Refuse loudly; the verdict says why.
        err = f"refusing to build with {free_gb:.1f} GB free (floor {MIN_FREE_GB} GB)"
    if err:
        return {"built": False, "results": {}, "stderr": err, "argv": None,
                "features": features, "dropped_features": dropped, "seconds": 0}
    jobs, why = jobs_budget(jobs_override)
    filt = " + ".join(sorted({test_filter(c) for c in cites}))
    # --no-tests=warn: a filter that matches nothing is NEVER-RAN (the report
    # lacks the test), not a build failure; nextest's default exit 4 for that
    # case would be misread as one.
    argv = ["cargo", "nextest", "run", "-p", pkg, "--config-file", str(NEXTEST_CONFIG),
            "--profile", PROFILE, "--no-fail-fast", "--no-tests=warn", "-E", filt]
    if features:
        argv += ["--features", ",".join(features)]
    if jobs:
        argv += ["--build-jobs", str(jobs), "-j", str(jobs)]
    junit = wt / "target" / "nextest" / PROFILE / "junit.xml"
    before = junit.stat().st_mtime if junit.is_file() else 0.0
    t0 = time.time()
    try:
        r = subprocess.run(argv, cwd=wt, capture_output=True, text=True,
                           stdin=subprocess.DEVNULL, timeout=CARGO_TIMEOUT,
                           env={**os.environ, "CARGO_TERM_COLOR": "never"})
        stderr = r.stderr
        rc = r.returncode
    except subprocess.TimeoutExpired as e:
        stderr = (e.stderr or "") + f"\n[timed out after {CARGO_TIMEOUT}s]"
        rc = -1
    secs = round(time.time() - t0, 1)
    # A report that did not move is not this run's report (sabotage.py's rule).
    fresh = junit.is_file() and junit.stat().st_mtime > before
    built = fresh or ("error[E" not in stderr and "error: could not compile" not in stderr
                      and "error: no tests to run" not in stderr and rc in (0, 100))
    results = junit_results(junit.read_text()) if fresh else {}
    return {"built": built and (fresh or rc == 0), "results": results, "stderr": stderr[-6000:],
            "argv": shlex.join(argv), "features": features, "dropped_features": dropped,
            "seconds": secs, "jobs": f"{jobs} ({why})", "exit": rc}


def run_py(wt: Path, script: str) -> dict:
    t0 = time.time()
    r = subprocess.run([sys.executable, script, "--self-test"], cwd=wt, capture_output=True,
                       text=True, stdin=subprocess.DEVNULL, timeout=600)
    out = r.stdout + r.stderr
    crashed = "Traceback (most recent call last)" in out
    key = f"{script}::--self-test"
    return {"built": not crashed, "results": {} if crashed else {key: r.returncode == 0},
            "stderr": out[-6000:], "argv": f"{script} --self-test", "features": [],
            "dropped_features": [], "seconds": round(time.time() - t0, 1), "exit": r.returncode}


# ── the API retreat ────────────────────────────────────────────────────────
RUST_ERR_LINE = re.compile(r"^error(?:\[E\d+\])?:([^\n]*)", re.M)
BACKTICKED = re.compile(r"`([^`]+)`")
WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
RUST_NOISE = {"mut", "dyn", "impl", "ref", "self", "Self", "crate", "super", "const", "static",
              "fn", "for", "as", "in", "where", "async", "unsafe", "str", "u8", "u16", "u32",
              "u64", "usize", "i8", "i16", "i32", "i64", "isize", "bool", "char", "f32", "f64"}
PY_ERR_IDENT = re.compile(r"(?:NameError: name|AttributeError: .* has no attribute|"
                          r"ImportError: cannot import name|got an unexpected keyword argument) '(\w+)'")


def error_identifiers(stderr: str, kind: str) -> list:
    """Every identifier the compiler (or interpreter) names on an error line,
    in order, deduplicated. Warnings are ignored. `&ClaimRecord`, `Vec<Foo>`
    and `a::b::C` all yield their identifiers."""
    names = []
    if kind == "rust":
        # Diagnostics only. cargo's summary lines ("could not compile
        # `corpus-engine` (lib test)", "aborting due to 3 previous errors")
        # name the crate, not a symbol, and would feed the retreat noise.
        found = (w for line in RUST_ERR_LINE.finditer(stderr)
                 if not re.match(r"\s*(?:could not compile|aborting due to|failed to (?:compile|build|run))", line.group(1))
                 for span in BACKTICKED.findall(line.group(1))
                 for w in WORD.findall(span) if w not in RUST_NOISE)
    else:
        found = PY_ERR_IDENT.findall(stderr)
    for w in found:
        if w not in names:
            names.append(w)
    return names


def _defines(added: str, ident: str, path: str) -> bool:
    i = re.escape(ident)
    if path.endswith(".py"):
        return bool(re.search(rf"\b(?:def|class)\s+{i}\b|^\s*{i}\s*=", added, re.M))
    return bool(re.search(rf"\b(?:fn|struct|enum|trait|type|const|static|mod|union|macro_rules!)\s+{i}\b", added)
                or re.search(rf"^\s*(?:pub(?:\([^)]*\))?\s+)?{i}\s*:", added, re.M)   # a field
                or re.search(rf"^\s*{i}\s*(?:\([^)]*\))?\s*,\s*$", added, re.M))       # a unit/tuple variant


def hunks_introducing(files: list, idents: list, restored: set) -> list:
    """The reverted SOURCE hunks to restore so the named identifiers exist.

    Hunks that DEFINE an identifier come first and alone: for E0560 "struct
    `GcReport` has no field named `tombstones_written`" the hunk to restore is
    the one adding the field, not the behaviour hunk that constructs a
    GcReport. Only when no reverted hunk defines any of the names does a
    hunk that merely mentions one qualify — a `use` line, a re-export."""
    live = [(f, h) for f in files for h in f["hunks"] if h["role"] == SOURCE and h["id"] not in restored
            and not f["path"].endswith((".md", ".txt"))]   # a doc hunk cannot fix a compile error
    defining = [h["id"] for f, h in live if any(_defines(h["added"], i, f["path"]) for i in idents)]
    if defining:
        return defining
    return [h["id"] for f, h in live
            if any(re.search(rf"\b{re.escape(i)}\b", h["added"]) for i in idents)]


# ── adjudication ───────────────────────────────────────────────────────────
def matched(results: dict, cites: list) -> tuple:
    """({cited name: passed|None}, {cited name: [keys]} for names that matched
    MORE than one report key). None when the report has no such test.
    Ambiguity is never resolved by picking: every match is judged, and the
    record names them, so a verdict that rests on an ambiguous name says so."""
    out, ambiguous = {}, {}
    for c in cites:
        n = c["name"]
        keys = [k for k in results if k == n or k.endswith("::" + n)]
        if c.get("binary"):
            keys = [k for k in keys if k.startswith(c["binary"] + "::")]
        out[n] = None if not keys else all(results[k] for k in keys)
        if len(keys) > 1:
            ambiguous[n] = keys
    return out, ambiguous


def adjudicate(commit: str, wt: Path, jobs_override=None, cites=None) -> dict:
    t0 = time.time()
    full = git("rev-parse", commit).strip()
    subject, _, body = git("show", "-s", "--format=%s%n%b", full).partition("\n")
    rec = {"commit": full[:9], "subject": subject[:100], "citations": cites or cited_tests(full, body),
           "verdict": None, "detail": None, "control": [], "mutant": [], "restored_for_api": [],
           "reverted_hunks": 0, "source_hunks": 0, "test_hunks": 0}
    if not rec["citations"]:
        rec.update(verdict=NEVER_RAN, detail="the body names no test that exists at this commit")
        return rec
    explicit = [c for c in rec["citations"] if not c.get("inferred") and c.get("role") != "mention"]
    inferred = [c for c in rec["citations"] if c.get("inferred")]
    judged = explicit or inferred
    if not judged:
        rec.update(verdict=NEVER_RAN, detail="every named test is a mention (flake / unrelated / "
                   "untouched), not a citation of evidence")
        return rec
    rec["judged"] = "explicit citations" if explicit else "inferred: tests this commit added/modified"
    files = classify(full)
    rec["source_hunks"] = sum(h["role"] == SOURCE for f in files for h in f["hunks"])
    rec["test_hunks"] = sum(h["role"] == TEST for f in files for h in f["hunks"])
    rec["files"] = [{"path": f["path"], "status": f["status"],
                     "roles": sorted({h["role"] for h in f["hunks"]})} for f in files]
    if rec["source_hunks"] == 0:
        rec.update(verdict=NEVER_RAN, detail="the commit changes no source hunk — nothing to revert")
        return rec

    ensure_worktree(wt, full)
    groups = {}
    for c in judged:
        groups.setdefault((c["kind"], c["crate"] or c["file"]), []).append(c)

    def run_all(label):
        runs, ok, missing, failed = [], True, [], []
        for (kind, unit), cs in groups.items():
            r = run_rust(wt, unit, cs, jobs_override) if kind == "rust" else run_py(wt, unit)
            r["unit"] = unit
            got, amb = matched(r["results"], cs if kind == "rust" else [{"name": f"{unit}::--self-test"}])
            r["cited"] = got
            if amb:
                r["ambiguous"] = amb
                rec.setdefault("ambiguous", {}).update(amb)
            runs.append(r)
            if not r["built"]:
                ok = False
            missing += [n for n, v in got.items() if v is None]
            failed += [n for n, v in got.items() if v is False]
            log(f"{full[:9]} {label} {unit}: built={r['built']} cited={got} {r['seconds']}s")
        return runs, ok, missing, failed

    # 2. POSITIVE CONTROL
    runs, built, missing, failed = run_all("control")
    rec["control"] = runs
    if not built:
        rec.update(verdict=CNJ, detail="control did not build at the commit itself")
        return rec
    if missing:
        rec.update(verdict=NEVER_RAN, detail=f"cited test(s) not in the report at control: {missing}")
        return rec
    if failed:
        rec.update(verdict=CNJ, detail=f"control FAILED — the cited test is red at its own commit: {failed}")
        return rec

    # 3. MUTANT, with the API retreat
    restored, attempts = set(), []
    try:
        for attempt in range(MAX_RETREAT + 1):
            reverted = apply_mutant(wt, full, files, restored)
            if not reverted:
                rec.update(verdict=CNJ, detail="every source hunk had to be restored for the kept "
                           "tests to compile — no behavioural hunk is separable from the API")
                break
            runs, built, missing, failed = run_all(f"mutant#{attempt}")
            attempts.append({"attempt": attempt, "reverted": len(reverted), "built": built,
                             "runs": runs})
            if built:
                rec["reverted_hunks"] = len(reverted)
                # Per citation, because a commit citing two tests can have one
                # empty citation and one real one; the commit-level verdict
                # would report VALIDATED and hide the empty one.
                rec["per_citation"] = {n: (NEVER_RAN if v is None else VALIDATED if v is False else UNSUPPORTED)
                                       for r in runs for n, v in r["cited"].items()}
                rev_files = sorted({h.rsplit("@", 1)[0] for h in reverted})
                api_files = sorted({h.rsplit("@", 1)[0] for h in restored})
                scope = (f" (after restoring {len(restored)} API hunk(s) in {api_files}; "
                         f"still reverted: {rev_files})" if restored else "")
                if missing:
                    rec.update(verdict=CNJ, detail=f"cited test(s) vanished from the report under "
                               f"the mutant: {missing}")
                elif failed:
                    rec.update(verdict=VALIDATED, detail=f"mutant fails {failed}{scope}")
                elif restored:
                    # Precise about what was tested: when API and behaviour
                    # share a hunk (a new fn whose body IS the fix), the
                    # retreat restores both, and what the test is then shown
                    # not to see is the REST of the change — a call site, a
                    # wiring — not the whole of it. fee0fcd1b, 2026-09-01.
                    rec.update(verdict=UNSUPPORTED, detail=f"the mutant still passes every cited test"
                               f"{scope} — the test sees the restored API, not the hunks in {rev_files}")
                else:
                    rec.update(verdict=UNSUPPORTED, detail="mutant passes every cited test — the "
                               "test does not depend on the reverted source")
                break
            idents = []
            for r in runs:
                idents += error_identifiers(r["stderr"], "py" if r["argv"].endswith("--self-test") else "rust")
            more = hunks_introducing(files, idents, restored)
            attempts[-1]["error_identifiers"] = idents[:20]
            attempts[-1]["restoring"] = more
            if not more:
                tail = "\n".join(l for l in runs[-1]["stderr"].splitlines() if l.startswith("error"))[-800:]
                rec.update(verdict=CNJ, detail="mutant does not build and the errors name no "
                           f"identifier a reverted hunk introduces: {tail or runs[-1]['stderr'][-400:]}")
                break
            restored.update(more)
        else:
            rec.update(verdict=CNJ, detail=f"no compiling mutant after {MAX_RETREAT} API retreats")
    except RuntimeError as e:
        rec.update(verdict=CNJ, detail=str(e))
    finally:
        git("checkout", "-q", "-f", full, "--", ".", cwd=wt)
        git("clean", "-fdq", cwd=wt)
    rec["mutant"] = attempts
    rec["restored_for_api"] = sorted(restored)
    rec["seconds"] = round(time.time() - t0, 1)
    return rec


# ── self-test ──────────────────────────────────────────────────────────────
def self_test() -> int:
    checks = []

    def check(name, cond):
        checks.append((name, bool(cond)))

    rs = ('use x;\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn a() { let s = "}"; }\n}\n'
          'fn prod() {}\n#[tokio::test]\nasync fn b() {}\n')
    regions = test_regions_rs(rs)
    check("cfg(test) mod is a region", (2, 7) in regions)
    check("a brace inside a string does not end the region", all(b >= 7 for a, b in regions if a == 2))
    check("bare #[tokio::test] fn outside the mod is a region", any(a == 9 for a, b in regions))
    check("is_test_fn sees the attribute", is_test_fn(rs, "b"))
    check("is_test_fn refuses a plain fn right after a test", not is_test_fn(rs, "prod"))
    check("a helper in a tests/ file is not a test", not is_test_fn("fn model_id_for() {}", "model_id_for"))
    check("a multi-line attribute still counts",
          is_test_fn("#[rstest(\n    case(1),\n)]\nfn m(x: u8) {}", "m"))
    check("test_fns_in lists attributed fns only", test_fns_in(rs) == ["a", "b"])

    diff = ("diff --git a/src/lib.rs b/src/lib.rs\nindex 1..2 100644\n--- a/src/lib.rs\n+++ b/src/lib.rs\n"
            "@@ -1 +1 @@\n-old\n+new\n@@ -5,0 +6,2 @@\n+    #[test]\n+    fn t() {}\n@@ -9 +11,0 @@\n-gone\n")
    header, hunks = parse_hunks(diff)
    check("three -U0 hunks parsed", len(hunks) == 3 and header.startswith("diff --git"))
    check("a pure deletion hunk has new_len 0", hunks[2]["new_len"] == 0 and hunks[2]["new_start"] == 11)
    check("added text is the + lines", hunks[1]["added"] == "    #[test]\n    fn t() {}")
    check("inside() uses the new-side range", inside(hunks[1], [(6, 7)]) and not inside(hunks[0], [(6, 7)]))

    check("file_role: tests/ is TEST", file_role("crate/tests/x.rs") == TEST)
    check("file_role: Cargo.lock is KEEP", file_role("Cargo.lock") == KEEP)
    check("file_role: src is undecided", file_role("crate/src/lib.rs") is None)
    check("file_role: a golden is TEST", file_role("crate/src/goldens/report.md") == TEST)

    py = "FIXTURE = 1\ndef work():\n    pass\ndef _self_test():\n    pass\n"
    check("py regions: FIXTURE and _self_test, not work", test_regions_py(py) == [(1, 1), (4, 5)])

    err = ("error[E0609]: no field `node_id` on type `&ClaimRecord`\n --> x.rs:1:1\n"
           "error[E0599]: no method named `open_index_transient` found for struct `Engine`\n"
           "error[E0425]: cannot find function `offload_verdict_opt` in this scope\n"
           "error[E0433]: failed to resolve: use of undeclared type `mesh::Foo`\n"
           "warning: unused variable: `x`\n"
           "error: could not compile `corpus-engine` (lib test) due to 4 previous errors\n"
           "error: aborting due to 4 previous errors; 1 warning emitted\n")
    ids = error_identifiers(err, "rust")
    check("rust error identifiers, in order, warnings ignored",
          ids == ["node_id", "ClaimRecord", "open_index_transient", "Engine", "offload_verdict_opt", "mesh", "Foo"])
    check("python NameError identifier",
          error_identifiers("NameError: name 'substrate_epoch' is not defined", "py") == ["substrate_epoch"])
    files = [{"path": "a.rs", "hunks": [
        {"id": "a.rs@1", "role": SOURCE, "added": "pub node_id: String,"},
        {"id": "a.rs@9", "role": SOURCE, "added": "GcReport { node_id: n, tombstones_written: k }"},
        {"id": "a.rs@40", "role": TEST, "added": "assert!(r.node_id.is_empty())"}]}]
    check("retreat restores the hunk that DEFINES the identifier, not the one constructing it",
          hunks_introducing(files, ["GcReport", "node_id"], set()) == ["a.rs@1"])
    check("retreat falls back to a mention when nothing defines the name",
          hunks_introducing(files, ["tombstones_written"], set()) == ["a.rs@9"])
    check("retreat skips already-restored hunks", hunks_introducing(files, ["node_id"], {"a.rs@1"}) == ["a.rs@9"])
    docs = files + [{"path": "docs/A.md", "hunks": [{"id": "docs/A.md@3", "role": SOURCE, "added": "ClaimTombstone rows"}]}]
    check("retreat never restores a doc hunk", "docs/A.md@3" not in hunks_introducing(docs, ["ClaimTombstone"], set()))
    check("test_fn_regions names and spans the fn",
          [(n, a) for n, a, b in test_fn_regions(rs)] == [("a", 5), ("b", 9)])

    check("tokens(): snake_case and CamelCase only, >=6 chars, std noise dropped",
          tokens("let x = foo_bar(); ClaimRecord; to_string(); ab_c; CARGO_MANIFEST_DIR; Vec<u8>")
          == {"foo_bar", "ClaimRecord"})
    files = [{"path": "c/src/lib.rs", "hunks": [{"role": SOURCE, "added": "fn compute_score() {}", "removed": "fn old_score() {}"},
                                                 {"role": TEST, "added": "fn test_only_token() {}", "removed": ""}]},
             {"path": "docs/X.md", "hunks": [{"role": SOURCE, "added": "compute_score doc_only_word", "removed": ""}]},
             {"path": "sovereign-recipes/sec-filings-company/recipe.toml", "hunks": [{"role": SOURCE, "added": "x = 1", "removed": ""}]},
             {"path": "scripts/setup-sec-corpus.sh", "hunks": [{"role": SOURCE, "added": "y", "removed": ""}]}]
    touched, stems = source_surface(files)
    check("source_surface: SOURCE hunks only, removed lines count, docs excluded, distinctive stems only",
          touched == {"compute_score", "old_score"} and stems == {"setup-sec-corpus"})
    check("overlap_class: suspect when the file names nothing touched",
          overlap_class(touched, stems, "fn t() { load_all_recipes(); }", "load_all_recipes();")[0] == "suspect")
    check("overlap_class: weak when only the file (not the fn) names it",
          overlap_class(touched, stems, "fn h() { compute_score() }\nfn t() { h() }", "h()")[0] == "weak")
    body1, body2 = "assert!(compute_score() > 0)", "run(\"setup-sec-corpus\")"
    check("overlap_class: reaches when the fn body names it; stems count",
          overlap_class(touched, stems, body1, body1)[0] == "reaches"
          and overlap_class(touched, stems, body2, body2)[0] == "reaches")
    check("citation_role: a flake disclaimer is a mention even with 'passes' in it",
          citation_role("Gates green. tests 9076 pass, 1 unrelated environmental flake "
                        "(`read_cpu_ram_state_returns_values` asserts nonzero; passes alone).",
                        "read_cpu_ram_state_returns_values")[0] == "mention")
    check("citation_role: 'Verified: x passes' is evidence",
          citation_role("Body.\n\nVerified: every_bundled_recipe_loads passes, so it parses.",
                        "every_bundled_recipe_loads")[0] == "evidence")
    check("citation_role: 'the sibling x is untouched' is a mention",
          citation_role("The sibling gap_derived_queries is untouched.", "gap_derived_queries")[0] == "mention")
    check("citation_role: a pass/fail tally that lists the reds is a mention",
          citation_role("Full workspace suite 9454 pass / 3 fail — the three are `sigkill_child_midstream_recovers`.",
                        "sigkill_child_midstream_recovers")[0] == "mention")
    check("citation_role: a bare name with no signal is unclear",
          citation_role("See also foo_bar_baz for the shape.", "foo_bar_baz")[0] == "unclear")

    junit = ('<testsuite><testcase name="tests::a" classname="c::lib"></testcase>'
             '<testcase name="b" classname="c::it"><failure/></testcase>'
             '<testcase name="s" classname="c::it"><skipped/></testcase></testsuite>')
    res = junit_results(junit)
    check("junit: pass/fail read, skipped absent", res == {"c::lib::tests::a": True, "c::it::b": False})
    got, amb = matched(res, [{"name": "a"}, {"name": "b"}, {"name": "zz"}])
    check("matched() joins by suffix and reports absence as None",
          got == {"a": True, "b": False, "zz": None} and amb == {})
    res2 = {"c::lib::x::a": True, "c::lib::y::a": False, "c::it::a": True}
    got, amb = matched(res2, [{"name": "a"}])
    check("an ambiguous name judges every match and is NAMED, never picked",
          got == {"a": False} and amb == {"a": ["c::lib::x::a", "c::lib::y::a", "c::it::a"]})
    got, amb = matched(res2, [{"name": "a", "binary": "c::it"}])
    check("a binary id pins an integration test to its own binary", got == {"a": True} and amb == {})
    check("citation() derives the binary id for crate/tests/<stem>.rs only",
          citation("t", "x/tests/foo.rs", "x").get("binary") == "x::foo"
          and "binary" not in citation("t", "x/tests/common/mod.rs", "x")
          and "binary" not in citation("t", "x/src/lib.rs", "x"))
    check("test_filter pins by binary_id when known",
          test_filter({"name": "t", "binary": "x::foo"}) == "(test(/(^|::)t$/) & binary_id(=x::foo))")

    bad = [n for n, ok in checks if not ok]
    for n, ok in checks:
        print(f"  {'ok ' if ok else 'BAD'} {n}")
    print(f"{len(checks)} checks, {len(bad)} failed")
    return 1 if bad else 0


# ── main ───────────────────────────────────────────────────────────────────
def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("commits", nargs="*")
    ap.add_argument("--range", help="git rev range, e.g. HEAD~300..HEAD")
    ap.add_argument("--candidates", action="store_true", help="list commits that cite a test; no builds")
    ap.add_argument("--static", action="store_true",
                    help="static overlap class per explicit citation (suspect/weak/reaches); no builds")
    ap.add_argument("--from", dest="from_jsonl", help="take commits + citations from a --candidates jsonl")
    ap.add_argument("--worktree", default=str(ROOT.parent / f"wt-{ROOT.name}-evidence"))
    ap.add_argument("--jobs", help="cargo/nextest job count; default from scripts/lib/cargo-jobs.sh")
    ap.add_argument("--jsonl", help="append one record per commit as it completes; resumable")
    ap.add_argument("--max", type=int, default=0)
    ap.add_argument("--self-test", action="store_true")
    a = ap.parse_args()
    if a.self_test:
        return self_test()

    commits = list(a.commits)
    given = {}
    if a.from_jsonl:
        for line in Path(a.from_jsonl).read_text().splitlines():
            row = json.loads(line)
            given[row["commit"]] = row
            commits.append(row["commit"])
    if a.range:
        commits += git("rev-list", "--no-merges", "--reverse", a.range).split()
    if not commits:
        print("evidence-verdict: no commits selected — a zero-work run is not a pass", file=sys.stderr)
        return 4

    if a.static:
        counts = {}
        for c in commits:
            full = git("rev-parse", c).strip()
            row = given.get(full[:9])
            subject, _, body = git("show", "-s", "--format=%s%n%b", full).partition("\n")
            if row is None:
                row = {"commit": full[:9], "subject": subject[:90], "citations": cited_tests(full, body)}
            for c in row["citations"]:   # a --candidates file written before roles existed
                if "role" not in c and not c.get("inferred"):
                    c["role"], c["context"] = citation_role(body, c.get("via_file") or c["name"])
            s = static_overlap(full, row["citations"])
            counts[s["static"]] = counts.get(s["static"], 0) + 1
            print(json.dumps({"commit": row["commit"], "subject": row["subject"], **s}), flush=True)
        log("static: " + "  ".join(f"{k} {v}" for k, v in sorted(counts.items())))
        return 0

    if a.candidates:
        n = 0
        for c in commits:
            full = git("rev-parse", c).strip()
            subject, _, body = git("show", "-s", "--format=%s%n%b", full).partition("\n")
            cites = cited_tests(full, body)
            if cites:
                n += 1
                print(json.dumps({"commit": full[:9], "subject": subject[:90], "citations": cites}))
                if a.max and n >= a.max:
                    break
        log(f"{n} of {len(commits)} commits cite a test that exists at the commit")
        return 0

    done = set()
    if a.jsonl and Path(a.jsonl).is_file():
        for line in Path(a.jsonl).read_text().splitlines():
            try:
                done.add(json.loads(line)["commit"])
            except (ValueError, KeyError):
                pass
    wt = Path(a.worktree).resolve()
    counts, n = {}, 0
    for c in commits:
        short = git("rev-parse", c).strip()[:9]
        if short in done:
            log(f"{short} already in {a.jsonl} — skipped")
            continue
        rec = adjudicate(short, wt, a.jobs)
        counts[rec["verdict"]] = counts.get(rec["verdict"], 0) + 1
        print(f"  {rec['verdict']:16s} {rec['commit']}  {rec['subject'][:60]}\n"
              f"                   {rec['detail']}", flush=True)
        if a.jsonl:
            with open(a.jsonl, "a") as fh:
                fh.write(json.dumps(rec, default=str) + "\n")
        n += 1
        if a.max and n >= a.max:
            break
    print("\n" + "  ".join(f"{k} {v}" for k, v in sorted(counts.items())))
    return 0


if __name__ == "__main__":
    sys.exit(main())
