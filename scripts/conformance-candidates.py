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
import argparse, json, random, re, subprocess, sys, time, tomllib, urllib.request
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


# RANKING IS A MATRIX MULTIPLY, NOT A PYTHON LOOP. 582 requirements against
# 11,786 tests at 1024 dims is 7 billion multiply-adds; in pure Python that is
# 20-40 minutes of setup EVERY run, which is most of what makes a resume
# expensive. numpy does the same arithmetic in about a second.
#
# The vectors are L2-normalised ONCE, up front, so the per-requirement step is
# a plain dot product and `argpartition` takes the top k without sorting 11,786
# rows 582 times.
def normalise(mv):
    import numpy as np
    m = np.asarray(mv, dtype=np.float32)
    n = np.linalg.norm(m, axis=1, keepdims=True)
    n[n == 0] = 1.0
    return m / n


def cosine_top(qv, mat, names, k):
    import numpy as np
    q = np.asarray(qv, dtype=np.float32)
    qn = float(np.linalg.norm(q)) or 1.0
    sims = mat @ (q / qn)
    k = min(k, len(names))
    idx = np.argpartition(-sims, k - 1)[:k]
    idx = idx[np.argsort(-sims[idx])]
    return [(float(sims[i]), names[i]) for i in idx]


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
# Each unresolvable name still costs a round trip, so the walk is bounded.
#
# IT WAS 6, AND THAT ASSUMPTION WAS WRONG. The bound used to be justified as
# "the function a test is about is called early in its body, not on line 40".
# Measured against this script's own rejections on the FE probe (2026-09-01):
# ALL 15 `no-code-under-test` verdicts came from tests carrying 9 to 14
# distinct candidate call names in the window, so the walk stopped at 6 and
# reported "nothing first-party resolved" for a target it had not yet looked
# at. That bucket was the second largest in the run, and two of its entries
# were the DST invariant falsifiers — tests that plainly do call production
# code. The bound now covers the observed range.
#
# Cost is bounded and small: a pair that RESOLVES returns immediately, so only
# rejections walk the full list, and `_sym_cache` is shared across pairs.
MAX_LOOKUPS = 14
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
- DO NOT judge whether the test's WORDING matches the requirement's WORDING.
  The requirement is a guide to a BEHAVIOUR. If this test exercises that
  behaviour at all, propose the edit. Something else decides whether the test
  actually depends on it.
