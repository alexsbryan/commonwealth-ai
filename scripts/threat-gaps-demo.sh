#!/usr/bin/env bash
# threat-gaps-demo.sh — the instrument for quality/campaigns/threat-gaps.toml.
#
#   scripts/threat-gaps-demo.sh up | down | verdict <bar|all>
#
# Five bars, one run, one JSON row each as the LAST lines — floors and
# directions READ from the campaign file, never written here. Exit 1 if any
# bar FAILED, 4 if some COULD-NOT-JUDGE, 0 when all five PASSED
# (scripts/lib/demo_verdicts.py, shared with ring-room-demo.sh).
#
# THE NODE DOOR IS NOT FORKED. This sources scripts/ring-room-demo.sh, which
# sources scripts/ring-doc-demo.sh, and reuses their `node_exec` (which logs
# every command), `node_curl`, `sv`, `mkcfg`, `containers_up`, `join_one`,
# `wait_online`, `members_from_mesh`, `stale_binaries`, `need_binaries`,
# `room_start_daemon`, `mesh_json` and `room_cnj_rows`. Two things were owed
# to the room for that and are its only changes: the sourced guard it lacked,
# and the verdict table lifted into scripts/lib/.
#
# WHAT IS NOT REUSED, AND WHY. `room_up` is the ROOM's four machines — its
# guest door, its two apps, its media holder — and every node in it binds
# `internal_bind = "127.0.0.1"` (ring-doc-demo.sh:337). The bar this
# instrument exists for is a STRANGER reaching a node's internal port over
# plain IP, so a loopback-bound listener would answer connection-refused and
# the run would measure the instrument's own posture instead of the product's.
# The bring-up below is therefore its own, over the same door, and it restores
# the SHIPPED default (`0.0.0.0`, setup_config.rs `default_internal_bind`) —
# it never writes `internal_auth`, `client_tokens` or an RPC bind into a node
# config, because every bar's goodhart line requires the shipped default.
#
# THE NODE SET — four containers on ring-doc's one network:
#   a  the founder, a member. The node under test.
#   b  a member. It is the control: whatever refuses the stranger must not
#      refuse a member's gossip (the bar's clause (c) and its goodhart line).
#   c  brought up with the others and joined DURING the walk, not at bring-up:
#      clause (d), a fresh node joining over the same listener.
#   s  the STRANGER — a container on the same network with no daemon, no
#      config, no key and no credential. Its whole vocabulary is curl.
#
# WHAT THIS STAND-IN CANNOT SHOW, said rather than defaulted (ARCH §6):
#   - the iroh half of the stranger bar (clause (b), a non-member key dialing
#     `cwth/http/0`) has no CLI surface on this host — `svrn mesh` has no dial
#     verb — so it is read from the file that already decides who may dial
#     what, sovereign-mesh/tests/main/iroh_dialer_admission_e2e.rs;
#   - clauses (c) and (d) of tg-rpc-port-not-on-lan need two machines and a
#     resident big model and read COULD-NOT-JUDGE here, owed to the queue's
#     HUMAN-tg-rpc-two-machines row;
#   - tg-meshapp-window-bridge-only needs a mesh-app WINDOW and this stand-in
#     has no webview, so its clauses are read from the desktop crate and its
#     `reason` says the live window was not driven.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
# The node set, prefix, founder and data dir go in BEFORE the source: they are
# ring-doc's own knobs for exactly this (a second topology through one door).
export RING_DOC_NODES="a b c s"
export RING_DOC_CPREFIX=threat-gaps
export RING_DOC_FOUNDER=a
export RING_DOC_DIR="${RING_DOC_DIR:-$REPO/target/threat-gaps-demo}"
export RING_DOC_BACKEND="${RING_DOC_BACKEND:-podman}"
export RING_ROOM_TOPOLOGY=three
# shellcheck source=scripts/ring-room-demo.sh
source "$REPO/scripts/ring-room-demo.sh" _sourced

