#!/usr/bin/env python3
"""The RACE judge instrument — ONE decider for how a judge call is made and
what counts as a scorable verdict (§10.6).

Two scorers drive the same judge: `drb/overall-derivation/score_race.py` (the
10-task subset instrument) and `arms/lab/score_one.py` (single-task
iteration). They differ legitimately in retry recipe and in what they read off
disk. They must NOT differ in the two things below, because a number produced
under one and compared against a number produced under the other is not a
comparison.
"""

# ── INSTRUMENT AMENDMENT 2026-08-23: the judge is made deterministic ────────
#
# The vendored client sends `model`, `messages`, `max_completion_tokens` and
# `reasoning_effort` — and NO sampling parameters (utils/api.py:167-172). The
# local daemon therefore applies its own default, and
# `InferenceConfig::default().temperature` is **0.7**
# (sovereign-contracts/src/types/mod.rs:107). Every RACE number this campaign
# recorded before this pin — the 17.3751 baseline, the 43.6696 Perplexity bar,
# the 44.3995 composite, every per-task delta — is ONE DRAW from a
# temperature-0.7 process.
#
# Measured, and this is why the pin exists: task 56's IDENTICAL article,
# re-judged, scored **46.2359** against its recorded **43.1843**. A +3.05
# swing on unchanged input, against a margin-of-interest of +0.52.
#
# The official protocol scores 100 tasks, so per-call noise averages out. We
# score 10, where it dominates. Since the local 27B is already a declared
# substitution for the official gemini/GPT-5.5-class judge, determinism is
# worth more here than fidelity to that judge's sampler.
#
# Consequence, and it is not optional: the Perplexity bar and our own arm are
# BOTH re-measured under this pin before any comparison is made. A pinned
# reading cannot be compared against an unpinned one.
JUDGE_TEMPERATURE = 0.0
JUDGE_TOP_P = 1.0

DIMS = ["comprehensiveness", "insight", "instruction_following", "readability"]


def pin_sampling(client):
    """Force greedy decoding on the vendored client without editing the
    pinned clone. `_post` is the single place every judge payload passes
    through, so wrapping it is the one decider (§10.6) — adding the
    parameters at each call site would let two paths disagree."""
    original_post = client._post

    def post(payload):
        return original_post(dict(payload,
                                  temperature=JUDGE_TEMPERATURE,
                                  top_p=JUDGE_TOP_P))

    client._post = post
    return client


def unscorable(out, dims=DIMS):
    """Why this judge output cannot be scored, or None if it can.

    The official driver checks only that each dimension KEY exists. A key
    whose list is EMPTY passes that check and then scores 0/0 through
    calculate_weighted_scores — the dimension silently contributes nothing to
    EITHER side, and a could-not-judge is recorded as a score. Measured on
    task 65: `readability` came back `[]` and the run reported 46.56 with a
    whole dimension missing. Four verdicts, not two (§18.1): the caller
    retries, then REFUSES. Never a defaulted zero (§18.3)."""
    if not isinstance(out, dict):
        return "judge output is %s, not an object" % type(out).__name__
    missing = [d for d in dims if d not in out]
    empty = [d for d in dims if d in out and not out[d]]
    if missing or empty:
        return "missing dims %s, empty dims %s" % (missing, empty)
    return None


# ── INSTRUMENT AMENDMENT 2026-08-26: the judge ENDPOINT is decided here too ──
#
# The defect this closes, found mid-flight. `score_race.py`'s guard
# (`served_models`) defaulted to the local daemon, while the vendored
# `utils/api.py` resolves ITS endpoint at import time from `LLM_BACKEND`, which
# defaults to `openrouter`. Both bed runners export LLM_BACKEND/OPENAI_BASE_URL/
# OPENAI_API_KEY, so the trap never fired through them — but a scorer invoked
# directly, the way its own docstring documents it, verified that OUR daemon
# serves the pinned judge and then sent every judge call to OpenRouter, using
# whatever OPENROUTER_API_KEY happened to be in the ambient environment.
#
# Two deciders for one endpoint (§10.6) whose failure mode is a silent
# substitution of the instrument (§18.3): the run emits numbers reported as
# "our pinned 27B ruler" that some other service produced. The guard passing is
# what makes it dangerous — it reads as verified.
#
# So the endpoint joins the sampling pin as instrument state: decided once,
# here, and enforced for every scorer that drives this judge.
LOCAL_HOSTS = ("127.0.0.1", "localhost", "::1", "0.0.0.0")
DEFAULT_JUDGE_BASE_URL = "http://127.0.0.1:9741/v1"


# The vendored client also freezes its socket timeout at import
# (utils/api.py:82, `LLM_HTTP_TIMEOUT`, default 600s). Task 83 is the largest
# prompt in the subset — a ~40k-token reference plus the article — and at this
# host's prefill rate it needs ~8 min before the first token. At 600s it is cut
# off CLIENT-side and recorded NEVER-RAN, which reads as a model refusal and is
# not one. A judge call is allowed to be slow; it is not allowed to be silently
# cut off. `score_one.py` carried this fix in its own copy while `score_race.py`
# did not — which is exactly why both now live here.
JUDGE_HTTP_TIMEOUT_S = "3600"


def configure_judge_client() -> str:
    """Decide the judge endpoint and socket timeout, refuse a remote endpoint,
    and set the env the vendored client reads AT IMPORT TIME.

    MUST be called before `from utils.api import AIClient` — api.py binds
    LLM_BACKEND, its base URL and its timeout at module import, so setting them
    afterwards changes nothing and the call still leaves the host.

    Returns the resolved base URL, which the caller's `/models` guard must use
    so the endpoint it verifies is the endpoint the judge calls reach.
    """
    import os
    import sys

    base = os.environ.get("OPENAI_BASE_URL", DEFAULT_JUDGE_BASE_URL)
    if not os.environ.get("SOVEREIGN_RACE_ALLOW_REMOTE_JUDGE"):
        host = base.split("//", 1)[-1].split("/", 1)[0].split(":")[0]
        if host not in LOCAL_HOSTS:
            sys.exit(
                f"exit 3: judge endpoint {base} is not local. Every RACE number "
                f"this campaign holds is defined as our pinned judge on our own "
                f"daemon; scoring against a remote service substitutes the "
                f"instrument without saying so (ARCH 18.3), and bills for it. "
                f"Set SOVEREIGN_RACE_ALLOW_REMOTE_JUDGE=1 to override — and name "
                f"the substitution wherever the number is reported."
            )
    os.environ["LLM_BACKEND"] = "openai"
    os.environ["OPENAI_BASE_URL"] = base
    os.environ.setdefault("OPENAI_API_KEY", "local")
    os.environ.setdefault("LLM_HTTP_TIMEOUT", JUDGE_HTTP_TIMEOUT_S)
    return base
