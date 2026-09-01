#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""
Propose (requirement, test, mutation) triples for scripts/sabotage.py to judge.

RUNS ON THE LOCAL DAEMON, NOT ON AGENT TOKENS. Batch classify/score work is
sovereign-native by standing operator direction (2026-08-09).

WHY A CHEAP MODEL IS ENOUGH. sabotage.py is the ground truth: a wrong candidate
SURVIVES its mutation and is discarded automatically. So this stage needs
RECALL, not precision — it may propose five candidates per requirement and be
wrong about four. That is the whole reason the matching can be automated at all;
hand-adjudication measured 22% accurate and had no such backstop (note cf566968).

Three stages, only the last of which needs a chat model:

  1. INVENTORY   every test in the workspace, name -> file:line, from the junit
                 report plus ripgrep. Deterministic.
  2. RETRIEVE    embed each requirement clause and each test name, take top-K by
                 cosine. This repo's test names are English sentences
                 (`a_quote_from_beyond_the_prompt_truncation_is_no_longer_demoted`),
                 which makes name-vs-clause a strong signal for free.
  3. PROPOSE     for each pair, the chat model reads the clause and the test's
                 source and proposes a find/replace in the code under test that
                 should make that test fail.

  scripts/conformance-candidates.py --out quality/sabotage/generated.toml
  scripts/conformance-candidates.py --family GR --top-k 5
"""
import argparse, json, re, subprocess, sys, tomllib, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DAEMON = "http://127.0.0.1:9741"


def post(path, body, timeout=180):
    req = urllib.request.Request(
        DAEMON + path, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())


def requirements(family=None):
    reg = tomllib.load(open(ROOT / "quality/requirements.toml", "rb"))
    enf = tomllib.load(open(ROOT / "quality/requirements-enforceability.toml", "rb"))
    out = []
    for r in reg["requirements"]:
        if r.get("alias_of") or r["level"] == "out-of-scope":
            continue
        if enf.get(r["id"]) not in ("cli", "desktop", "structural"):
            continue          # model/review: no automated instrument
        if family and r["family"] != family:
            continue
        out.append(r)
    return out


# A test fn, by its `#[test]`/`#[tokio::test]` attribute rather than by a name
# heuristic. `ripgrep` is NOT assumed present — it is absent on this host, and a
# generator that dies on a missing tool at minute 40 of an overnight run is
# worse than one that walks the tree itself.
TEST_ATTR = re.compile(r"#\[(?:tokio::)?test\b")
FN_DECL = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([a-z_][a-z_0-9]*)\s*\(")


def inventory():
    """Every #[test] fn in the workspace: name -> (file, line). Deterministic."""
    tests = {}
    for path in ROOT.rglob("*.rs"):
        rel = path.relative_to(ROOT).as_posix()
        if rel.startswith(("target", ".cargo", "research/verifier-v0")):
            continue
        try:
            lines = path.read_text(errors="ignore").splitlines()
        except OSError:
            continue
        armed = False
        for i, line in enumerate(lines, 1):
            if TEST_ATTR.search(line):
                armed = True
                continue
            m = FN_DECL.match(line)
            if m and armed:
                tests.setdefault(m.group(1), (rel, i))
                armed = False
            elif line.strip() and not line.lstrip().startswith("#["):
                armed = armed and not m
    return tests


def embed(texts, batch=64):
    vecs = []
    for i in range(0, len(texts), batch):
        r = post("/v1/embeddings", {"model": "embed", "input": texts[i:i + batch]})
        vecs.extend(d["embedding"] for d in r["data"])
        print(f"  embedded {min(i+batch, len(texts))}/{len(texts)}", file=sys.stderr, flush=True)
    return vecs