SCRIPT="$REPO/scripts/threat-gaps-demo.sh"
CAMPAIGN="$REPO/quality/campaigns/threat-gaps.toml"
# The doc tg-doc-names-nothing-dead reads. A knob so the instrument can be
# pointed at a COPY and shown to tell two trees apart; the default is the doc.
TG_DOC="${TG_DOC:-$REPO/docs/THREAT_MODEL.md}"
# `room_start_daemon` reads this; the room sets it only under its own topology.
ROOM_RUST_LOG="${ROOM_RUST_LOG:-info,mesh.decision=info,transport=debug}"
# Which legs run. `stranger` is the only one that needs containers; the other
# four read the tree and cost seconds. A leg left out reads COULD-NOT-JUDGE.
TG_LEGS="${TG_LEGS:-stranger}"

# The stranger's door continues the node door's own stride past the fourth
# node the room defines, so no port or address is written here.
CPORT[s]=$(( CPORT[d] + (CPORT[d] - CPORT[c]) ))
IPORT[s]=$(( IPORT[d] + (IPORT[d] - IPORT[c]) ))
DPORT[s]=$(( DPORT[d] + (DPORT[d] - DPORT[c]) ))
IP[s]="${IP[d]%.*}.$(( ${IP[d]##*.} + (${IP[d]##*.} - ${IP[c]##*.}) ))"
PNET[s]=$NET
MESHNAME[s]=Stranger
PERSON[s]=stranger
# No daemon, so nothing to forward to.
NOFWD[s]=1

TG_MEMBERS=(a b)          # joined at bring-up
TG_JOINER=c               # joins during the walk — clause (d)
TG_STRANGER=s

tg_bar_ids() {
  python3 -c "
import sys, tomllib
print(' '.join(b['id'] for b in tomllib.load(open(sys.argv[1],'rb'))['bar']))" "$CAMPAIGN"
}

# ── bring-up: ring-doc's door, the SHIPPED internal bind ────────────────────
# ring-doc pins every node to loopback so its three cannot meet the real house
# on this LAN. These four meet nobody either — they are on a private podman
# bridge with mDNS off and no seed addresses — and the bar under test is about
# the interface the product ships, so the product's default is restored.
tg_mkcfg() { # node
  ring_doc_mkcfg "$1"
  sed -i 's/^internal_bind = "127.0.0.1"$/internal_bind = "0.0.0.0"/' "$D/$1/config.toml"
}

tg_up() {
  need_binaries
  rm -rf "$D"; mkdir -p "$D"
  local n
  # Every node gets a config, the joiner included: it is brought up cold and
  # joins mid-walk, which is the thing clause (d) is about.
  for n in "${TG_MEMBERS[@]}" "$TG_JOINER"; do tg_mkcfg "$n"; done
  mkdir -p "$D/$TG_STRANGER"
  [ "$BACKEND" = podman ] && { containers_up || return 3; }
  for n in "${TG_MEMBERS[@]}" "$TG_JOINER"; do room_start_daemon "$n"; done
  wait_all_up "${TG_MEMBERS[@]}" "$TG_JOINER" || return 3
  wait_homed "${TG_MEMBERS[@]}" "$TG_JOINER" || return 3
  for n in "${TG_MEMBERS[@]:1}"; do join_one "$n" && wait_online "${MESHNAME[$n]}" || return 3; done
  sleep 12
  members_from_mesh "${#TG_MEMBERS[@]}" || return 3
  echo "up: ${MESHNAME[a]} and ${MESHNAME[b]} are the mesh, ${MESHNAME[$TG_JOINER]} is cold, $TG_STRANGER holds nothing"
}

tg_down() {
  local n
  for n in "${TG_MEMBERS[@]}" "$TG_JOINER"; do node_kill "$n" "$D/$n/pid"; done
  sleep 1
  [ "$BACKEND" = podman ] && containers_down
  return 0
}

# ── the walk: one uncredentialed POST, and what it did not break ────────────
tg_quiesced() { # node — the node's own reading of its own state, over its loopback
  node_curl "$1" -s --max-time 5 "$(at "${IPORT[$1]}")/internal/mesh/quiesce" 2>/dev/null \
    | python3 -c "import sys,json; print(json.load(sys.stdin).get('quiesced'))" 2>/dev/null
}

