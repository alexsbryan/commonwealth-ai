#!/usr/bin/env python3
"""Author the model-lane generalization bank (NEXT_EDIT.md §6, gen/README.md).

Every case is hand-written code in this file — there is no git mining,
because generalization episodes need intent a harvester cannot infer.
Build-time validation replays each case through:

  1. the rule-lane replica (imported from ../harvest.py) — asserting the
     rule lane really is silent (or fires, for the rule-owns negatives), and
  2. a CONSULT-GATE REPLICA (below) — written from the same spec
     `next_edit_model.rs` implements. A replica<->Rust divergence fails the
     eval loudly whichever side is wrong; a replica<->expectation divergence
     fails right here at authoring time.

Deterministic: no RNG, no timestamps. Re-running rewrites cases.jsonl
byte-identically.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import harvest  # noqa: E402  (the rule-lane replica)

unit = harvest.unit

# ---- consult-gate replica (mirror of next_edit_model.rs) --------------
#
# Evaluated only when the rule lane returned no edits. Decides whether the
# model lane may be consulted, why, and which needle guides region choice.

MIN_PARAM_PREFIX = 4  # common-prefix chars two afters must share
MIN_NEEDLE = 3        # a shorter LCS is noise, not an anchor

STYLES = ("snake", "screaming", "camel", "pascal")


def word_runs(s: str) -> list[tuple[bool, str]]:
    runs: list[tuple[bool, str]] = []
    cur, mode = "", None
    for ch in s:
        m = ch.isalnum() or ch == "_"
        if mode is None or m == mode:
            cur += ch
            mode = m
        else:
            runs.append((mode, cur))
            cur, mode = ch, m
    if cur:
        runs.append((mode, cur))
    return runs


def split_words(run: str) -> list[str]:
    """getUserData -> [get,user,data]; PARSE_ARGS -> [parse,args]."""
    parts: list[str] = []
    for piece in run.split("_"):
        w = ""
        for ch in piece:
            if w and ch.isupper() and not w[-1].isupper():
                parts.append(w)
                w = ch
            else:
                w += ch
        if w:
            parts.append(w)
    return [p.lower() for p in parts]


def render_words(words: list[str], style: str) -> str:
    if style == "snake":
        return "_".join(words)
    if style == "screaming":
        return "_".join(w.upper() for w in words)
    if style == "camel":
        return words[0] + "".join(w.capitalize() for w in words[1:])
    return "".join(w.capitalize() for w in words)  # pascal


def restyle(s: str, style: str) -> str:
    out = []
    for is_word, seg in word_runs(s):
        out.append(render_words(split_words(seg), style) if is_word else seg)
    return "".join(out)


def casing_variant_needle(text: str, rule: dict) -> str | None:
    """First casing rendering of rule.find with a live guarded site."""
    for style in STYLES:
        vfind = restyle(rule["find"], style)
        if vfind == rule["find"]:
            continue
        vrep = restyle(rule["replace"], style)
        gl = bool(vfind[:1]) and (vfind[0].isalnum() or vfind[0] == "_")
        gr = bool(vfind[-1:]) and (vfind[-1].isalnum() or vfind[-1] == "_")
        if harvest.find_guarded_sites(text, vfind, gl, gr, vrep):
            return vfind
    return None


def lcsubstr(a: str, b: str) -> str:
    best_len, best_end = 0, 0
    prev = [0] * (len(b) + 1)
    for i in range(1, len(a) + 1):
        cur = [0] * (len(b) + 1)
        for j in range(1, len(b) + 1):
            if a[i - 1] == b[j - 1]:
                cur[j] = prev[j - 1] + 1
                if cur[j] > best_len:
                    best_len, best_end = cur[j], i
        prev = cur
    return a[best_end - best_len:best_end]


def common_prefix_len(a: str, b: str) -> int:
    n = 0
    for x, y in zip(a, b):
        if x != y:
            break
        n += 1
    return n


def ctx_needle(a: dict, b: dict) -> str | None:
    s = lcsubstr(
        a["left"] + a["before"] + a["right"],
        b["left"] + b["before"] + b["right"],
    ).strip()
    return s if len(s) >= MIN_NEEDLE else None


def gate(history: list[dict], text: str, prediction: dict) -> dict:
    def skip(reason):
        return {"consulted": False, "reason": None, "needle": None,
                "skipped": reason}

    def consult(reason, needle):
        return {"consulted": True, "reason": reason, "needle": needle,
                "skipped": None}

    if prediction["fire"]:
        return skip("rule_fired")
    cores = [u for u in history[-harvest.HISTORY_WINDOW:]
             if u["before"] != u["after"]]
    if len(cores) < 2:
        return skip("gate")
    a, b = cores[-1], cores[-2]
    ra, rb = harvest.expand_rule(a), harvest.expand_rule(b)

    # 1. Casing variant: the literal rule is real (support >= 2) and
    # exhausted, but the same rename remains at another casing.
    # DETECTED but DEFERRED in v1 (mirrors next_edit_model.rs): bank
    # runs 1-2 showed Mellum2 destructive on exactly this shape — the
    # category re-activates when the deterministic rule sub-lane lands.
    if (prediction["rule"] is not None and prediction["reason"] == "no_sites"
            and prediction["support"] >= 2):
        needle = casing_variant_needle(text, prediction["rule"])
        if needle:
            return skip("casing_deferred")

    def multiline(u):
        return "\n" in u["before"] or "\n" in u["after"]

    # 2. Multiline fan-out: identical multi-line insertion at two sites —
    # the rule lane declines multiline units by design.
    if (ra is None and rb is None and multiline(a) and multiline(b)
            and a["before"] == b["before"] and a["after"] == b["after"]
            and len(a["after"].strip()) >= MIN_NEEDLE):
        return consult("multiline_fanout", ctx_needle(a, b))

    if ra is not None and rb is not None and harvest.rule_key(ra) != harvest.rule_key(rb):
        # 3. Fan-out insert: identical cores, differing contexts — induction
        # can never reach support 2 because the expanded rules differ.
        if a["before"] == b["before"] and a["after"] == b["after"]:
            return consult("fanout_insert", ctx_needle(a, b))
        # 4. Param insert: same target, per-site-varying replacement sharing
        # a meaningful prefix (.expect("...") with different messages).
        if (a["before"] == b["before"] and a["after"] != b["after"]
                and common_prefix_len(a["after"], b["after"]) >= MIN_PARAM_PREFIX):
            needle = a["before"] if a["before"].strip() else ctx_needle(a, b)
            return consult("param_insert", needle)
    return skip("gate")


# ---- condition evaluation (shared with scripts/next_edit_gen_eval.py) --

def cond_holds(doc: str, cond: dict) -> bool:
    if "count" in cond:
        s, n = cond["count"]
        return doc.count(s) == n
    if "count_ne" in cond:
        s, n = cond["count_ne"]
        return doc.count(s) != n
    if "recount" in cond:
        pat, n = cond["recount"]
        return len(re.findall(pat, doc)) == n
    if "contains" in cond:
        return cond["contains"] in doc
    if "not_contains" in cond:
        return cond["not_contains"] not in doc
    raise ValueError(f"unknown cond {cond}")


# ---- case assembly ----------------------------------------------------

KIND_OF = {"cv": "deferred_casing", "sf": "positive", "pi": "positive",
           "fi": "positive", "gn": "gate_negative", "mn": "model_negative"}
CATEGORY_OF = {"cv": "casing_variant", "sf": "signature_fanout",
               "pi": "param_insert", "fi": "field_init"}

CASES: list[dict] = []


def gen(cid: str, language: str, path: str, history: list[dict], text: str,
        cursor_marker: str, expect: dict, note: str | None = None) -> None:
    prefix = cid[:2]
    kind = KIND_OF[prefix]
    idx = text.index(cursor_marker) + len(cursor_marker)
    cursor_u16 = harvest.chars_to_u16(text, [idx])[idx]

    # -- build-time validation ------------------------------------------
    assert text.count("\n") <= 22, f"{cid}: doc must fit one region (<=22 lines)"
    for u in history:
        for f in ("before", "after", "left", "right"):
            assert len(u[f].encode()) <= 2048, f"{cid}: unit field over cap"
    p = harvest.predict(history, text, idx)
    g = gate(history, text, p)
    assert g["consulted"] == expect["consult"], \
        f"{cid}: gate replica says consulted={g['consulted']} " \
        f"(skipped={g['skipped']}), case expects {expect['consult']}; " \
        f"rule-lane: {p['reason'] if not p['fire'] else 'FIRED'}"
    if expect["consult"]:
        assert g["reason"] == expect["consult_reason"], \
            f"{cid}: gate reason {g['reason']} != {expect['consult_reason']}"
    if expect.get("not_consulted_reason"):
        assert g["skipped"] == expect["not_consulted_reason"], \
            f"{cid}: skipped {g['skipped']} != {expect['not_consulted_reason']}"
    if expect.get("engine") == "rule":
        assert p["fire"], f"{cid}: expects rule-lane fire but replica is silent"
    else:
        assert not p["fire"], f"{cid}: rule lane fired; gen cases must start silent"
    # Conds must be meaningful pre-apply: wrong-conds all false (else a
    # no-op model would be 'wrong'), and for firing positives at least one
    # correct-cond false (else a no-op model would be 'correct').
    for cond in expect.get("wrong", []):
        assert not cond_holds(text, cond), f"{cid}: wrong-cond {cond} already true"
    if expect.get("fire") and expect.get("engine") != "rule":
        assert any(not cond_holds(text, c) for c in expect.get("correct", [])), \
            f"{cid}: all correct-conds already hold pre-apply"
        assert expect.get("correct"), f"{cid}: firing positive needs correct conds"
    if kind == "model_negative":
        assert expect["consult"] and not expect["fire"], \
            f"{cid}: model_negative = consulted but correct output is silence"

    CASES.append({
        "id": cid,
        "kind": kind,
        "category": CATEGORY_OF.get(prefix),
        "language": language,
        "request": {
            "history": history,
            "text": text,
            "cursor": cursor_u16,
            "path": path,
            "language": language,
            "debug": True,
            "model_lane": True,
        },
        "expect": {
            "consult": expect["consult"],
            "consult_reason": expect.get("consult_reason"),
            "not_consulted_reason": expect.get("not_consulted_reason"),
            "fire": expect["fire"],
            "engine": expect.get("engine"),
            "correct": expect.get("correct", []),
            "wrong": expect.get("wrong", []),
            "needle": g["needle"],
        },
        **({"note": note} if note else {}),
    })


# ======================================================================
# cv — casing_variant positives: literal sites exhausted, the same rename
# remains at another casing. Rule lane: silent (no_sites).
# ======================================================================

gen("cv01", "typescript", "src/profile.ts",
    [unit("getUserData", "fetchUserData", "  const raw = ", "(userId);"),
     unit("getUserData", "fetchUserData", "  const avatar = ", "(userId + \"/avatar\");")],
    'async function loadProfile(userId: string) {\n'
    '  const raw = fetchUserData(userId);\n'
    '  const avatar = fetchUserData(userId + "/avatar");\n'
    '  return render(raw, avatar);\n'
    '}\n'
    '\n'
    '// legacy snake_case alias kept for the CLI surface\n'
    'export const get_user_data = (id: string) => fetchUserData(id);\n'
    '\n'
    'test("alias stays in sync", () => {\n'
    '  expect(get_user_data("42")).toEqual(fetchUserData("42"));\n'
    '});\n',
    '"/avatar");',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["get_user_data", 0]}, {"count": ["fetch_user_data", 2]}],
     "wrong": [{"count_ne": ["fetchUserData(", 4]}]})

gen("cv02", "rust", "src/retry.rs",
    [unit("HttpRetry", "NetRetry", "pub struct ", " {"),
     unit("HttpRetry", "NetRetry", "impl ", " {")],
    'pub struct NetRetry {\n'
    '    pub max: u32,\n'
    '    pub base_ms: u64,\n'
    '}\n'
    '\n'
    'impl NetRetry {\n'
    '    pub fn backoff(&self, attempt: u32) -> u64 {\n'
    '        self.base_ms << attempt.min(self.max)\n'
    '    }\n'
    '}\n'
    '\n'
    '#[derive(Deserialize)]\n'
    'pub struct RawConfig {\n'
    '    pub http_retry: NetRetry,\n'
    '}\n'
    '\n'
    'pub fn retry_of(cfg: &RawConfig) -> &NetRetry {\n'
    '    &cfg.http_retry\n'
    '}\n',
    'impl NetRetry',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["http_retry", 0]}, {"count": ["net_retry", 2]}],
     "wrong": [{"count_ne": ["NetRetry", 4]}]})

gen("cv03", "python", "cli/args.py",
    [unit("parse_args", "build_args", '    "cli": ', ","),
     unit("parse_args", "build_args", '    "rpc": ', ",")],
    'def build_args(argv):\n'
    '    return _parse(argv, STRICT)\n'
    '\n'
    'HANDLERS = {\n'
    '    "cli": build_args,\n'
    '    "rpc": build_args,\n'
    '}\n'
    '\n'
    '# plugin ABI: SCREAMING re-export, do not remove\n'
    'PARSE_ARGS = build_args\n',
    '"rpc": build_args',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["PARSE_ARGS", 0]}, {"count": ["BUILD_ARGS", 1]}],
     "wrong": [{"count_ne": ["build_args", 4]}]})

gen("cv04", "go", "stats/stats.go",
    [unit("UserCount", "MemberCount", "\t", ' int `json:"user_count"`'),
     unit("UserCount", "MemberCount", "\t", ' int `json:"user_count"`')],
    'type DailyStats struct {\n'
    '\tMemberCount int `json:"user_count"`\n'
    '\tErrors      int `json:"errors"`\n'
    '}\n'
    '\n'
    'type WeeklyStats struct {\n'
    '\tMemberCount int `json:"user_count"`\n'
    '\tPeak        int `json:"peak"`\n'
    '}\n',
    'WeeklyStats struct {\n\tMemberCount',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["user_count", 0]}, {"count": ["member_count", 2]}],
     "wrong": [{"count_ne": ["MemberCount", 2]}]})

gen("cv05", "java", "src/Uploader.java",
    [unit("maxRetries", "maxAttempts", "        int ", " = MAX_RETRIES;"),
     unit("maxRetries", "maxAttempts", "        while (attempt < ", " && !ok) {")],
    'class Uploader {\n'
    '    static final int MAX_RETRIES = 5;\n'
    '\n'
    '    boolean push(Blob blob) {\n'
    '        int maxAttempts = MAX_RETRIES;\n'
    '        int attempt = 0;\n'
    '        boolean ok = false;\n'
    '        while (attempt < maxAttempts && !ok) {\n'
    '            ok = tryPush(blob, attempt++);\n'
    '        }\n'
    '        return ok;\n'
    '    }\n'
    '}\n',
    'maxAttempts && !ok',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["MAX_RETRIES", 0]}, {"count": ["MAX_ATTEMPTS", 2]}],
     "wrong": [{"count_ne": ["maxAttempts", 2]}]})

gen("cv06", "ruby", "lib/image.rb",
    [unit("image_scaler", "image_resizer", "  s = ", " w, h"),
     unit("image_scaler", "image_resizer", "  thumb = ", " 64, 64")],
    'def scale(w, h)\n'
    '  s = image_resizer w, h\n'
    '  thumb = image_resizer 64, 64\n'
    '  [s, thumb]\n'
    'end\n'
    '\n'
    'class ImageScaler\n'
    '  def self.image_resizer(w, h)\n'
    '    ImageScaler.new(w, h)\n'
    '  end\n'
    'end\n',
    'image_resizer 64, 64',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["ImageScaler", 0]}, {"count": ["ImageResizer", 2]}],
     "wrong": [{"count_ne": ["image_resizer", 3]}]})

gen("cv07", "kotlin", "app/Profile.kt",
    [unit("userName", "displayName", "    val ", ": String,"),
     unit("userName", "displayName", "    val label = ", " + suffix")],
    'data class Profile(\n'
    '    val displayName: String,\n'
    '    val id: Long,\n'
    ')\n'
    '\n'
    'fun Profile.asRow(): Map<String, Any> {\n'
    '    return mapOf(\n'
    '        "user_name" to displayName,\n'
    '        "id" to id,\n'
    '    )\n'
    '}\n'
    '\n'
    'fun Profile.label(suffix: String): String {\n'
    '    val label = displayName + suffix\n'
    '    return label\n'
    '}\n',
    'displayName + suffix',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["user_name", 0]}, {"count": ["display_name", 1]}],
     "wrong": [{"count_ne": ["displayName", 3]}]})

gen("cv08", "c", "src/frame.c",
    [unit("read_frame", "pull_frame", "    int n = ", "(fd, buf);"),
     unit("read_frame", "pull_frame", "    int m = ", "(fd, tail);")],
    '#define READ_FRAME(fd, dst) pull_frame((fd), (dst))\n'
    '\n'
    'static char spare[64];\n'
    '\n'
    'static int drain(int fd, char *buf, char *tail) {\n'
    '    int n = pull_frame(fd, buf);\n'
    '    int m = pull_frame(fd, tail);\n'
    '    return n + m + READ_FRAME(fd, spare);\n'
    '}\n',
    'pull_frame(fd, tail);',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["READ_FRAME", 0]}, {"count": ["PULL_FRAME", 2]}],
     "wrong": [{"count_ne": ["pull_frame(", 3]}]})

gen("cv09", "csharp", "src/RetryPolicy.cs",
    [unit("RetryDelay", "BackoffDelay", "    public int ", " { get; set; }"),
     unit("RetryDelay", "BackoffDelay", "        total += ", ";")],
    'class RetryPolicy {\n'
    '    public int BackoffDelay { get; set; }\n'
    '\n'
    '    public int TotalWait(int rounds) {\n'
    '        int total = 0;\n'
    '        for (int i = 0; i < rounds; i++) {\n'
    '            total += BackoffDelay;\n'
    '        }\n'
    '        return total;\n'
    '    }\n'
    '\n'
    '    public RetryPolicy(int retryDelay) {\n'
    '        BackoffDelay = retryDelay;\n'
    '    }\n'
    '}\n',
    'total += BackoffDelay;',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["retryDelay", 0]}, {"count": ["backoffDelay", 2]}],
     "wrong": [{"count_ne": ["BackoffDelay", 3]}]})

gen("cv10", "javascript", "src/alerts.js",
    [unit("sendAlert", "pushAlert", "  ", "(user, msg);"),
     unit("sendAlert", "pushAlert", "  return ", "(admin, text);")],
    'function notify(user, msg) {\n'
    '  pushAlert(user, msg);\n'
    '}\n'
    '\n'
    'function escalate(admin, text) {\n'
    '  return pushAlert(admin, text);\n'
    '}\n'
    '\n'
    '// v1 names kept as aliases until the CLI migrates\n'
    'function send_alert(user, msg) {\n'
    '  return pushAlert(user, msg);\n'
    '}\n'
    'const send_alert_all = (users, msg) => users.map((u) => send_alert(u, msg));\n',
    'return pushAlert(admin, text);',
    {"consult": False, "not_consulted_reason": "casing_deferred", "fire": False,
     "correct": [{"count": ["send_alert(", 0]}, {"count": ["push_alert(", 2]}],
     "wrong": [{"count_ne": ["pushAlert(", 3]}]})

# ======================================================================
# sf — signature_fanout positives: identical insertion, differently-shaped
# call sites. Rule lane: silent (contexts differ -> support never reaches 2).
# ======================================================================

gen("sf01", "go", "net/pool.go",
    [unit("", ", timeoutMS", "\tconn := dial(primaryHost, 8080", ")"),
     unit("", ", timeoutMS", "\tbackup := dial(backupHost, altPort", ")")],
    'func connectAll(cfg Config) []Conn {\n'
    '\ttimeoutMS := cfg.TimeoutMS\n'
    '\tconn := dial(primaryHost, 8080, timeoutMS)\n'
    '\tbackup := dial(backupHost, altPort, timeoutMS)\n'
    '\tmirror := dial(mirrorHost, 9090)\n'
    '\tlocal := dial(localHost, cfg.Port)\n'
    '\treturn []Conn{conn, backup, mirror, local}\n'
    '}\n',
    'altPort, timeoutMS)',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", timeoutMS)", 4]}],
     "wrong": [{"contains": "timeoutMS, timeoutMS"}, {"count_ne": ["dial(", 4]}]})

gen("sf02", "typescript", "src/loaders.ts",
    [unit("", ", { signal }", "  const res = await fetch(profileUrl", ")"),
     unit("", ", { signal }", "  const meta = await fetch(metaUrl", ")")],
    'async function loadAll(signal: AbortSignal) {\n'
    '  const res = await fetch(profileUrl, { signal });\n'
    '  const meta = await fetch(metaUrl, { signal });\n'
    '  const prefs = await fetch(prefsUrl);\n'
    '  const flags = await fetch(flagsUrl);\n'
    '  return Promise.all([res, meta, prefs, flags]);\n'
    '}\n',
    'metaUrl, { signal });',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", { signal })", 4]}],
     "wrong": [{"contains": "{ signal }, { signal }"}, {"count_ne": ["fetch(", 4]}]})

gen("sf03", "python", "pipeline/run.py",
    [unit("", ", ctx=ctx", "    log_event(EV_START", ")"),
     unit("", ", ctx=ctx", "    log_event(EV_AUTH", ")")],
    'def run_pipeline(job, ctx):\n'
    '    log_event(EV_START, ctx=ctx)\n'
    '    log_event(EV_AUTH, ctx=ctx)\n'
    '    token = fetch_token(job)\n'
    '    log_event(EV_TOKEN)\n'
    '    result = execute(job, token)\n'
    '    log_event(EV_DONE)\n'
    '    return result\n',
    'EV_AUTH, ctx=ctx)',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", ctx=ctx)", 4]}],
     "wrong": [{"contains": "ctx=ctx, ctx=ctx"}, {"count_ne": ["log_event(", 4]}]})

gen("sf04", "rust", "src/ui.rs",
    [unit("", ", theme", "    let header = Widget::new(HEADER, bounds_h", ")"),
     unit("", ", theme", "    let footer = Widget::new(FOOTER, bounds_f", ")")],
    'fn build_ui(theme: &Theme) -> Ui {\n'
    '    let header = Widget::new(HEADER, bounds_h, theme);\n'
    '    let footer = Widget::new(FOOTER, bounds_f, theme);\n'
    '    let side = Widget::new(SIDEBAR, bounds_s);\n'
    '    let body = Widget::new(BODY, bounds_b);\n'
    '    Ui { header, footer, side, body }\n'
    '}\n',
    'bounds_f, theme)',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", theme)", 4]}],
     "wrong": [{"contains": "theme, theme"}, {"count_ne": ["Widget::new(", 4]}]})

gen("sf05", "java", "src/Boot.java",
    [unit("", ", StandardCharsets.UTF_8", "        Config base = Config.load(basePath", ");"),
     unit("", ", StandardCharsets.UTF_8", "        Config user = Config.load(userPath", ");")],
    'class Boot {\n'
    '    void init() {\n'
    '        Config base = Config.load(basePath, StandardCharsets.UTF_8);\n'
    '        Config user = Config.load(userPath, StandardCharsets.UTF_8);\n'
    '        Config site = Config.load(sitePath);\n'
    '        Config env = Config.load(envPath);\n'
    '        apply(base, user, site, env);\n'
    '    }\n'
    '}\n',
    'userPath, StandardCharsets.UTF_8);',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", StandardCharsets.UTF_8)", 4]}],
     "wrong": [{"contains": "UTF_8, StandardCharsets"}, {"count_ne": ["Config.load(", 4]}]})

gen("sf06", "c", "src/net.c",
    [unit("", ", MAX_LEN", "    int n = read_frame(sock_fd, buf", ");"),
     unit("", ", MAX_LEN", "    int m = read_frame(sock_fd, tail", ");")],
    'static int drain(int sock_fd) {\n'
    '    int n = read_frame(sock_fd, buf, MAX_LEN);\n'
    '    int m = read_frame(sock_fd, tail, MAX_LEN);\n'
    '    int p = read_frame(sock_fd, spare);\n'
    '    int q = read_frame(sock_fd, scratch);\n'
    '    return n + m + p + q;\n'
    '}\n',
    'tail, MAX_LEN);',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", MAX_LEN)", 4]}],
     "wrong": [{"contains": "MAX_LEN, MAX_LEN"}, {"count_ne": ["read_frame(", 4]}]})

gen("sf07", "ruby", "app/responder.rb",
    [unit("", ", layout: false", "  when :edit then render(:edit", ")"),
     unit("", ", layout: false", "  when :show then render(:show", ")")],
    'def respond(kind)\n'
    '  case kind\n'
    '  when :edit then render(:edit, layout: false)\n'
    '  when :show then render(:show, layout: false)\n'
    '  when :index then render(:index)\n'
    '  when :stale then render(:stale)\n'
    '  end\n'
    'end\n',
    ':show, layout: false)',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", layout: false)", 4]}],
     "wrong": [{"contains": "layout: false, layout: false"},
               {"count_ne": ["render(", 4]}]})

gen("sf08", "kotlin", "app/Pump.kt",
    [unit("", ", scope", "        emit(TickEvent", ")"),
     unit("", ", scope", "        emit(WarmupEvent", ")")],
    'fun pump(scope: CoroutineScope) {\n'
    '    emit(TickEvent, scope)\n'
    '    emit(WarmupEvent, scope)\n'
    '    emit(DrainEvent)\n'
    '    emit(FlushEvent)\n'
    '}\n',
    'WarmupEvent, scope)',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", scope)", 4]}],
     "wrong": [{"contains": "scope, scope"}, {"count_ne": ["emit(", 4]}]})

gen("sf09", "go", "store/hydrate.go",
    [unit("", "ctx, ", "\tusers, err := fetchAll(", "ids)"),
     unit("", "ctx, ", "\tnames, err := fetchAll(", "keys)")],
    'func hydrate(ctx context.Context) error {\n'
    '\tusers, err := fetchAll(ctx, ids)\n'
    '\tif err != nil {\n'
    '\t\treturn err\n'
    '\t}\n'
    '\tnames, err := fetchAll(ctx, keys)\n'
    '\tgroups, err := fetchAll(gids)\n'
    '\thosts, err := fetchAll(addrs)\n'
    '\treturn join(users, names, groups, hosts)\n'
    '}\n',
    'fetchAll(ctx, keys)',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": ["fetchAll(ctx, ", 4]}],
     "wrong": [{"contains": "ctx, ctx"}, {"count_ne": ["fetchAll(", 4]}]})

gen("sf10", "csharp", "src/Flush.cs",
    [unit("", ", cancellationToken", "        await store.Save(order", ");"),
     unit("", ", cancellationToken", "        await store.Save(receipt", ");")],
    'async Task Flush(CancellationToken cancellationToken) {\n'
    '    await store.Save(order, cancellationToken);\n'
    '    await store.Save(receipt, cancellationToken);\n'
    '    await store.Save(invoice);\n'
    '    await store.Save(ledger);\n'
    '}\n',
    'receipt, cancellationToken);',
    {"consult": True, "consult_reason": "fanout_insert", "fire": True,
     "correct": [{"count": [", cancellationToken)", 4]}],
     "wrong": [{"contains": "cancellationToken, cancellationToken"},
               {"count_ne": ["store.Save(", 4]}]})

# ======================================================================
# pi — param_insert positives: same target, per-site-varying replacement.
# Rule lane: silent (replaces differ -> rules differ -> support 1).
# ======================================================================

gen("pi01", "rust", "src/boot.rs",
    [unit("unwrap()", 'expect("read config")', "    let cfg = read_config().", ";"),
     unit("unwrap()", 'expect("bind socket")', "    let sock = bind_socket(&cfg).", ";")],
    'fn boot() -> Server {\n'
    '    let cfg = read_config().expect("read config");\n'
    '    let sock = bind_socket(&cfg).expect("bind socket");\n'
    '    let tls = load_tls(&cfg).unwrap();\n'
    '    let pool = ThreadPool::new(cfg.workers).unwrap();\n'
    '    Server { sock, tls, pool }\n'
    '}\n',
    'expect("bind socket");',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"count": [".unwrap()", 0]},
                 {"recount": [r'\.expect\("[^"]{3,60}"\)', 4]}],
     "wrong": [{"contains": 'expect(".unwrap'}, {"contains": "unwrap().unwrap"}]})

gen("pi02", "javascript", "src/poll.js",
    [unit("reject()", 'reject(new Error("timeout"))', "      ", ";"),
     unit("reject()", 'reject(new Error("bad status"))', "      ", ";")],
    'function poll(url) {\n'
    '  return new Promise((resolve, reject) => {\n'
    '    const t = setTimeout(() => reject(new Error("timeout")), 5000);\n'
    '    http.get(url, (res) => {\n'
    '      if (res.statusCode !== 200) reject(new Error("bad status"));\n'
    '      else resolve(res);\n'
    '    });\n'
    '    socket.on("error", () => reject());\n'
    '    socket.on("close", () => reject());\n'
    '  });\n'
    '}\n',
    '"bad status"));',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"count": ["reject()", 0]},
                 {"recount": [r'reject\(new Error\("[^"]{3,60}"\)\)', 4]}],
     "wrong": [{"contains": "reject(reject"}, {"contains": "Error(new Error"}]})

gen("pi03", "python", "etl/coerce.py",
    [unit("raise", 'raise ValueError("missing key")', "        ", ""),
     unit("raise", 'raise ValueError("bad type")', "        ", "")],
    'def coerce(row):\n'
    '    if "id" not in row:\n'
    '        raise ValueError("missing key")\n'
    '    if not isinstance(row["id"], int):\n'
    '        raise ValueError("bad type")\n'
    '    if row.get("total", 0) < 0:\n'
    '        raise\n'
    '    if len(row) > MAX_COLS:\n'
    '        raise\n'
    '    return Row(**row)\n',
    'ValueError("bad type")',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"recount": [r'raise ValueError\("[^"]{3,60}"\)', 4]},
                 {"count": ["\n        raise\n", 0]}],
     "wrong": [{"contains": "raise raise"}, {"contains": "ValueError(raise"}]})

gen("pi04", "go", "sync/sync.go",
    [unit("return err", 'return fmt.Errorf("dial: %w", err)', "\t\t", ""),
     unit("return err", 'return fmt.Errorf("read: %w", err)', "\t\t", "")],
    'func sync(addr string) error {\n'
    '\tc, err := dial(addr)\n'
    '\tif err != nil {\n'
    '\t\treturn fmt.Errorf("dial: %w", err)\n'
    '\t}\n'
    '\tdata, err := c.read()\n'
    '\tif err != nil {\n'
    '\t\treturn fmt.Errorf("read: %w", err)\n'
    '\t}\n'
    '\tif err := verify(data); err != nil {\n'
    '\t\treturn err\n'
    '\t}\n'
    '\tif err := persist(data); err != nil {\n'
    '\t\treturn err\n'
    '\t}\n'
    '\treturn nil\n'
    '}\n',
    '"read: %w", err)',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"recount": [r'return fmt\.Errorf\("[a-z]+: %w", err\)', 4]},
                 {"count": ["\t\treturn err\n", 0]}],
     "wrong": [{"contains": "%w, %w"}, {"contains": "Errorf(return"}]})

gen("pi05", "typescript", "src/refresh.ts",
    [unit("// TODO", "// TRACKED: JIRA-812", "  ", " retry here"),
     unit("// TODO", "// TRACKED: JIRA-901", "  ", " cache invalidation")],
    'export function refresh(store: Store) {\n'
    '  // TRACKED: JIRA-812 retry here\n'
    '  store.pull();\n'
    '  // TRACKED: JIRA-901 cache invalidation\n'
    '  store.rebuild();\n'
    '  // TODO drop legacy rows\n'
    '  store.compact();\n'
    '  // TODO emit metrics\n'
    '  store.notify();\n'
    '}\n',
    'JIRA-901 cache invalidation',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"count": ["// TODO", 0]},
                 {"recount": [r'// TRACKED: JIRA-\d+', 4]}],
     "wrong": [{"contains": "// TODO TRACKED"}]})

gen("pi06", "java", "src/Repo.java",
    [unit("e.printStackTrace()", 'log.warn("save failed", e)', "            ", ";"),
     unit("e.printStackTrace()", 'log.warn("load failed", e)', "            ", ";")],
    'class Repo {\n'
    '    void saveAll(List<Row> rows) {\n'
    '        try { db.save(rows); } catch (SQLException e) {\n'
    '            log.warn("save failed", e);\n'
    '        }\n'
    '    }\n'
    '    void loadAll() {\n'
    '        try { db.load(); } catch (SQLException e) {\n'
    '            log.warn("load failed", e);\n'
    '        }\n'
    '    }\n'
    '    void purge() {\n'
    '        try { db.purge(); } catch (SQLException e) {\n'
    '            e.printStackTrace();\n'
    '        }\n'
    '    }\n'
    '    void vacuum() {\n'
    '        try { db.vacuum(); } catch (SQLException e) {\n'
    '            e.printStackTrace();\n'
    '        }\n'
    '    }\n'
    '}\n',
    'log.warn("load failed", e);',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"count": ["e.printStackTrace()", 0]},
                 {"recount": [r'log\.warn\("[a-z ]{3,40}", e\)', 4]}],
     "wrong": [{"contains": "printStackTrace(), e"},
               {"contains": "log.warn(e.printStackTrace"}]})

gen("pi07", "ruby", "app/worker.rb",
    [unit("retry", "retry_with(backoff: 2)", "    ", ""),
     unit("retry", "retry_with(backoff: 5)", "    ", "")],
    'def process(batch)\n'
    '  batch.each do |job|\n'
    '    run(job)\n'
    '  rescue Timeout::Error\n'
    '    retry_with(backoff: 2)\n'
    '  rescue Net::OpenTimeout\n'
    '    retry_with(backoff: 5)\n'
    '  rescue IOError\n'
    '    retry\n'
    '  rescue StandardError\n'
    '    retry\n'
    '  end\n'
    'end\n',
    'retry_with(backoff: 5)',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"recount": [r'retry_with\(backoff: \d+\)', 4]},
                 {"count": ["    retry\n", 0]}],
     "wrong": [{"contains": "retry_with(retry"}, {"contains": "backoff: backoff"}]})

gen("pi08", "kotlin", "app/Establish.kt",
    [unit("!!", '?: error("no session")', "    val s = session", ""),
     unit("!!", '?: error("no token")', "    val t = token", "")],
    'fun establish(ctx: Ctx): Session {\n'
    '    val s = session ?: error("no session")\n'
    '    val t = token ?: error("no token")\n'
    '    val u = user!!\n'
    '    val g = grants!!\n'
    '    return Session(s, t, u, g)\n'
    '}\n',
    'error("no token")',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"count": ["!!", 0]},
                 {"recount": [r'\?: error\("[a-z ]{3,40}"\)', 4]}],
     "wrong": [{"contains": "!!?:"}, {"contains": "error(error"}]})

gen("pi09", "c", "src/ingest.c",
    [unit("abort()", 'die("parse error")', "        ", ";"),
     unit("abort()", 'die("io error")', "        ", ";")],
    'static void ingest(FILE *f) {\n'
    '    if (!parse_header(f))\n'
    '        die("parse error");\n'
    '    if (ferror(f))\n'
    '        die("io error");\n'
    '    if (!parse_body(f))\n'
    '        abort();\n'
    '    if (fclose(f) != 0)\n'
    '        abort();\n'
    '}\n',
    'die("io error");',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"count": ["abort()", 0]},
                 {"recount": [r'die\("[a-z ]{3,40}"\)', 4]}],
     "wrong": [{"contains": "die(abort"}, {"contains": "abort(die"}]})

gen("pi10", "csharp", "src/Checkout.cs",
    [unit("throw new Exception()",
          'throw new InvalidOperationException("cart empty")', "            ", ";"),
     unit("throw new Exception()",
          'throw new InvalidOperationException("no buyer")', "            ", ";")],
    'class Checkout {\n'
    '    void Validate(Cart cart) {\n'
    '        if (cart.Items.Count == 0)\n'
    '            throw new InvalidOperationException("cart empty");\n'
    '        if (cart.Buyer == null)\n'
    '            throw new InvalidOperationException("no buyer");\n'
    '        if (cart.Total < 0)\n'
    '            throw new Exception();\n'
    '        if (cart.Currency == null)\n'
    '            throw new Exception();\n'
    '    }\n'
    '}\n',
    '"no buyer");',
    {"consult": True, "consult_reason": "param_insert", "fire": True,
     "correct": [{"count": ["throw new Exception()", 0]},
                 {"recount": [r'throw new InvalidOperationException\("[a-z ]{3,40}"\)', 4]}],
     "wrong": [{"contains": "Exception(Exception"},
               {"contains": "InvalidOperationException()"}]})

# ======================================================================
# fi — field_init positives: identical multi-line insertion across literal
# sites. Rule lane: silent (no_rule — multiline units are uninducible).
# ======================================================================

gen("fi01", "rust", "src/pools.rs",
    [unit("", "\n        retries: 3,", "port: 8080,", "\n    };"),
     unit("", "\n        retries: 3,", "port: 9090,", "\n    };")],
    'fn make_pools() -> (Pool, Pool, Pool) {\n'
    '    let a = Pool {\n'
    '        host: PRIMARY,\n'
    '        port: 8080,\n'
    '        retries: 3,\n'
    '    };\n'
    '    let b = Pool {\n'
    '        host: BACKUP,\n'
    '        port: 9090,\n'
    '        retries: 3,\n'
    '    };\n'
    '    let c = Pool {\n'
    '        host: MIRROR,\n'
    '        port: 7070,\n'
    '    };\n'
    '    (a, b, c)\n'
    '}\n',
    'port: 9090,\n        retries: 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["        retries: 3,", 3]}],
     "wrong": [{"contains": "retries: 3,\n        retries: 3,"}]})

gen("fi02", "typescript", "src/clients.ts",
    [unit("", "\n    retries: 3,", "timeoutMs: 5000,", "\n  });"),
     unit("", "\n    retries: 3,", "timeoutMs: 8000,", "\n  });")],
    'export function clients() {\n'
    '  const a = makeClient({\n'
    '    baseUrl: PRIMARY,\n'
    '    timeoutMs: 5000,\n'
    '    retries: 3,\n'
    '  });\n'
    '  const b = makeClient({\n'
    '    baseUrl: BACKUP,\n'
    '    timeoutMs: 8000,\n'
    '    retries: 3,\n'
    '  });\n'
    '  const c = makeClient({\n'
    '    baseUrl: MIRROR,\n'
    '    timeoutMs: 2000,\n'
    '  });\n'
    '  return [a, b, c];\n'
    '}\n',
    'timeoutMs: 8000,\n    retries: 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["    retries: 3,", 3]}],
     "wrong": [{"contains": "retries: 3,\n    retries: 3,"}]})

gen("fi03", "python", "net/sessions.py",
    [unit("", "\n        retries=3,", "timeout=5,", "\n    )"),
     unit("", "\n        retries=3,", "timeout=8,", "\n    )")],
    'def build_sessions():\n'
    '    a = Session(\n'
    '        host=PRIMARY,\n'
    '        timeout=5,\n'
    '        retries=3,\n'
    '    )\n'
    '    b = Session(\n'
    '        host=BACKUP,\n'
    '        timeout=8,\n'
    '        retries=3,\n'
    '    )\n'
    '    c = Session(\n'
    '        host=MIRROR,\n'
    '        timeout=2,\n'
    '    )\n'
    '    return a, b, c\n',
    'timeout=8,\n        retries=3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["        retries=3,", 3]}],
     "wrong": [{"contains": "retries=3,\n        retries=3,"}]})

gen("fi04", "go", "cfg/targets.go",
    [unit("", "\n\t\tRetries: 3,", "Port: 8080,", "\n\t}"),
     unit("", "\n\t\tRetries: 3,", "Port: 9090,", "\n\t}")],
    'func targets() []Target {\n'
    '\treturn []Target{{\n'
    '\t\tHost: primary,\n'
    '\t\tPort: 8080,\n'
    '\t\tRetries: 3,\n'
    '\t}, {\n'
    '\t\tHost: backup,\n'
    '\t\tPort: 9090,\n'
    '\t\tRetries: 3,\n'
    '\t}, {\n'
    '\t\tHost: mirror,\n'
    '\t\tPort: 7070,\n'
    '\t}}\n'
    '}\n',
    'Port: 9090,\n\t\tRetries: 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["\t\tRetries: 3,", 3]}],
     "wrong": [{"contains": "Retries: 3,\n\t\tRetries: 3,"}]})

gen("fi05", "java", "src/Clients.java",
    [unit("", "\n            .retries(3)", ".timeout(5)", "\n            .build();"),
     unit("", "\n            .retries(3)", ".timeout(8)", "\n            .build();")],
    'class Clients {\n'
    '    HttpClient primary = HttpClient.builder()\n'
    '            .host(PRIMARY)\n'
    '            .timeout(5)\n'
    '            .retries(3)\n'
    '            .build();\n'
    '    HttpClient backup = HttpClient.builder()\n'
    '            .host(BACKUP)\n'
    '            .timeout(8)\n'
    '            .retries(3)\n'
    '            .build();\n'
    '    HttpClient mirror = HttpClient.builder()\n'
    '            .host(MIRROR)\n'
    '            .timeout(2)\n'
    '            .build();\n'
    '}\n',
    '.timeout(8)\n            .retries(3)',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["            .retries(3)\n", 3]}],
     "wrong": [{"contains": ".retries(3)\n            .retries(3)"}]})

gen("fi06", "ruby", "config/backends.rb",
    [unit("", "\n    retries: 3,", "timeout: 5,", "\n  },"),
     unit("", "\n    retries: 3,", "timeout: 8,", "\n  },")],
    'BACKENDS = {\n'
    '  primary: {\n'
    '    host: PRIMARY,\n'
    '    timeout: 5,\n'
    '    retries: 3,\n'
    '  },\n'
    '  backup: {\n'
    '    host: BACKUP,\n'
    '    timeout: 8,\n'
    '    retries: 3,\n'
    '  },\n'
    '  mirror: {\n'
    '    host: MIRROR,\n'
    '    timeout: 2,\n'
    '  },\n'
    '}\n',
    'timeout: 8,\n    retries: 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["    retries: 3,", 3]}],
     "wrong": [{"contains": "retries: 3,\n    retries: 3,"}]})

gen("fi07", "kotlin", "app/Endpoints.kt",
    [unit("", "\n        retries = 3,", "timeoutSec = 5,", "\n    ),"),
     unit("", "\n        retries = 3,", "timeoutSec = 8,", "\n    ),")],
    'val endpoints = listOf(\n'
    '    Endpoint(\n'
    '        host = PRIMARY,\n'
    '        timeoutSec = 5,\n'
    '        retries = 3,\n'
    '    ),\n'
    '    Endpoint(\n'
    '        host = BACKUP,\n'
    '        timeoutSec = 8,\n'
    '        retries = 3,\n'
    '    ),\n'
    '    Endpoint(\n'
    '        host = MIRROR,\n'
    '        timeoutSec = 2,\n'
    '    ),\n'
    ')\n',
    'timeoutSec = 8,\n        retries = 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["        retries = 3,", 3]}],
     "wrong": [{"contains": "retries = 3,\n        retries = 3,"}]})

gen("fi08", "c", "src/upstreams.c",
    [unit("", "\n        .retries = 3,", ".port = 8080,", "\n    },"),
     unit("", "\n        .retries = 3,", ".port = 9090,", "\n    },")],
    'static struct upstream upstreams[] = {\n'
    '    {\n'
    '        .host = PRIMARY,\n'
    '        .port = 8080,\n'
    '        .retries = 3,\n'
    '    },\n'
    '    {\n'
    '        .host = BACKUP,\n'
    '        .port = 9090,\n'
    '        .retries = 3,\n'
    '    },\n'
    '    {\n'
    '        .host = MIRROR,\n'
    '        .port = 7070,\n'
    '    },\n'
    '};\n',
    '.port = 9090,\n        .retries = 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["        .retries = 3,", 3]}],
     "wrong": [{"contains": ".retries = 3,\n        .retries = 3,"}]})

gen("fi09", "csharp", "src/Nodes.cs",
    [unit("", "\n            Retries = 3,", "TimeoutSec = 5,", "\n        },"),
     unit("", "\n            Retries = 3,", "TimeoutSec = 8,", "\n        },")],
    'static readonly Node[] Nodes = {\n'
    '        new Node {\n'
    '            Host = Primary,\n'
    '            TimeoutSec = 5,\n'
    '            Retries = 3,\n'
    '        },\n'
    '        new Node {\n'
    '            Host = Backup,\n'
    '            TimeoutSec = 8,\n'
    '            Retries = 3,\n'
    '        },\n'
    '        new Node {\n'
    '            Host = Mirror,\n'
    '            TimeoutSec = 2,\n'
    '        },\n'
    '};\n',
    'TimeoutSec = 8,\n            Retries = 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ["            Retries = 3,", 3]}],
     "wrong": [{"contains": "Retries = 3,\n            Retries = 3,"}]})

gen("fi10", "json", "config/servers.json",
    [unit("", ',\n    "retries": 3', '"timeout": 5', "\n  },"),
     unit("", ',\n    "retries": 3', '"timeout": 8', "\n  },")],
    '{\n'
    '  "primary": {\n'
    '    "host": "10.0.0.1",\n'
    '    "timeout": 5,\n'
    '    "retries": 3\n'
    '  },\n'
    '  "backup": {\n'
    '    "host": "10.0.0.2",\n'
    '    "timeout": 8,\n'
    '    "retries": 3\n'
    '  },\n'
    '  "mirror": {\n'
    '    "host": "10.0.0.3",\n'
    '    "timeout": 2\n'
    '  }\n'
    '}\n',
    '"timeout": 8,\n    "retries": 3',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": True,
     "correct": [{"count": ['"retries": 3', 3]}],
     "wrong": [{"contains": '"retries": 3,\n    "retries"'}, {"contains": "3,,"}]})

# ======================================================================
# gn — gate negatives: the deterministic consult gate must refuse (or the
# rule lane owns the case outright). 100% bar (GM2), like the rule bank.
# ======================================================================

gen("gn01", "typescript", "src/watch.ts",
    [unit("parseHeader", "readHeader", "  const h = ", "(buf);"),
     unit("5000", "8000", "  const t = setTimeout(cb, ", ");")],
    'export function watch(buf: Buffer, cb: () => void) {\n'
    '  const h = readHeader(buf);\n'
    '  const t = setTimeout(cb, 8000);\n'
    '  const backup = setTimeout(cb, 5000);\n'
    '  return { h, t, backup };\n'
    '}\n',
    'setTimeout(cb, 8000);',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="dissimilar cores: rename then unrelated literal change")

gen("gn02", "rust", "src/single.rs",
    [unit("unwrap()", 'expect("read config")', "    let cfg = read_config().", ";")],
    'fn boot() -> Config {\n'
    '    let cfg = read_config().expect("read config");\n'
    '    let extra = read_extra().unwrap();\n'
    '    merge(cfg, extra)\n'
    '}\n',
    'expect("read config");',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="one edit is never a pattern")

gen("gn03", "python", "etl/empty.py",
    [],
    'def passthrough(rows):\n'
    '    return [r for r in rows if r.ok]\n',
    'rows if r.ok',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="empty history")

gen("gn04", "javascript", "src/boot.js",
    [unit("log", "debug", "console.", '("boot");'),
     unit("log", "debug", "console.", '("ready");')],
    'console.debug("boot");\n'
    'console.debug("ready");\n'
    'console.log("listening");\n'
    'console.log("draining");\n',
    'console.debug("ready");',
    {"consult": False, "not_consulted_reason": "rule_fired", "fire": True,
     "engine": "rule"},
    note="rule lane owns identical-rule repeats; model must never be consulted")

gen("gn05", "typescript", "src/keys.ts",
    [unit("id", "iid", "  const ", " = next();"),
     unit("id", "iid", "  let ", " = 0;")],
    'function keys(row: Row) {\n'
    '  const iid = next();\n'
    '  let iid = 0;\n'
    '  const id = parse(row);\n'
    '  return [iid, id];\n'
    '}\n',
    'let iid = 0;',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="identical short rule below threshold: rule-lane restraint is policy, "
         "not a model opportunity")

gen("gn06", "typescript", "src/exhausted.ts",
    [unit("getUserData", "fetchUserData", "  const raw = ", "(userId);"),
     unit("getUserData", "fetchUserData", "  const avatar = ", "(userId + \"/avatar\");")],
    'async function loadProfile(userId: string) {\n'
    '  const raw = fetchUserData(userId);\n'
    '  const avatar = fetchUserData(userId + "/avatar");\n'
    '  return render(raw, avatar);\n'
    '}\n',
    '"/avatar");',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="rename exhausted and NO casing variants exist — nothing to offer")

gen("gn07", "rust", "src/noop.rs",
    [unit("x", "x", "    let ", " = 1;"),
     unit("unwrap()", 'expect("open db")', "    let db = open_db().", ";")],
    'fn init() -> Db {\n'
    '    let x = 1;\n'
    '    let db = open_db().expect("open db");\n'
    '    let cache = warm_cache(&db).unwrap();\n'
    '    Db { db, cache, x }\n'
    '}\n',
    'expect("open db");',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="a no-op unit contributes nothing; one real edit is not a pattern")

gen("gn08", "typescript", "src/mixed.ts",
    [unit("", "\n    retries: 3,", "port: 8080,", "\n  };"),
     unit("", "\n    backoff: 2.0,", "port: 9090,", "\n  };")],
    'export function make() {\n'
    '  const a = {\n'
    '    host: PRIMARY,\n'
    '    port: 8080,\n'
    '    retries: 3,\n'
    '  };\n'
    '  const b = {\n'
    '    host: BACKUP,\n'
    '    port: 9090,\n'
    '    backoff: 2.0,\n'
    '  };\n'
    '  const c = {\n'
    '    host: MIRROR,\n'
    '    port: 7070,\n'
    '  };\n'
    '  return [a, b, c];\n'
    '}\n',
    'backoff: 2.0,',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="two multiline inserts with different content are not one pattern")

gen("gn09", "javascript", "src/tune.js",
    [unit("0", "3", "  cfg.retries = ", ";"),
     unit("0", "7", "  cfg.attempts = ", ";")],
    'function tune(cfg) {\n'
    '  cfg.retries = 3;\n'
    '  cfg.attempts = 7;\n'
    '  cfg.budget = 0;\n'
    '  cfg.floor = 0;\n'
    '  return cfg;\n'
    '}\n',
    'cfg.attempts = 7;',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="same before, afters share no meaningful prefix — magic numbers "
         "diverging is not a pattern")

gen("gn10", "javascript", "src/report.js",
    [unit("warn(msg)", 'push("warn", msg)', "  log.", ";"),
     unit("error(msg)", 'push("error", msg)', "  log.", ";")],
    'function report(msg) {\n'
    '  log.push("warn", msg);\n'
    '  log.push("error", msg);\n'
    '  log.debug(msg);\n'
    '  log.trace(msg);\n'
    '}\n',
    'log.push("error", msg);',
    {"consult": False, "not_consulted_reason": "gate", "fire": False},
    note="befores differ: converging afters alone do not make a pattern")

# ======================================================================
# mn — model negatives: the gate legitimately consults, but the correct
# output is silence. Any fire is a wrong edit (GM3).
# ======================================================================

gen("mn01", "go", "net/pool_done.go",
    [unit("", ", timeoutMS", "\tconn := dial(primaryHost, 8080", ")"),
     unit("", ", timeoutMS", "\tbackup := dial(backupHost, altPort", ")")],
    'func connectAll(cfg Config) []Conn {\n'
    '\ttimeoutMS := cfg.TimeoutMS\n'
    '\tconn := dial(primaryHost, 8080, timeoutMS)\n'
    '\tbackup := dial(backupHost, altPort, timeoutMS)\n'
    '\tmirror := dial(mirrorHost, 9090, timeoutMS)\n'
    '\tlocal := dial(localHost, cfg.Port, timeoutMS)\n'
    '\treturn []Conn{conn, backup, mirror, local}\n'
    '}\n',
    'altPort, timeoutMS)',
    {"consult": True, "consult_reason": "fanout_insert", "fire": False},
    note="fan-out complete: every call already has the parameter")

gen("mn02", "rust", "src/boot_done.rs",
    [unit("unwrap()", 'expect("read config")', "    let cfg = read_config().", ";"),
     unit("unwrap()", 'expect("bind socket")', "    let sock = bind_socket(&cfg).", ";")],
    'fn boot() -> Server {\n'
    '    let cfg = read_config().expect("read config");\n'
    '    let sock = bind_socket(&cfg).expect("bind socket");\n'
    '    let tls = load_tls(&cfg).expect("load tls");\n'
    '    let pool = ThreadPool::new(cfg.workers).expect("thread pool");\n'
    '    Server { sock, tls, pool }\n'
    '}\n',
    'expect("bind socket");',
    {"consult": True, "consult_reason": "param_insert", "fire": False},
    note="every unwrap already converted")

gen("mn03", "rust", "src/pools_done.rs",
    [unit("", "\n        retries: 3,", "port: 8080,", "\n    };"),
     unit("", "\n        retries: 3,", "port: 9090,", "\n    };")],
    'fn make_pools() -> (Pool, Pool) {\n'
    '    let a = Pool {\n'
    '        host: PRIMARY,\n'
    '        port: 8080,\n'
    '        retries: 3,\n'
    '    };\n'
    '    let b = Pool {\n'
    '        host: BACKUP,\n'
    '        port: 9090,\n'
    '        retries: 3,\n'
    '    };\n'
    '    (a, b)\n'
    '}\n',
    'port: 9090,\n        retries: 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": False},
    note="every literal already carries the field")

gen("mn04", "go", "net/pool_log.go",
    [unit("", ", timeoutMS", "\tconn := dial(primaryHost, 8080", ")"),
     unit("", ", timeoutMS", "\tbackup := dial(backupHost, altPort", ")")],
    'func connectAll(cfg Config) []Conn {\n'
    '\ttimeoutMS := cfg.TimeoutMS\n'
    '\tconn := dial(primaryHost, 8080, timeoutMS)\n'
    '\tbackup := dial(backupHost, altPort, timeoutMS)\n'
    '\tlog.Printf("dial(mirrorHost, 9090) unreachable, skipping")\n'
    '\treturn []Conn{conn, backup}\n'
    '}\n',
    'altPort, timeoutMS)',
    {"consult": True, "consult_reason": "fanout_insert", "fire": False,
     "wrong": [{"contains": '9090, timeoutMS) unreachable'}]},
    note="the only textual echo of the pattern is inside a log string — "
         "editing prose is a wrong edit")

gen("mn05", "typescript", "src/loaders_done.ts",
    [unit("", ", { signal }", "  const res = await fetch(profileUrl", ")"),
     unit("", ", { signal }", "  const meta = await fetch(metaUrl", ")")],
    'async function loadAll(signal: AbortSignal) {\n'
    '  const res = await fetch(profileUrl, { signal });\n'
    '  const meta = await fetch(metaUrl, { signal });\n'
    '  const prefs = await fetch(prefsUrl, { signal });\n'
    '  const flags = await fetch(flagsUrl, { signal });\n'
    '  return Promise.all([res, meta, prefs, flags]);\n'
    '}\n',
    'metaUrl, { signal });',
    {"consult": True, "consult_reason": "fanout_insert", "fire": False},
    note="fan-out complete")

gen("mn06", "python", "etl/coerce_done.py",
    [unit("raise", 'raise ValueError("missing key")', "        ", ""),
     unit("raise", 'raise ValueError("bad type")', "        ", "")],
    'def coerce(row):\n'
    '    if "id" not in row:\n'
    '        raise ValueError("missing key")\n'
    '    if not isinstance(row["id"], int):\n'
    '        raise ValueError("bad type")\n'
    '    if row.get("total", 0) < 0:\n'
    '        raise ValueError("negative total")\n'
    '    return Row(**row)\n',
    'ValueError("bad type")',
    {"consult": True, "consult_reason": "param_insert", "fire": False},
    note="every bare raise already converted")

gen("mn07", "typescript", "src/clients_done.ts",
    [unit("", "\n    retries: 3,", "timeoutMs: 5000,", "\n  });"),
     unit("", "\n    retries: 3,", "timeoutMs: 8000,", "\n  });")],
    'export function clients() {\n'
    '  const a = makeClient({\n'
    '    baseUrl: PRIMARY,\n'
    '    timeoutMs: 5000,\n'
    '    retries: 3,\n'
    '  });\n'
    '  const b = makeClient({\n'
    '    baseUrl: BACKUP,\n'
    '    timeoutMs: 8000,\n'
    '    retries: 3,\n'
    '  });\n'
    '  return [a, b];\n'
    '}\n',
    'timeoutMs: 8000,\n    retries: 3,',
    {"consult": True, "consult_reason": "multiline_fanout", "fire": False},
    note="every literal already carries the field")

gen("mn08", "java", "src/BootDone.java",
    [unit("", ", StandardCharsets.UTF_8", "        Config base = Config.load(basePath", ");"),
     unit("", ", StandardCharsets.UTF_8", "        Config user = Config.load(userPath", ");")],
    'class Boot {\n'
    '    void init() {\n'
    '        Config base = Config.load(basePath, StandardCharsets.UTF_8);\n'
    '        Config user = Config.load(userPath, StandardCharsets.UTF_8);\n'
    '        Config site = Config.load(sitePath, StandardCharsets.UTF_8);\n'
    '        apply(base, user, site);\n'
    '    }\n'
    '}\n',
    'userPath, StandardCharsets.UTF_8);',
    {"consult": True, "consult_reason": "fanout_insert", "fire": False},
    note="fan-out complete")

gen("mn09", "rust", "src/boot_vendored.rs",
    [unit("unwrap()", 'expect("read config")', "    let cfg = read_config().", ";"),
     unit("unwrap()", 'expect("bind socket")', "    let sock = bind_socket(&cfg).", ";")],
    'fn boot() -> Server {\n'
    '    let cfg = read_config().expect("read config");\n'
    '    let sock = bind_socket(&cfg).expect("bind socket");\n'
    '    audit("legacy call sites still use .unwrap() in vendored code");\n'
    '    Server { sock }\n'
    '}\n',
    'expect("bind socket");',
    {"consult": True, "consult_reason": "param_insert", "fire": False,
     "wrong": [{"contains": '.expect("legacy'},
               {"contains": 'use .expect('}]},
    note="the only remaining .unwrap() is inside a string — editing prose "
         "is a wrong edit")

gen("mn10", "csharp", "src/FlushDone.cs",
    [unit("", ", cancellationToken", "        await store.Save(order", ");"),
     unit("", ", cancellationToken", "        await store.Save(receipt", ");")],
    'async Task Flush(CancellationToken cancellationToken) {\n'
    '    await store.Save(order, cancellationToken);\n'
    '    await store.Save(receipt, cancellationToken);\n'
    '    await store.Save(invoice, cancellationToken);\n'
    '    await store.Save(ledger, cancellationToken);\n'
    '}\n',
    'receipt, cancellationToken);',
    {"consult": True, "consult_reason": "fanout_insert", "fire": False},
    note="fan-out complete")


# ---- emit -------------------------------------------------------------

def main() -> None:
    out = HERE / "cases.jsonl"
    counts: dict[str, int] = {}
    for c in CASES:
        counts[c["kind"]] = counts.get(c["kind"], 0) + 1
    langs = sorted({c["language"] for c in CASES})
    with out.open("w") as f:
        for c in CASES:
            f.write(json.dumps(c, ensure_ascii=False) + "\n")
    print(f"wrote {len(CASES)} cases -> {out}")
    print(f"  kinds: {counts}")
    print(f"  languages ({len(langs)}): {', '.join(langs)}")


if __name__ == "__main__":
    main()