# The 11,776 test-name vectors are ~12 minutes of the 13-minute probe and do not
# change between runs of the same tree. Cached as raw float32 next to a manifest
# of the exact names they were built from: if the inventory shifts by one test,
# the cache is discarded rather than misaligned. A silently stale vector table
# would rank the wrong tests and be invisible in the output.
def embed_cached(texts, cache_path):
    import array, hashlib
    key = hashlib.sha256("\n".join(texts).encode()).hexdigest()
    cache = ROOT / cache_path
    meta = cache.with_suffix(".json")
    if cache.is_file() and meta.is_file():
        m = json.loads(meta.read_text())
        if m.get("key") == key:
            buf = array.array("f")
            buf.frombytes(cache.read_bytes())
            dim = m["dim"]
            print(f"  cache hit: {len(texts)} vectors x {dim}", file=sys.stderr)
            return [list(buf[i * dim:(i + 1) * dim]) for i in range(len(texts))]
        print("  cache stale (inventory changed) — re-embedding", file=sys.stderr)
    vecs = embed(texts)
    dim = len(vecs[0])
    buf = array.array("f", [x for v in vecs for x in v])
    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.write_bytes(buf.tobytes())
    meta.write_text(json.dumps({"key": key, "dim": dim, "n": len(vecs)}))
    return vecs


def cosine_top(qv, mv, names, k):
    import math
    qn = math.sqrt(sum(x * x for x in qv)) or 1.0
    scored = []
    for v, n in zip(mv, names):
        d = sum(a * b for a, b in zip(qv, v))
        vn = math.sqrt(sum(x * x for x in v)) or 1.0
        scored.append((d / (qn * vn), n))
    scored.sort(reverse=True)
    return scored[:k]


# ── The code under test ────────────────────────────────────────────────────
#
# WHY THIS EXISTS. v1 of this generator showed the model ONLY the test source
# and then asked it to mutate "the code this test exercises" -- code it had
# never been shown. Measured on the full GR family (250 pairs, 2026-09-01): the
# two dominant rejections were `test-code` (it mutated the test body, the only
# thing in front of it) and `find-absent` (it invented a snippet for a file it
# could not see). Those are an INPUT defect, not evidence about coverage, and
# reading them as "no coverage exists" would have sent the campaign to write
# tests it already has.
#
# The fix is entirely inventory (ARCH principle 11): `callees` is SCIP-resolved
# and already answers "what production function does this test call", and
# `symbols` returns that function's source with its path and line range. Both
# are registry-only since 2026-08-31, so the CLI form is the reachable one.
# The `// <path>:<start>-<end>  [kind]  (corpus)` header `symbols` prints above
# the source it returns. The KIND is load-bearing: a bare name like `answer` is
# both a local variable in a test and a module in kernel-types, and resolving
# the local to the module returned a 745-line file as the "function under test"
# (measured 2026-09-01, before this filter existed).
SYMBOL_HEADER = re.compile(r"^// (\S+?):(\d+)-(\d+)\s+\[(\w+)\]", re.M)

# Call sites in the test body. Deliberately a local regex and NOT the `callees`
# tool: `callees` left the MCP surface on 2026-08-31 and `tools/call` gates on
# `is_mcp_exposed`, so a generator built on it works against today's daemon and
# starts refusing the moment the daemon is rebuilt from this tree -- returning
# nothing, which this pipeline would record as "no code under test" and a reader
# would mistake for "no coverage exists". That is the exact misreading this
# whole instrument was rebuilt to prevent, so the dependency is not taken.
#
# Precision is not required here: sabotage.py is ground truth, and a wrongly
# chosen target SURVIVES its mutation and is discarded. Recall is what matters.
CALL_SITE = re.compile(r"\b([a-z_][a-z0-9_]{3,})\s*\(")
# Each unresolvable name still costs a round trip, so the walk is bounded. The
# function a test is about is called early in its body, not on line 40.
MAX_LOOKUPS = 6
NOT_A_CALL = {
    "assert", "assert_eq", "assert_ne", "panic", "println", "format", "write",
    "vec", "some", "none", "ok", "err", "self", "clone", "into", "from",
    "to_string", "unwrap", "expect", "push", "insert", "collect", "iter",
    "len", "is_empty", "contains", "matches", "await", "async", "return",
    "unwrap_or", "unwrap_or_default", "map", "filter", "join", "new",
}
_sym_cache = {}