leg_stranger() {
  local a=a code before after bind reach online_before online_after joined=0
  bind=$(sed -n 's/^internal_bind = "\(.*\)"$/\1/p' "$D/$a/config.toml")
  online_before=$(mesh_json "$a" | python3 -c "
import sys,json
try: m=json.load(sys.stdin)['members']
except Exception: print(''); raise SystemExit
print(sum(1 for x in m if x.get('status')=='online'))" 2>/dev/null)
  before=$(tg_quiesced "$a")
  # Can the stranger even reach the port? A connection refused here is the
  # instrument's posture, not the product's, and is reported as such.
  reach=$(node_exec "$TG_STRANGER" curl -s -o /dev/null -w '%{http_code}' --max-time 8 \
    "http://${IP[$a]}:${IPORT[$a]}/status" 2>/dev/null)
  # THE PROBE. No bearer, no mesh header, no key — Demo 1, from the one
  # machine on this network that is not in the mesh.
  code=$(node_exec "$TG_STRANGER" curl -s -o "$D/stranger-quiesce.body" -w '%{http_code}' --max-time 15 \
    -X POST "http://${IP[$a]}:${IPORT[$a]}/internal/mesh/quiesce" \
    -H 'content-type: application/json' -d '{"quiesced":true}' 2>/dev/null)
  after=$(tg_quiesced "$a")
  # Put the node back however it answered, so the rest of the walk measures a
  # node that is not quiesced.
  node_curl "$a" -s --max-time 5 -X POST "$(at "${IPORT[$a]}")/internal/mesh/quiesce" \
    -H 'content-type: application/json' -d '{"quiesced":false}' > /dev/null 2>&1
  # Clause (c): a member's gossip round, across one interval, after the probe.
  sleep 20
  online_after=$(mesh_json "$a" | python3 -c "
import sys,json
try: m=json.load(sys.stdin)['members']
except Exception: print(''); raise SystemExit
print(sum(1 for x in m if x.get('status')=='online'))" 2>/dev/null)
  # Clause (d): a fresh node joins over the same listener.
  join_one "$TG_JOINER" > /dev/null 2>&1 && wait_online "${MESHNAME[$TG_JOINER]}" > /dev/null 2>&1 && joined=1
  python3 -c "
import json, sys
k = ['bind','reach','code','before','after','online_before','online_after','joined']
d = dict(zip(k, sys.argv[1:9]))
d['body'] = open(sys.argv[9]).read()[:400] if len(sys.argv) > 9 else ''
json.dump(d, sys.stdout)" \
    "$bind" "$reach" "$code" "$before" "$after" "$online_before" "$online_after" "$joined" \
    "$D/stranger-quiesce.body" > "$D/stranger.json"
}

# `cargo xtask docs-gate` is clause (a) of tg-doc-names-nothing-dead. It takes
# the cargo lock, so it goes through the one wrapper that serialises on it.
leg_docsgate() {
  # Beside $D, never in it: the bring-up empties $D.
  ( cd "$REPO/corpus-engine" && "$REPO/scripts/with-cargo-lock.sh" cargo xtask docs-gate ) \
    > "$D-docs-gate.log" 2>&1
  echo $? > "$D-docs-gate.rc"
}

# ── the report ──────────────────────────────────────────────────────────────
report() { # bar|all
  python3 - "$D" "$CAMPAIGN" "$1" "$SCRIPT" "$TG_DOC" "$TG_LEGS" "$(tg_bar_ids)" <<'PY'
import json, os, re, sys, tomllib
d, campaign, want, script_p, doc_p, legs, bar_ids = sys.argv[1:8]
sys.path.insert(0, os.path.join(os.path.dirname(script_p), "lib"))
import demo_verdicts

REPO = os.path.dirname(script_p) + "/.."
bars = {b["id"]: b for b in tomllib.load(open(campaign, "rb"))["bar"]}
rows, row = demo_verdicts.new_rows()


def src(rel):
    try:
        return open(os.path.join(REPO, rel), encoding="utf-8", errors="replace").read()
    except OSError:
        return None


def artifact(name):
    try:
        return json.load(open(os.path.join(d, name)))
    except Exception:
        return None


def score(clauses):
    """The floor_basis rule every bar here shares: any clause false is 0.0;
    a clause nobody could measure leaves the bar unmeasured, never a pass."""
    vals = list(clauses.values())
    if any(v is False for v in vals):
        return 0.0
    if any(v is None for v in vals):
        return None
    return 1.0


def said(clauses):
    return " ".join(f"({k})={'true' if v is True else 'false' if v is False else 'unmeasured'}"
                    for k, v in clauses.items())


# ── tg-stranger-refused-9742 ────────────────────────────────────────────────
BAR = "tg-stranger-refused-9742"
st = artifact("stranger.json")
# (b) the iroh half. `sovereign-mesh/tests/main/iroh_dialer_admission_e2e.rs`
# is where "who may dial what" is already decided, one test per ALPN. Clause
# (b) is a test in THAT file that dials the INTERNAL alpn (the bare `ALPN`
# const, not CLIENT_/RPC_/MEDIA_/OFFER_/APP_) and reads a refusal.
adm_rel = "sovereign/crates/sovereign-mesh/tests/main/iroh_dialer_admission_e2e.rs"
adm = src(adm_rel)
alpn_tests, internal_alpn_tests = [], []
if adm is not None:
    for fn in re.finditer(r"async fn (\w+)\(\)\s*\{", adm):
        name, start = fn.group(1), fn.end()
        body = adm[start:start + 1500]
        alpn_tests.append(name)
        if re.search(r"(?<![_A-Z])ALPN\b", body) and re.search(r"is_err|unwrap_err|401|403|refus", body):
            internal_alpn_tests.append(name)
# (e) the exempt set, pinned by a test: exactly `/internal/join` and
# `/internal/gossip` named together in one place the daemon crate tests read.
daemon_src = os.path.join(REPO, "sovereign/crates/sovereign-daemon/src")
exempt_pin = []
for root, _dirs, files in os.walk(daemon_src):
    for f in files:
        if not f.endswith(".rs"):
            continue
        rel = os.path.relpath(os.path.join(root, f), REPO)
        t = src(rel) or ""
        if "/internal/join" in t and "/internal/gossip" in t and re.search(r"EXEMPT|exempt", t):
            exempt_pin.append(rel)
if "stranger" not in legs.split(",") or st is None:
    row(BAR, None, "the stranger leg did not run: no bring-up, nothing measured",
        adm_file=adm_rel, internal_alpn_tests=internal_alpn_tests)
elif st["reach"] in ("000", ""):
    row(BAR, None,
        f"the stranger could not reach {st['bind']}:9742 at all (curl wrote '{st['reach']}') — "
        "that is the instrument's own posture, not a refusal by the product",
        **st)
else:
    clauses = {
        "a": st["code"] == "401" and st["after"] == st["before"],
        "b": bool(internal_alpn_tests),
        "c": (st["online_before"] != "" and st["online_after"] != ""
              and int(st["online_after"]) >= int(st["online_before"]) >= 2),
        "d": st["joined"] == "1",
        "e": bool(exempt_pin),
    }
    reason = (f"uncredentialed POST /internal/mesh/quiesce from a non-member on the same network "
              f"answered {st['code']}; the node's quiesced went {st['before']} -> {st['after']}. "
              f"{said(clauses)}. ")
    if not internal_alpn_tests:
        reason += (f"(b): no test in {adm_rel} dials the internal ALPN and reads a refusal; "
                   f"it names {len(alpn_tests)} tests, all on the client/rpc/media paths. ")
    if not exempt_pin:
        reason += "(e): no file under sovereign-daemon/src names /internal/join and /internal/gossip as an exempt set."
    row(BAR, score(clauses), reason.strip(), clauses=clauses,
        adm_file=adm_rel, exempt_pin=exempt_pin, **st)

# ── tg-rpc-port-not-on-lan ──────────────────────────────────────────────────
BAR = "tg-rpc-port-not-on-lan"
bs = src("sovereign/crates/sovereign-daemon/src/bootstrap.rs") or ""
m = re.search(r'DEFAULT_RPC_BIND[^=]*=\s*"([^"]+)"', bs)
default_bind = m.group(1) if m else None
launch = src("sovereign/crates/sovereign-contracts/src/launch.rs") or ""
me = re.search(r"pub enum RpcServe\s*\{(.*?)\n\}", launch, re.S)
variants = re.findall(r"^\s{4}(\w+)", me.group(1), re.M) if me else []
clauses = {
    "a": None if default_bind is None else default_bind.startswith("127.0.0.1"),
    # A refusal arm is a third state beside Off/On: a non-loopback bind
    # without the acknowledgement knob must be REFUSED with a sentence, and a
    # two-variant enum has nowhere to put that.
    "b": None if not variants else len(variants) > 2,
    "c": None,   # two machines
    "d": None,   # two machines, a resident big model
}
row(BAR, score(clauses),
    f"DEFAULT_RPC_BIND = {default_bind!r} (bootstrap.rs); RpcServe has variants {variants}. "
    f"{said(clauses)}. (c) and (d) need two machines and a resident big model and are owed to "
    "the queue's HUMAN-tg-rpc-two-machines row — they are COULD-NOT-JUDGE on one host, never PASSED.",
    clauses=clauses, default_rpc_bind=default_bind, rpc_serve_variants=variants)

# ── tg-token-revoked-alone ──────────────────────────────────────────────────
BAR = "tg-token-revoked-alone"
node_state = src("sovereign/crates/sovereign-daemon/src/state/node.rs") or ""
one_token = re.search(r"client_token:\s*Option<Arc<str>>", node_state) is not None
token_verb = []
for root, _dirs, files in os.walk(os.path.join(REPO, "sovereign/crates")):
    for f in files:
        if f.endswith(".rs") and "mesh token" in (src(os.path.relpath(os.path.join(root, f), REPO)) or ""):
            token_verb.append(os.path.relpath(os.path.join(root, f), REPO))
clauses = {"a": bool(token_verb), "b": bool(token_verb), "c": bool(token_verb), "d": None}
row(BAR, score(clauses),
    "no `svrn mesh token` verb exists — `mesh token` appears in no .rs under sovereign/crates, and "
    f"the client token is still one `client_token: Option<Arc<str>>` in state/node.rs ({one_token}). "
    "There is nothing to mint, name or revoke, so (a), (b) and (c) are false rather than unmeasured; "
    "(d) cannot be read until a named-token posture exists.",
    clauses=clauses, one_shared_token=one_token, token_verb_sites=token_verb)

# ── tg-meshapp-window-bridge-only ───────────────────────────────────────────
BAR = "tg-meshapp-window-bridge-only"
dk = "sovereign/crates/sovereign-desktop/src-tauri"
shim = src(f"{dk}/src/meshapp_shim.js") or ""
shim_names = sorted(set(re.findall(r"\bmeshapp_[a-z0-9_]+", shim)))
main_rs = src(f"{dk}/src/main.rs") or ""
handler = re.search(r"generate_handler!\s*\[(.*?)\]", main_rs, re.S)
registered = set(re.findall(r"\b(\w+)\b", handler.group(1))) if handler else set()
unregistered = [n for n in shim_names if n not in registered]
# (a)/(c): a window-level allowlist consulted by the ONE invoke closure, and a
# test that ties it to the shim's own names.
allowlist_sites, allowlist_test = [], []
for root, _dirs, files in os.walk(os.path.join(REPO, dk, "src")):
    for f in files:
        if not f.endswith(".rs"):
            continue
        rel = os.path.relpath(os.path.join(root, f), REPO)
        t = src(rel) or ""
        # An allowlist the invoke gate consults: a list of the shim's command
        # names in the daemon-side source, not the three per-command hand
        # guards that exist today.
        if re.search(r"(?i)meshapp[_ ]?(allow|bridge)[_ ]?(list|commands)", t):
            allowlist_sites.append(rel)
        # A test that PARSES the shim and asserts over it — the only shape of
        # clause (c) that cannot drift when a command is added to one side.
        for body in re.split(r"#\[test\]", t)[1:]:
            body = body[:3000]
            if re.search(r"MESHAPP_SHIM|meshapp_shim\.js", body) and "assert" in body:
                allowlist_test.append(rel)
                break
clauses = {"a": bool(allowlist_sites), "b": not unregistered and bool(shim_names),
           "c": bool(allowlist_test), "d": None}
row(BAR, score(clauses),
    f"the shim names {len(shim_names)} meshapp_* commands and generate_handler! registers "
    f"{len(registered)}; unregistered: {unregistered}. No window-level allowlist is consulted by the "
    f"invoke closure (sites: {allowlist_sites}) and no test ties a list to meshapp_shim.js "
    f"(tests: {allowlist_test}). THE LIVE WINDOW WAS NOT DRIVEN: this stand-in has no webview, so "
    "(a)-(d) are read from the desktop crate, and (d) — the main window's commands unchanged — "
    "is a before/after property no single run can read.",
    clauses=clauses, shim_names=shim_names, unregistered=unregistered)

# ── tg-doc-names-nothing-dead ───────────────────────────────────────────────
BAR = "tg-doc-names-nothing-dead"
try:
    doc = open(doc_p, encoding="utf-8").read()
except OSError as e:
    doc = None
    doc_err = str(e)
rc_p = d + "-docs-gate.rc"
gate_rc = int(open(rc_p).read().strip()) if os.path.exists(rc_p) else None
dead = []
gaps_missing = []
if doc is not None:
    # Every backticked token whose FIRST segment is a directory of this repo is
    # a path this doc cites. Anything else in backticks is a route, a port or
    # a symbol and is not ours to check.
    for tok in set(re.findall(r"`([^`\n]+)`", doc)):
        tok = tok.strip().rstrip(",.;:)")
        if "/" not in tok or tok.startswith("/") or " " in tok:
            continue
        first = tok.split("/")[0]
        if not os.path.isdir(os.path.join(REPO, first)):
            continue
        if not os.path.exists(os.path.join(REPO, tok)):
            dead.append(tok)
    dead.sort()
    # Every entry under Known gaps: struck, By design, or carrying BOTH
    # `Closes when:` and `Owner:` (ledger A56, the rule this doc is held to).
    gaps = doc.split("## Known gaps")[1] if "## Known gaps" in doc else ""
    gaps = gaps.split("\n## ")[0]
    entries = re.split(r"\n(?=\d+\.\s)", gaps)[1:]
    for e in entries:
        head = e.split("\n")[0][:80]
        if e.lstrip().startswith("~~") or "~~" in e.split("\n")[0]:
            continue
        if "By design" in e:
            continue
        if "*Closes when:*" in e and "*Owner:*" in e:
            continue
        gaps_missing.append(head)
clauses = {
    "a": None if gate_rc is None else gate_rc == 0,
    "b": None if doc is None else not dead,
    "c": None if doc is None else not gaps_missing,
}
row(BAR, score(clauses),
    (f"reading {os.path.relpath(doc_p, REPO)}: cargo xtask docs-gate exit={gate_rc}; "
     f"{len(dead)} cited path(s) absent from the tree: {dead}; "
     f"{len(gaps_missing)} Known-gaps entry/entries with neither `Closes when:`+`Owner:` nor a "
     f"strike nor By design: {gaps_missing}. {said(clauses)}")
    if doc is not None else f"could not read {doc_p}: {doc_err}",
    clauses=clauses, doc=os.path.relpath(doc_p, REPO), dead_paths=dead,
    gaps_missing=gaps_missing, docs_gate_exit=gate_rc)

order = bar_ids.split()
sys.exit(demo_verdicts.emit(
    rows, bars, order if want == "all" else [want], artifact=d,
    topology=(f"four {os.environ.get('RING_DOC_BACKEND')} nodes on one network, one host: two "
              "members, one cold joiner, and a stranger with no daemon and no key"
              if "stranger" in legs.split(",") else "no nodes: the tree only")))
PY
}

# Sourced by nothing today; guarded anyway, as both scripts below it are.
[[ "${BASH_SOURCE[0]}" == "$0" ]] || return 0
case "${1:-}" in
  up)   tg_up ;;
  down) tg_down; echo "stopped" ;;
  verdict)
    python3 -c "
import sys, tomllib
ids = [b['id'] for b in tomllib.load(open(sys.argv[1],'rb'))['bar']]
sys.exit(0 if sys.argv[2] in ids + ['all'] else 1)" "$CAMPAIGN" "${2:-}" \
      || { echo "verdict: name a bar or all — see quality/campaigns/threat-gaps.toml" >&2; exit 2; }
    # The demo builds what it measures, or it says so and judges nothing.
    stale=$(stale_binaries)
    [ -z "$stale" ] || { room_cnj_rows "$stale" "$2" "$(tg_bar_ids)"; echo "$stale" >&2; exit 3; }
    need_binaries
    : > "$CMDLOG"
    leg_docsgate
    case ",$TG_LEGS," in
      *,stranger,*)
        trap tg_down EXIT
        tg_up > "$D-up.log" 2>&1 || { echo "bring-up failed, see $D-up.log" >&2; exit 3; }
        STAGE=walk
        leg_stranger > "$D-stranger.log" 2>&1
        ;;
    esac
    report "$2"
    ;;
  *) sed -n '2,50p' "$0"; exit 2 ;;
esac