- Answer NO only if this test could not possibly be affected by the behaviour
  the requirement describes — a different subsystem entirely.

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
    ap.add_argument("--resume", action="store_true",
                    help="skip pairs already recorded in --trace and keep the "
                         "candidates already in --out; safe to stop and restart")
    ap.add_argument("--jobs", type=int, default=4,
                    help="concurrent model requests; the daemon batches them")
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
    tv = normalise(embed_cached([n.replace("_", " ") for n in names], a.embed_cache))
    print("embedding requirements…", file=sys.stderr)
    rv = embed([r["text"] for r in reqs])

    # WHY EVERY REJECTION IS NAMED AND COUNTED SEPARATELY. The first full-family
    # probe (2026-09-01, GR, 50 requirements x 5) returned 0 of 250 behind a
    # SINGLE `skipped` counter. "Zero candidates" from a collapsed counter cannot
    # distinguish the model DECLINING to connect a clause to a test
    # (`model-declined`, which is an opinion and not evidence) from an
    # instrument defect (`find-absent`: the snippet it quoted is not in the
    # file). Those two readings send the campaign in opposite directions --
    # write coverage, or fix the extractor -- so the buckets are the
    # measurement, not the total. Neither is proof that coverage is absent;
    # only a SURVIVED mutation is.
    # ARCH_PRINCIPLES 18.3: absence is REPORTED, never defaulted.
    from collections import Counter
    mutants, rejects = [], Counter()
    # APPEND on resume. Opening "w" here would truncate the very file the
    # resume logic below reads to learn what is already done — the log would
    # erase itself at the moment it was needed.
    trace = open(ROOT / a.trace, "a" if a.resume else "w") if a.trace else None

    def reject(reason, r, name, score, text=None, detail=None):
        rejects[reason] += 1
        if trace:
            trace.write(json.dumps({
                "requirement": r["id"], "test": name, "score": round(score, 3),
                "verdict": reason, "detail": detail,
                # `response_chars` is the FULL length; `response` is clipped for
                # the log. Without the length beside it, a response clipped at
                # 2000 here is indistinguishable from a model that stopped at
                # 2000 — which is precisely how a token ceiling got diagnosed as
                # "the model emits more malformed output" (ARCH §18.4: validate
                # the instrument before the result).
                "response_chars": len(text or ""),
                "response": (text or "")[:2000],
            }) + "\n")
            trace.flush()

    # WHY THIS IS A THREAD POOL. The daemon serves concurrent completions far
    # better than one at a time — measured 2026-09-01 on this host, 3 requests
    # took 7.2s sequentially and 0.8s together (9.4x; better than linear,
    # because they batch). Sequentially the full 582-requirement registry is
    # about eight hours, which is the difference between an overnight job and
    # one that is still running tomorrow afternoon.
    #
    # Each worker DECIDES ONE PAIR AND RETURNS ITS OUTCOME. Nothing shared is
    # mutated inside a worker, so there is no lock to get wrong: the counters,
    # the trace and the bank are written by the main thread as results arrive.
    from concurrent.futures import ThreadPoolExecutor

    def decide(job):
        """One (requirement, test) pair -> (verdict, model_text, payload).

        `payload` is the mutant dict when the verdict is "candidate", and the
        rejection detail otherwise. Pure with respect to shared state.
        """
        r, score, name = job
        path, line = tests[name]
        src = "\n".join((ROOT / path).read_text().splitlines()[max(0, line - 2):line + 40])
        under = code_under_test(src, path)
        if under is None:
            # No first-party callee resolved. Nothing to mutate, and asking the
            # model anyway is what produced v1's invented snippets.
            return ("no-code-under-test", None, None)
        cpath, csrc = under
        # THE SLOTS ARE FINITE AND A BUSY DAEMON SAYS SO. At 8 workers, 16 of 30
        # requests came back 503 (measured 2026-09-01) — a rejection bucket that
        # says nothing about coverage and would have polluted the result if it
        # were counted as one. Backpressure is retried, not recorded: a 503 is
        # the daemon asking us to wait, and only a request that fails every
        # attempt becomes a verdict.
        text, last = None, None
        for attempt in range(4):
            try:
                resp = post("/v1/chat/completions", {
                    # 600 CUT 9% OF THE YIELD MID-STRING. Measured on the
                    # 100-pair FE probe (2026-09-01): every one of the 9
                    # `bad-json` rejections was a truncation, not malformed
                    # output — `Unterminated string`, the model stopped inside
                    # `find` or `replace` with the brace never closed. A mutant
                    # carries two whole Rust function bodies, which 600 tokens
                    # does not fit. This costs nothing on short answers (they
                    # stop at EOS); it only lets the long ones finish.
                    "model": a.model, "temperature": 0.2, "max_tokens": 2000,
                    "messages": [{"role": "user", "content": PROMPT.format(
                        rid=r["id"], rtext=r["text"], tname=name, tfile=path,
                        tsrc=src, cpath=cpath, csrc=csrc)}],
                })
                text = resp["choices"][0]["message"]["content"]
                break
            except Exception as e:
                last = str(e)[:200]
                time.sleep(1.5 * (attempt + 1) + random.random())
        if text is None:
            return ("daemon-error", None, last)
        # A reasoning model wraps its answer in <think>...</think>; the JSON is
        # what follows. Strip it before looking, or the greedy brace scan
        # swallows the reasoning and every parse fails as malformed.
        body_text = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
        if body_text.upper().rstrip(".") == "NO":
            # THE MODEL DECLINED. Read this as "the model saw no connection",
            # NOT as "no coverage exists".
            #
            # OPERATOR CORRECTION 2026-09-01: we are not Goodharting to the
            # clause as written. The spec is a comprehensive GUIDE and a test
            # answers its SPIRIT. Asked to compare a clause's wording against a
            # test's wording, a small model says NO whenever the vocabulary
            # differs — which is why FE returned 65 of these out of 85
            # rejections while retrieval was working correctly (77% of the
            # tests it retrieved were genuinely mesh tests). It was judging
            # PROSE.
            #
            # The mutation adjudicator does not care about wording at all: it
            # asks whether breaking the behaviour kills the named test, which
            # is the spirit question, mechanically answered. So this stage
            # needs RECALL (the docstring above says so) and the prompt now
            # tells the model to propose unless the test is in a different
            # subsystem entirely.
            return ("model-declined", text, None)
        m = re.search(r"\{.*\}", body_text, re.S)
        if not m:
            return ("no-json", text, None)
        try:
            j = json.loads(m.group(0))
        except json.JSONDecodeError as e:
            return ("bad-json", text, str(e)[:200])
        if not all(k in j for k in ("target", "find", "replace")):
            return ("missing-keys", text, "keys=" + ",".join(sorted(j)))
        if j["target"] != cpath:
            # It was told the one legal target. Naming another file means it is
            # guessing again, which is the v1 failure by another route.
            return ("wrong-target", text, f'said {j["target"]}, shown {cpath}')
        tgt = ROOT / j["target"]
        # Cheap pre-filters. The runner would report STALE anyway, but a bank
        # full of dead mutants wastes whole suite runs.
        if not tgt.is_file():
            return ("target-missing", text, j["target"])
        body = tgt.read_text()
        n = body.count(j["find"])
        if n == 0:
            return ("find-absent", text, j["find"][:120])
        if n > 1:
            # AMBIGUITY IS AN ANCHOR THAT IS TOO SHORT, NOT A BAD CANDIDATE.
            # All 6 on the FE probe (2026-09-01) were exactly 2x, and four of
            # them were the same one-line struct field (`pub mesh_proof:
            # Option<String>,`) that occurs in two structs. Widen the anchor
            # upward, a line at a time, until it names one site — prepending
            # the SAME lines to `replace` so the edit is unchanged.
            #
            # This picks the FIRST occurrence, which the model may not have
            # meant. That is safe here and nowhere else: a mutant aimed at the
            # wrong site SURVIVES its mutation and is discarded by
            # sabotage.py. Recall is what this stage owes; precision comes
            # from the adjudicator (ARCH §18.1 — a wrong candidate costs a
            # slot, never a false claim).
            head = body[: body.index(j["find"])].split("\n")
            if head and head[-1] == "":
                # The prefix ends AT the newline before `find`, so split leaves
                # a trailing "". Keeping it makes every `pad` inject a blank
                # line and match nothing — measured: 0 of 6 resolved with it,
                # 6 of 6 without.
                head = head[:-1]
            for back in range(1, 25):
                if back > len(head):
                    break
                pad = "\n".join(head[-back:]) + "\n"
                if body.count(pad + j["find"]) == 1:
                    j["find"] = pad + j["find"]
                    j["replace"] = pad + j["replace"]
                    break
            n = body.count(j["find"])
        if n > 1:
            return ("find-ambiguous", text, f"{n}x {j['find'][:100]}")
        # A MUTATION INSIDE TEST CODE IS NOT EVIDENCE, AND IT IS THE MODEL'S
        # FAVOURITE ANSWER. Measured on the first probe (2026-09-01): two of
        # three candidates edited the test body — one inverted an assertion
        # (`is_none()` -> `is_some()`), which reddens the test and proves
        # NOTHING about the product, and would have been recorded CAUGHT. The
        # prompt asks for production code and is ignored, so the rule is
        # enforced here instead of requested there.
        site = body[: body.index(j["find"])]
        if "#[cfg(test)]" in site or "mod tests" in site:
            return ("test-code", text, j["target"])
        return ("candidate", text, {
            "id": f"{r['id'].lower()}-{name[:40]}", "requirement": r["id"],
            "target": j["target"], "find": j["find"], "replace": j["replace"],
            # The BARE function name, deliberately. sabotage.py resolves it
            # against the key space of the baseline report it just ran and
            # refuses ambiguity rather than guessing. Deriving the full
            # `<binary id>::<module>::<fn>` key here would put a second
            # key-deriver in the tree, disagreeing with the report that is the
            # only authority on it (ARCH §10.6).
            "mustFail": [name], "score": round(score, 3),
            "breaks": r["text"][:200],
        })

    jobs = [(r, score, name)
            for r, qv in zip(reqs, rv)
            for score, name in cosine_top(qv, tv, names, a.top_k)]

    # RESUME. A full-registry pass is hours, and the daemon it runs on is the
    # machine's, not this script's — the operator will want it back. Every pair
    # this script decides is appended to the trace before the next one starts,
    # so the trace IS the resume log: re-running with the same --trace skips
    # what has already been decided and keeps the bank it already wrote.
    #
    # Keyed on (requirement, test), not on a counter, because the job list is
    # rebuilt from the registry each run and a positional offset would silently
    # shift if the registry or the test inventory changed underneath it.
    done = set()
    if a.resume:
        tp = ROOT / a.trace if a.trace else None
        if tp and tp.is_file():
            for line in tp.read_text().splitlines():
                try:
                    row = json.loads(line)
                except json.JSONDecodeError:
                    continue            # a torn last line: that pair re-runs
                done.add((row["requirement"], row["test"]))
                if row["verdict"] != "candidate":
                    rejects[row["verdict"]] += 1
        bp = ROOT / a.out
        if bp.is_file():
            for m in tomllib.load(open(bp, "rb")).get("mutant", []):
                mutants.append({**m, "score": m.get("score", 0.0)})
        before = len(jobs)
        jobs = [j for j in jobs if (j[0]["id"], j[2]) not in done]
        print(f"candidates: resuming — {before - len(jobs)} pair(s) already "
              f"decided, {len(mutants)} candidate(s) carried forward",
              file=sys.stderr)

    print(f"candidates: {len(jobs)} pair(s) over {a.jobs} worker(s)", file=sys.stderr)

    seen_per_req = {}
    for rid, _t in done:
        seen_per_req[rid] = seen_per_req.get(rid, 0) + 1
    with ThreadPoolExecutor(max_workers=a.jobs) as ex:
        for job, out in zip(jobs, ex.map(decide, jobs)):
            r, score, name = job
            verdict, text, payload = out
            if verdict == "candidate":
                mutants.append(payload)
            else:
                rejects[verdict] += 1
            if trace:
                trace.write(json.dumps({
                    "requirement": r["id"], "test": name, "score": round(score, 3),
                    "verdict": verdict,
                    "detail": payload["target"] if verdict == "candidate" else payload,
                    "response_chars": len(text or ""),
                    "response": (text or "")[:2000],
                }) + "\n")
                trace.flush()
            # THE BANK AND THE TRACE MUST STAY IN LOCKSTEP, so the bank is
            # written after EVERY pair rather than at each requirement's end.
            # Otherwise a candidate found in a half-finished requirement is in
            # the trace (so resume skips its pair) but not in the bank (so
            # resume cannot carry it) — and it is silently lost. Rewriting a
            # few hundred rows per pair is nothing next to a model call.
            write_bank(ROOT / a.out, mutants, rejects)
            seen_per_req[r["id"]] = seen_per_req.get(r["id"], 0) + 1
            if seen_per_req[r["id"]] == a.top_k:
                n = len([m for m in mutants if m["requirement"] == r["id"]])
                print(f"  {r['id']}: {n} candidate(s)", file=sys.stderr, flush=True)

    total = sum(rejects.values())
    write_bank(ROOT / a.out, mutants, rejects)
    if trace:
        trace.close()
    print(f"candidates: wrote {len(mutants)} to {a.out}; {total} rejected",
          file=sys.stderr)
    for k, v in sorted(rejects.items(), key=lambda kv: -kv[1]):
        print(f"  {k:<18} {v:>4}", file=sys.stderr)
    # The one bucket that is a FINDING rather than a fault.
    declined = rejects["model-declined"]
    print(f"  -> {declined}/{total} rejections are the model declining to "
          f"connect the clause to the test (NOT proof of absent coverage); "
          f"{total - declined} are the pipeline failing to express a candidate",
          file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