def mcp_call(tool_id, args):
    """One `tools/call` over the daemon's MCP endpoint (~2s, vs ~37s for the
    CLI dispatcher, measured 2026-09-01).

    Raises on a refusal. A tool that has left the surface must stop this run
    loudly -- degrading to None would recreate the silent-absence failure.
    """
    r = post("/mcp", {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                      "params": {"name": tool_id, "arguments": args}}, timeout=90)
    if "error" in r:
        raise RuntimeError(f"{tool_id}: {r['error'].get('message', r['error'])}")
    parts = r.get("result", {}).get("content", [])
    return "".join(p.get("text", "") for p in parts)


def symbol_source(name):
    """(path, start_line, source) for a first-party FUNCTION, else None."""
    if name not in _sym_cache:
        try:
            _sym_cache[name] = mcp_call("symbols", {"name": name})
        except (RuntimeError, OSError) as e:
            if "tool not found" in str(e):
                raise                       # the surface moved; stop, do not guess
            _sym_cache[name] = ""
    out = _sym_cache[name]
    m = SYMBOL_HEADER.search(out or "")
    if not m:
        return None
    body = out.split("```rust", 1)[-1].rsplit("```", 1)[0].strip()
    # THE KIND FIELD IS NOT TRUSTWORTHY HERE. The same symbol came back
    # `[function]` and, minutes later while the index was re-enriching,
    # `[unknown]` (observed 2026-09-01) -- so gating on it rejected every
    # candidate and looked exactly like "nothing resolves". The source itself
    # is the reliable witness: a function definition contains `fn <name>(`,
    # and the module that a bare local like `answer` wrongly resolves to
    # does not.
    if not re.search(rf"\bfn\s+{re.escape(name)}\s*[(<]", body):
        return None
    return m.group(1), int(m.group(2)), body


# Where a file's test module starts, so a callee defined INSIDE it can be told
# from the production function above it. Both live in the same .rs file in this
# workspace -- `quote_verification.rs` holds `verify_answer_against_evidence`
# and the `mod tests` that exercises it -- so "is it a different file?" is the
# wrong question and rejected the correct target every time (measured
# 2026-09-01). Position in the file is the right one.
_cfg_test_line = {}


def test_mod_line(path):
    if path not in _cfg_test_line:
        try:
            text = (ROOT / path).read_text(errors="ignore")
        except OSError:
            _cfg_test_line[path] = None
            return None
        i = text.find("#[cfg(test)]")
        _cfg_test_line[path] = text[:i].count("\n") + 1 if i >= 0 else None
    return _cfg_test_line[path]


def code_under_test(test_src, test_file):
    """The production function the test calls, as (path, source).

    None means nothing first-party resolved -- informative in itself, and
    recorded as its own rejection rather than handed to the model to guess at.
    """
    seen = set()
    for name in CALL_SITE.findall(test_src):
        if name in seen or name in NOT_A_CALL or name.startswith("test_"):
            continue
        seen.add(name)
        if len(seen) > MAX_LOOKUPS:
            break
        got = symbol_source(name)
        if not got:
            continue
        path, line, src = got
        # The mutation must land in PRODUCTION code, which is a question about
        # position, not about which file. An integration test under tests/ has
        # no production code of its own; a unit test's target sits above the
        # `#[cfg(test)]` line in the same file.
        if "/tests/" in path or path.startswith("tests/"):
            continue
        cut = test_mod_line(path)
        if cut is not None and line >= cut:
            continue                        # defined inside the test module
        if len(src.splitlines()) > 200:
            continue                        # a whole module, not a function
        return path, src
    return None


PROMPT = """You are proposing a MUTATION that should make one test fail.

REQUIREMENT {rid}: {rtext}

The TEST, which asserts something. Do NOT edit this:
```rust
// {tfile}
{tsrc}
```

The PRODUCTION CODE it calls. Your edit goes HERE:
```rust
// {cpath}
{csrc}
```

Propose ONE edit to the PRODUCTION CODE above that would VIOLATE the
requirement and therefore make that test FAIL.

Rules:
- `target` must be exactly: {cpath}
- `find` must be copied VERBATIM from the production code shown above, and
  must appear exactly once in that file. Copy it; do not retype it.
- The edit must still COMPILE. Do not delete a signature or a brace.
- Do not edit the test. Breaking a test proves nothing about the product.
- If the test's assertion is not this requirement, answer exactly: NO

Answer as JSON only:
{{"target":"<path from repo root>","find":"<exact snippet>","replace":"<replacement>"}}"""


def write_bank(path, mutants, rejects):
    """The bank, rewritten in full. Cheap (a few hundred rows) and atomic
    enough: a partial bank is still a valid bank, and sabotage.py is the thing
    that decides whether any row in it is true."""
    total = sum(rejects.values())
    tally = " ".join(f"{k}={v}" for k, v in sorted(rejects.items()))
    esc = json.dumps
    body = ["# GENERATED by scripts/conformance-candidates.py — proposals, NOT claims.",
            "# scripts/sabotage.py adjudicates: a mutant that SURVIVES is discarded.",
            f"# {len(mutants)} candidate(s), {total} rejected before adjudication.",
            f"# rejections: {tally}", ""]
    for m in mutants:
        body.append("[[mutant]]")
        for k in ("id", "requirement", "target", "find", "replace"):
            body.append(f"{k} = {esc(m[k])}")
        body.append(f"mustFail = [{esc(m['mustFail'][0])}]")
        body.append(f"breaks = {esc(m['breaks'])}")
        body.append('expected = "CAUGHT"')
        body.append("")
    path.write_text("\n".join(body))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="quality/sabotage/generated.toml")
    ap.add_argument("--family")
    ap.add_argument("--top-k", type=int, default=5)
    ap.add_argument("--limit", type=int)
    ap.add_argument("--model", default="primary")
    ap.add_argument("--embed-cache", default="test-artifacts/test-name-vectors.f32")
    ap.add_argument("--trace", help="jsonl of every proposal + its verdict; the "
                                    "only way to tell absent coverage from a "
                                    "broken extractor after the fact")
    a = ap.parse_args()

    reqs = requirements(a.family)
    if a.limit:
        reqs = reqs[:a.limit]
    tests = inventory()
    names = sorted(tests)
    print(f"candidates: {len(reqs)} requirement(s) x {len(names)} test(s)", file=sys.stderr)

    print("embedding tests…", file=sys.stderr)
    tv = embed_cached([n.replace("_", " ") for n in names], a.embed_cache)
    print("embedding requirements…", file=sys.stderr)
    rv = embed([r["text"] for r in reqs])

    # WHY EVERY REJECTION IS NAMED AND COUNTED SEPARATELY. The first full-family
    # probe (2026-09-01, GR, 50 requirements x 5) returned 0 of 250 behind a
    # SINGLE `skipped` counter. "Zero candidates" from a collapsed counter cannot
    # distinguish the honest finding (`no-such-coverage`: the model read the test
    # and said this clause is not what it asserts) from an instrument defect
    # (`find-absent`: the snippet it quoted is not in the file). Those two
    # readings send the campaign in opposite directions -- write coverage, or fix
    # the extractor -- so the buckets are the measurement, not the total.
    # ARCH_PRINCIPLES 18.3: absence is REPORTED, never defaulted.
    from collections import Counter
    mutants, rejects = [], Counter()
    trace = open(ROOT / a.trace, "w") if a.trace else None

    def reject(reason, r, name, score, text=None, detail=None):
        rejects[reason] += 1
        if trace:
            trace.write(json.dumps({
                "requirement": r["id"], "test": name, "score": round(score, 3),
                "verdict": reason, "detail": detail,
                "response": (text or "")[:2000],
            }) + "\n")
            trace.flush()

    for r, qv in zip(reqs, rv):
        for score, name in cosine_top(qv, tv, names, a.top_k):
            path, line = tests[name]
            src = "\n".join((ROOT / path).read_text().splitlines()[max(0, line - 2):line + 40])
            under = code_under_test(src, path)
            if under is None:
                # No first-party callee resolved. Nothing to mutate, and asking
                # the model anyway is what produced v1's invented snippets.
                reject("no-code-under-test", r, name, score)
                continue
            cpath, csrc = under
            try:
                resp = post("/v1/chat/completions", {
                    "model": a.model, "temperature": 0.2, "max_tokens": 600,
                    "messages": [{"role": "user", "content": PROMPT.format(
                        rid=r["id"], rtext=r["text"], tname=name, tfile=path,
                        tsrc=src, cpath=cpath, csrc=csrc)}],
                })
                text = resp["choices"][0]["message"]["content"]
            except Exception as e:
                print(f"  ! {r['id']}/{name}: {e}", file=sys.stderr)
                reject("daemon-error", r, name, score, detail=str(e)[:200])
                continue
            # A reasoning model wraps its answer in <think>...</think>; the JSON
            # is what follows. Strip it before looking, or the greedy brace scan
            # swallows the reasoning and every parse fails as malformed.
            body_text = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
            if body_text.upper().rstrip(".") == "NO":
                # THE HONEST BUCKET: the model read the test and reports that its
                # assertion is not this clause. This is absent coverage, not a
                # pipeline fault, and it is the only rejection that means so.
                reject("no-such-coverage", r, name, score, text)
                continue
            m = re.search(r"\{.*\}", body_text, re.S)
            if not m:
                reject("no-json", r, name, score, text)
                continue
            try:
                j = json.loads(m.group(0))
            except json.JSONDecodeError as e:
                reject("bad-json", r, name, score, text, str(e)[:200])
                continue
            if not all(k in j for k in ("target", "find", "replace")):
                reject("missing-keys", r, name, score, text,
                       "keys=" + ",".join(sorted(j)))
                continue
            if j["target"] != cpath:
                # It was told the one legal target. Naming another file means it
                # is guessing again, which is the v1 failure by another route.
                reject("wrong-target", r, name, score, text,
                       f'said {j["target"]}, shown {cpath}')
                continue
            tgt = ROOT / j["target"]
            # Cheap pre-filters. The runner would report STALE anyway, but a
            # bank full of dead mutants wastes whole suite runs.
            if not tgt.is_file():
                reject("target-missing", r, name, score, text, j["target"])
                continue
            n = tgt.read_text().count(j["find"])
            if n == 0:
                reject("find-absent", r, name, score, text, j["find"][:120])
                continue
            if n > 1:
                reject("find-ambiguous", r, name, score, text,
                       f"{n}x {j['find'][:100]}")
                continue
            # A MUTATION INSIDE TEST CODE IS NOT EVIDENCE, AND IT IS THE MODEL'S
            # FAVOURITE ANSWER. Measured on the first probe (2026-09-01): two of
            # three candidates edited the test body — one inverted an assertion
            # (`is_none()` -> `is_some()`), which reddens the test and proves
            # NOTHING about the product, and would have been recorded CAUGHT.
            # The prompt asks for production code and is ignored, so the rule is
            # enforced here instead of requested there.
            body = tgt.read_text()
            site = body[: body.index(j["find"])]
            if "#[cfg(test)]" in site or "mod tests" in site:
                reject("test-code", r, name, score, text, j["target"])
                continue
            if trace:
                trace.write(json.dumps({
                    "requirement": r["id"], "test": name, "score": round(score, 3),
                    "verdict": "candidate", "detail": j["target"],
                    "response": text[:2000],
                }) + "\n")
                trace.flush()
            mutants.append({
                "id": f"{r['id'].lower()}-{name[:40]}", "requirement": r["id"],
                "target": j["target"], "find": j["find"], "replace": j["replace"],
                "mustFail": [name], "score": round(score, 3),
                "breaks": r["text"][:200],
            })
        print(f"  {r['id']}: {len([m for m in mutants if m['requirement']==r['id']])} candidate(s)",
              file=sys.stderr, flush=True)
        # Written after EVERY requirement, not once at the end. A 40-minute run
        # (the GR family; the full registry is hours) previously discarded every
        # candidate it had found if it died at minute 39.
        write_bank(ROOT / a.out, mutants, rejects)

    total = sum(rejects.values())
    write_bank(ROOT / a.out, mutants, rejects)
    if trace:
        trace.close()
    print(f"candidates: wrote {len(mutants)} to {a.out}; {total} rejected",
          file=sys.stderr)
    for k, v in sorted(rejects.items(), key=lambda kv: -kv[1]):
        print(f"  {k:<18} {v:>4}", file=sys.stderr)
    # The one bucket that is a FINDING rather than a fault.
    honest = rejects["no-such-coverage"]
    print(f"  -> {honest}/{total} rejections are absent coverage; "
          f"{total - honest} are the pipeline failing to express a candidate",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
