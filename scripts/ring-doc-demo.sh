#!/usr/bin/env bash
# ring-doc-demo.sh — the rd-1 instrument: three machines, one document.
#
# Three throwaway daemons on ONE host under their own `SOVEREIGN_DATA_DIR`, on
# real iroh, one mesh, the three keys rostered on all three. Each daemon gets
# its own `svrn ring dev` proxy, and a node driver runs the ring-doc PAGE's
# loop three times over (sovereign/apps/ring-doc/adapter.js — the same
# functions app.js calls, with app.js's own debounce and poll constants read
# out of app.js) against the three proxies. Headless because the bars are
# about what the rail and the adapter do, not about the DOM.
#
# ONE session, four phases, so the legs a bar joins are measured together:
#   1. 100 concurrent edits, three nodes, shared and separate paragraphs
#      → append-to-held latency, cursor latency, convergence.
#   2. C's daemon is stopped for 60 s while all three keep typing
#      → reconvergence, the union of what was typed, the gap panels.
#   3. B appends an update carrying A's Yjs clientID → attribution.
#   4. presence only, then a stop/start of B's daemon → the live lane kept
#      nothing.
# Plus two legs that are not the session's: the replication-sender census
# test, and `git diff --stat origin/main -- commonwealth/crates/commonwealth-rail*`.
#
# TOPOLOGY, named because the goodhart asks for it: three daemons on one host.
# One clock, so latencies need no NTP — and they say nothing about the
# Halo-to-Mac path, which is HUMAN-rd-1-three-machines.
#
# "Stop" and "start" here are a kill of the throwaway daemon's pid and a fresh
# `daemon run` over the same data dir — never `svrn daemon stop`, which resolves
# a daemon from config and must not be able to reach the operator's.
#
#   scripts/ring-doc-demo.sh up                  bring the three up, join, roster
#   scripts/ring-doc-demo.sh down                stop everything
#   scripts/ring-doc-demo.sh verdict <bar|all>   census, bring up, run, tear down
#
# `verdict` prints one co-lineage measurement line per bar as its LAST lines
# (`{"bar": …, "value": N, "floor": F, "verdict": …, "artifact": …}`). Floors
# and directions are READ from quality/campaigns/ring-doc.toml, never restated.
# The first run is the pre-registration: its numbers are recorded, not tuned to.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$REPO/scripts/ring-doc-demo.sh"
D="${RING_DOC_DIR:-$REPO/target/ring-doc-demo}"
DAEMON="$REPO/target/debug/sovereign-cli-daemon"
CLI="$REPO/target/debug/sovereign-cli"
APP="$REPO/sovereign/apps/ring-doc"
CAMPAIGN="$REPO/quality/campaigns/ring-doc.toml"
RING=ring-doc
export SOVEREIGN_NO_STALE_WARN=1

# client · internal (rail = client + 2) · the `ring dev` proxy. Clear of the
# daemon's 9741 and of ring-offers-demo's 197xx.
declare -A CPORT=([a]=19841 [b]=19851 [c]=19861)
declare -A IPORT=([a]=19842 [b]=19852 [c]=19862)
declare -A DPORT=([a]=19849 [b]=19859 [c]=19869)
declare -A PERSON=([a]=alex [b]=bo [c]=cy)
declare -A MESHNAME=([a]=Alex [b]=Bo [c]=Cy)

sv() { local n=$1; shift; SOVEREIGN_DATA_DIR="$D/$n" "$CLI" "$@" 2>/dev/null | grep -v '^svrnmesh: bridged'; }

need_binaries() {
  # Exit 3 is co-lineage's "artifact-absent": could-not-judge, not a failed bar.
  [ -x "$DAEMON" ] && [ -x "$CLI" ] && command -v node >/dev/null && return 0
  echo "ring-doc-demo: build first (cargo build --bins --features sovereign-cli/dev-tools) and put node on PATH" >&2
  exit 3
}

mkcfg() { # node
  local n=$1 dir="$D/$1"; mkdir -p "$dir"
  {
    echo 'mcp_servers = []'; echo
    echo '[daemon]'
    echo "client_port = ${CPORT[$n]}"; echo "internal_port = ${IPORT[$n]}"
    echo 'autostart = false'
    echo 'client_bind = "127.0.0.1"'; echo 'internal_bind = "127.0.0.1"'; echo
    echo '[data]'; echo "dir = \"$dir\""; echo
    # Terminal nodes: no weights, ~190 MB RSS each.
    echo '[node]'; echo 'entry = "http://127.0.0.1:9741/v1"'; echo
    echo '[iroh]'; echo 'enabled = true'; echo
    # No mDNS: these three must not meet the real house on this LAN.
    echo '[discovery]'; echo 'mdns = false'; echo 'seed_addrs = []'
  } > "$dir/config.toml"
}

start_daemon() { local n=$1; SOVEREIGN_DATA_DIR="$D/$n" "$DAEMON" daemon run >> "$D/$n/daemon.out" 2>> "$D/$n/daemon.err" & echo $! > "$D/$n/pid"; }

# ONE deadline for all of them: they cold-boot concurrently.
wait_all_up() {
  local deadline=$(( $(date +%s) + 240 )) p missing
  while [ "$(date +%s)" -lt "$deadline" ]; do
    missing=""
    for p in "$@"; do
      curl -s --max-time 2 "http://127.0.0.1:$p/status" >/dev/null 2>&1 || missing="$missing $p"
    done
    [ -z "$missing" ] && return 0
    sleep 3
  done
  echo "daemons on$missing never answered /status inside 240s" >&2
  return 1
}

# Every internal port binds loopback, so peers reach each other ONLY over iroh,
# and a node that joins before both endpoints have a relay home is handed a
# contact with no iroh path — measured 2026-09-17: B joined 3 s after boot,
# gossiped at A's plain addresses forever, and neither side could send the
# other the correction. `/status` answering is not "ready"; `relay_homed` is.
wait_homed() {
  local deadline=$(( $(date +%s) + 120 )) p missing
  while [ "$(date +%s)" -lt "$deadline" ]; do
    missing=""
    for p in "$@"; do
      curl -s --max-time 3 "http://127.0.0.1:$p/v1/mesh/status" 2>/dev/null \
        | python3 -c "import sys,json; sys.exit(0 if (json.load(sys.stdin).get('self_reachability') or {}).get('relay_homed') else 1)" 2>/dev/null \
        || missing="$missing $p"
    done
    [ -z "$missing" ] && return 0
    sleep 2
  done
  echo "daemons on$missing never homed on an iroh relay inside 120s" >&2
  return 1
}

# The founder sees the joiner Online — the condition its rotate guard waits
# for, and the one every later phase needs.
wait_online() { # display-name
  local deadline=$(( $(date +%s) + 120 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    curl -s --max-time 3 "http://127.0.0.1:${CPORT[a]}/v1/mesh/status" 2>/dev/null \
      | python3 -c "import sys,json; sys.exit(0 if any(m['name']=='$1' and m.get('status')=='online' for m in json.load(sys.stdin)['members']) else 1)" 2>/dev/null \
      && return 0
    sleep 3
  done
  echo "join: a never saw $1 online inside 120s" >&2
  return 1
}

join_one() { # node
  local n=$1 key="" i
  # Retried, never --force: the daemon refuses a rotate until the previous
  # joiner is confirmed by a gossip round, because rotating earlier could
  # partition the mesh it is building.
  for i in $(seq 1 30); do
    key=$(sv a mesh rotate | awk '/Join key:/{print $3}')
    [ -n "$key" ] && break
    sleep 3
  done
  [ -z "$key" ] && { echo "join: a would not rotate a key for $n inside 90s" >&2; return 1; }
  curl -s --max-time 90 -X POST "http://127.0.0.1:${CPORT[$n]}/v1/mesh/join" \
    -H 'content-type: application/json' \
    -d "{\"key_or_url\":\"https://sovereign.dev/join/$key?relay=127.0.0.1:${IPORT[a]}\",\"node_name\":\"${MESHNAME[$n]}\"}" \
    > "$D/join-$n.json"
}

# Every key on every node: a node whose roster lacks a signer reads that
# signer's acts as gaps, which is not the property under test.
roster_all() {
  local n m key
  declare -A KEY
  for n in a b c; do
    key=$(sv $n ring roster add "${PERSON[$n]}" --self --ring $RING | awk '/→/{print $3; exit}')
    [ -z "$key" ] && { echo "roster: node $n printed no key" >&2; return 1; }
    KEY[$n]=$key
  done
  for n in a b c; do
    for m in a b c; do
      [ "$n" = "$m" ] && continue
      sv $n ring roster add "${PERSON[$m]}" --key "${KEY[$m]}" --ring $RING > /dev/null || { echo "roster: $n could not add ${PERSON[$m]}" >&2; return 1; }
    done
  done
}

start_proxy() { # node — the page's door to its daemon, holding the grant
  local n=$1 deadline
  [ -f "$D/$n/dev.pid" ] && kill "$(cat "$D/$n/dev.pid")" 2>/dev/null && sleep 1
  SOVEREIGN_DATA_DIR="$D/$n" "$CLI" ring dev $RING --dir "$APP" --port "${DPORT[$n]}" >> "$D/$n/dev.out" 2>&1 & echo $! > "$D/$n/dev.pid"
  deadline=$(( $(date +%s) + 60 ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    curl -s --max-time 2 -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:${DPORT[$n]}/__ring/log" -d '{}' 2>/dev/null | grep -q 200 && return 0
    sleep 1
  done
  echo "ring dev for $n never served /__ring/log" >&2
  return 1
}

stop_node() { # node — the daemon only; its proxy stays up and answers 502, as a page's would
  local n=$1 pid i
  pid=$(cat "$D/$n/pid" 2>/dev/null) || return 0
  kill "$pid" 2>/dev/null
  for i in $(seq 1 40); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
  kill -9 "$pid" 2>/dev/null
  return 0
}

start_node() { # node — daemon over the same data dir, then a fresh proxy (the grant may not survive)
  local n=$1
  start_daemon "$n"
  wait_all_up "${CPORT[$n]}" || return 1
  start_proxy "$n"
}

cmd_up() {
  need_binaries
  rm -rf "$D"; mkdir -p "$D"
  local n
  for n in a b c; do mkcfg $n; done
  for n in a b c; do start_daemon $n; done
  wait_all_up "${CPORT[a]}" "${CPORT[b]}" "${CPORT[c]}" || exit 3
  wait_homed "${CPORT[a]}" "${CPORT[b]}" "${CPORT[c]}" || exit 3
  join_one b && wait_online Bo || exit 3
  join_one c && wait_online Cy || exit 3
  # One more gossip round, so B has heard about C from A.
  sleep 12
  roster_all || exit 3
  for n in a b c; do start_proxy $n || exit 3; done
  echo "up: a(${PERSON[a]}) b(${PERSON[b]}) c(${PERSON[c]}) — proxies on ${DPORT[a]} ${DPORT[b]} ${DPORT[c]}"
}

cmd_down() {
  local n
  for n in a b c; do
    [ -f "$D/$n/dev.pid" ] && kill "$(cat "$D/$n/dev.pid")" 2>/dev/null
    [ -f "$D/$n/pid" ] && kill "$(cat "$D/$n/pid")" 2>/dev/null
  done
  sleep 1
}

# This ring's journals on all three nodes, as `<files> <fingerprint>`. Only
# this namespace: the daemon's own measurements ring writes on its own clock
# and is not what the live lane could have touched.
journals() {
  local files; files=$(find "$D" -path "*/rings/$RING/*" -type f 2>/dev/null | sort)
  echo "$(echo "$files" | grep -c .) $(echo "$files" | xargs -r sha256sum | sha256sum | cut -c1-16)"
}

# ── the session ─────────────────────────────────────────────────────────────

driver_js() {
  cat > "$D/driver.mjs" <<'JS'
// The page's loop, three times, headless. Mirrors sovereign/apps/ring-doc/app.js
// line for line where app.js decides something; adds only the stamps a
// measurement needs (a ledger of which act carried which paragraph, and a
// wall-clock `t` on the cursor state).
import { execFile } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";

const { REPO, D, SCRIPT } = process.env;
const A = await import(`file://${REPO}/sovereign/apps/ring-doc/adapter.js`);
const { Y } = await import(`file://${REPO}/sovereign/apps/ring-doc/vendor/ring-doc-bundle.js`);
const { awarenessProtocol } = await import(`file://${REPO}/sovereign/apps/ring-doc/vendor/ring-doc-bundle.js`);

// app.js's constants, read from app.js: rd-1-tune edits them there, and a run
// that hardcoded them here would measure the page before the tune.
const APP_JS = readFileSync(`${REPO}/sovereign/apps/ring-doc/app.js`, "utf8");
const T = {};
for (const k of ["DEBOUNCE_MS", "POLL_MS", "PRESENCE_THROTTLE_MS", "LIVE_POLL_MS"]) {
  const m = APP_JS.match(new RegExp(`const ${k} = (\\d+);`));
  if (!m) { console.error(`app.js no longer declares ${k}`); process.exit(3); }
  T[k] = Number(m[1]);
}

// The PRE-REGISTRATION: the run's shape, fixed before any number exists.
const EDITS = 100;          // phase 1, split round-robin over three nodes
const SPLIT_S = 60;         // phase 2, C's daemon down
const PANEL_SAMPLE_S = 5;   // gap panels sampled this often during the split
const SEED = 17;

let rng = SEED;
const rand = () => { rng = (rng + 0x6d2b79f5) | 0; let t = Math.imul(rng ^ (rng >>> 15), 1 | rng);
  t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const now = () => Date.now();

// The SDK's fold — the same test double adapter.test.mjs uses, of the shim's
// traversal in sovereign-cli-llm/src/ring_cmd/dev.rs.
const fold = (log, reducer, initial) => {
  let acc = initial;
  for (const op of (log && log.ops) || []) {
    if (op.voided || op.payload == null) continue;
    acc = reducer(acc, op.payload, op);
  }
  return acc;
};

let phase = 0;
const ledger = new Map(); // act id → { writer, paras, t0, phase, forged }

class Page {
  constructor(name, port) {
    Object.assign(this, { name, port });
    this.ydoc = new Y.Doc();
    this.awareness = new awarenessProtocol.Awareness(this.ydoc);
    this.applied = new Set();
    this.attribution = createAttribution();
    this.pending = []; this.pendingParas = []; this.nextPara = null; this.forging = false;
    this.timer = null; this.inflight = 0; this.presenceTimer = null;
    this.roster = {}; this.myActor = null; this.liveGaps = []; this.panel = []; this.err = "";
    this.heldAt = new Map(); this.cursorSeen = new Map(); this.cursorSamples = [];
    this.draining = true; this.presenceOn = true; this.lastLog = null; this.deliveries = [];
    this.ydoc.on("update", (update, origin) => {
      if (origin === A.FROM_RAIL) return;
      this.pending.push(update);
      this.pendingParas.push({ para: this.nextPara, forged: this.forging });
      if (this.timer === null) this.timer = setTimeout(() => this.flush(), T.DEBOUNCE_MS);
    });
    this.awareness.on("update", (_c, origin) => {
      if (origin === A.FROM_LIVE) return;
      if (this.presenceTimer === null) this.presenceTimer = setTimeout(() => this.sendPresence(), T.PRESENCE_THROTTLE_MS);
    });
  }
  url(op) { return `http://127.0.0.1:${this.port}/__ring/${op}`; }
  async call(op, body) { // the shim's `call`
    const r = await fetch(this.url(op), { method: "POST", headers: { "content-type": "application/json" },
      body: JSON.stringify(body || {}), signal: AbortSignal.timeout(20000) });
    const t = await r.text(); let v = null;
    try { v = t ? JSON.parse(t) : null; } catch (_) { v = { error: t }; }
    // The daemon's refusals are `{error: {message, code}}`; the proxy's own are
    // `{error: "<text>"}`. A reason that reads "[object Object]" names neither.
    const why = v && v.error && (v.error.message || v.error);
    if (!r.ok) throw new Error(why ? String(why) : `ring: ${op} failed`);
    return v;
  }
  async flush() {
    this.timer = null;
    const batch = this.pending, paras = this.pendingParas;
    this.pending = []; this.pendingParas = [];
    if (batch.length === 0) return;
    this.inflight += 1;
    const t0 = now();
    try {
      const written = await this.call("append", { op: "record", payload: A.changeAct(Y.mergeUpdates(batch), A.DOC_ID) });
      ledger.set(written.id, { writer: this.name, paras: [...new Set(paras.flatMap((p) => [].concat(p.para)))],
        forged: paras.some((p) => p.forged), t0, phase, update: Y.mergeUpdates(batch) });
      if (written.actor && written.actor !== this.myActor) { this.myActor = written.actor; this.nameSelf(); }
      this.err = "";
    } catch (e) {
      this.pending = batch.concat(this.pending); this.pendingParas = paras.concat(this.pendingParas);
      this.err = `not recorded: ${String(e.message || e)}`;
    } finally { this.inflight -= 1; }
  }
  nameSelf() {
    const name = A.personFor(this.roster, this.myActor);
    if (name === null) return;
    this.awareness.setLocalStateField("user", { name, color: A.colorFor(name) });
  }
  async sendPresence() {
    this.presenceTimer = null;
    if (!this.presenceOn) return;
    try {
      const r = await fetch(this.url("live"), { method: "POST", headers: { "content-type": "text/plain" },
        body: A.presenceEnvelope(A.encodeSelf(this.awareness), A.DOC_ID), signal: AbortSignal.timeout(20000) });
      if (!r.ok) throw new Error(`ring: live send failed (${r.status})`);
      const body = await r.json().catch(() => null);
      if (body && Array.isArray(body.peers)) this.deliveries.push(...body.peers);
      this.lastPeers = body && Array.isArray(body.peers) ? body.peers : [];
      this.liveGaps = [];
    } catch (e) {
      this.lastPeers = { error: String(e.message || e) };
      this.liveGaps = [`presence not delivered: ${String(e.message || e)}`];
    }
  }
  async pollLive() {
    if (!this.draining) return;
    let body;
    try { body = await this.call("live-drain", {}); }
    catch (e) { this.liveGaps = [`presence not read: ${String(e.message || e)}`]; return; }
    const read = A.decodePresence(body.payloads || [], A.DOC_ID);
    A.applyPresence(this.awareness, read.updates);
    const t = now();
    for (const [id, st] of this.awareness.getStates()) {
      if (id === this.awareness.clientID || !st.cursor) continue;
      if ((this.cursorSeen.get(id) || 0) < st.cursor.t) {
        this.cursorSeen.set(id, st.cursor.t);
        this.cursorSamples.push({ phase, s: (t - st.cursor.t) / 1000 });
      }
    }
    this.liveGaps = body.dropped ? [...read.gaps, `${body.dropped} presence update(s) were dropped by the daemon's buffer`] : read.gaps;
  }
  async poll() {
    let log;
    try { log = await this.call("log", {}); } catch (e) { this.err = String(e.message || e); return; }
    this.lastLog = log;
    this.roster = (log.roster || {}).members || {};
    this.nameSelf();
    const read = A.decodeActs(log, fold, A.DOC_ID);
    const fresh = A.applyNew(this.ydoc, read.acts, this.applied);
    const t = now();
    for (const act of fresh) this.heldAt.set(act.id, t);
    this.attribution.absorb(read.acts);
    // The gap panel, exactly as app.js composes it; it keeps its last
    // contents when a poll fails, as the DOM does.
    this.panel = [...(log.gaps || []).map((g) => g.message || g.gap), ...read.gaps, ...this.liveGaps];
  }
  start() {
    this.poll(); this.pollTimer = setInterval(() => this.poll(), T.POLL_MS);
    this.pollLive(); this.liveTimer = setInterval(() => this.pollLive(), T.LIVE_POLL_MS);
  }
  frag() { return this.ydoc.getXmlFragment(A.PROSE_FRAGMENT); }
  type(p, token) { // one word typed at the end of paragraph p, and the caret moves
    const text = this.frag().get(p).get(0);
    this.nextPara = p;
    text.insert(text.length, token);
    this.awareness.setLocalStateField("cursor", { para: p, anchor: text.length, t: now() });
  }
  text() { return this.frag().toArray().map((el) => el.get(0).toString()).join("\n"); }
  sv() { return Buffer.from(Y.encodeStateVector(this.ydoc)).toString("hex"); }
}
const { createAttribution } = A;

const pages = { a: new Page("a", Number(process.env.PA)), b: new Page("b", Number(process.env.PB)), c: new Page("c", Number(process.env.PC)) };
const all = Object.values(pages);
// Async on purpose: a synchronous exec would freeze all three pages' timers
// for the whole of C's restart, and A and B must keep typing through it.
const sh = (...args) => new Promise((res, rej) => execFile("bash", [SCRIPT, ...args], (e, so, se) => {
  if (se) process.stderr.write(se);
  if (e) rej(e); else res(so.trim());
}));

/// Quiet: nothing pending, every act any page wrote held by every page, and
/// three byte-equal state vectors. `null` when the deadline passes first.
async function settle(deadlineS) {
  const t0 = now();
  while (now() - t0 < deadlineS * 1000) {
    const idle = all.every((p) => p.pending.length === 0 && p.timer === null && p.inflight === 0);
    const held = all.every((p) => [...ledger.keys()].every((id) => p.applied.has(id)));
    const svs = all.map((p) => p.sv());
    if (idle && held && svs.every((s) => s === svs[0])) return (now() - t0) / 1000;
    await sleep(250);
  }
  return null;
}

/// Every token exactly once on every page — lost and duplicated both counted.
function unionCheck(tokens) {
  const out = {};
  for (const p of all) {
    const text = p.text();
    let lost = 0, dup = 0;
    for (const tok of tokens) {
      const n = text.split(tok).length - 1;
      if (n === 0) lost += 1; else if (n > 1) dup += 1;
    }
    out[p.name] = { lost, dup };
  }
  return out;
}

const pct = (xs, q) => { if (xs.length === 0) return null; const s = [...xs].sort((x, y) => x - y);
  return s[Math.min(s.length - 1, Math.ceil(q * s.length) - 1)]; };

async function typeLoop(page, count, paras, prefix, gapMs, stopFlag) {
  const toks = [];
  for (let j = 0; count === null ? !stopFlag.stop : j < count; j += 1) {
    // Bracketed so no token is a substring of another (`a000` of `xa000`).
    const tok = `[${prefix}${page.name}${String(j).padStart(3, "0")}]`;
    page.type(paras[j % paras.length], ` ${tok}`);
    toks.push(tok);
    await sleep(gapMs[0] + rand() * (gapMs[1] - gapMs[0]));
  }
  return toks;
}

const out = { topology: "three daemons, one host, real iroh", timing: T, edits: EDITS, split_s: SPLIT_S };
all.forEach((p) => p.start());

// ── seed: A lays out three paragraphs; everyone waits until they hold them.
phase = 0;
pages.a.nextPara = [0, 1, 2];
pages.a.ydoc.transact(() => {
  for (let i = 0; i < 3; i += 1) {
    const para = new Y.XmlElement("paragraph"); const body = new Y.XmlText();
    body.insert(0, `P${i}:`); para.insert(0, [body]); pages.a.frag().push([para]);
  }
});
out.seed_settle_s = await settle(120);
if (out.seed_settle_s === null || all.some((p) => p.frag().length !== 3)) {
  out.fatal = "the seed paragraphs never reached all three nodes";
  writeFileSync(`${D}/session.json`, JSON.stringify(out, null, 2)); process.exit(0);
}

// ── phase 1: 100 concurrent edits. Node i types in paragraphs i and i+1, so
// every paragraph is shared by two typists and each typist has two.
phase = 1;
const per = [0, 1, 2].map((i) => Math.floor(EDITS / 3) + (i < EDITS % 3 ? 1 : 0));
const p1tokens = (await Promise.all(["a", "b", "c"].map((n, i) =>
  typeLoop(pages[n], per[i], [i, (i + 1) % 3], "", [300, 900], {})))).flat();
out.p1_settle_s = await settle(150);
const lat = [];
let missing = 0;
for (const [id, e] of ledger) {
  if (e.phase !== 1) continue;
  for (const p of all) {
    if (p.name === e.writer) continue;
    const h = p.heldAt.get(id);
    if (h === undefined) { missing += 1; lat.push((now() - e.t0) / 1000); } else lat.push((h - e.t0) / 1000);
  }
}
const cur = all.flatMap((p) => p.cursorSamples.filter((x) => x.phase === 1).map((x) => x.s));
const svs1 = all.map((p) => p.sv());
out.nudge = { acts: [...ledger.values()].filter((e) => e.phase === 1).length, samples: lat.length, missing,
  p50: pct(lat, 0.5), p99: pct(lat, 0.99) };
out.cursor = { samples: cur.length, p50: pct(cur, 0.5), p99: pct(cur, 0.99) };
out.converge = { sv_equal: svs1.every((s) => s === svs1[0]), text_equal: all.every((p) => p.text() === pages.a.text()),
  union: unionCheck(p1tokens), tokens: p1tokens.length };

// ── phase 2: the split. All three type throughout, C included (its flushes
// fail and are re-queued, as the page does).
phase = 2;
const stop2 = { stop: false };
const loops2 = ["a", "b", "c"].map((n, i) => typeLoop(pages[n], null, [i, (i + 1) % 3, 0], "x", [800, 1600], stop2));
await sleep(5000);
await sh("_stop", "c");
const splitAt = now();
const panels = { a: [], b: [], c: [] };
// What a's and b's last live POST said about C, per sample: delivered, its
// error, absent from `peers` (mesh still Online is the only way C is offered),
// or the POST itself failed.
const peerC = { a: [], b: [] };
const saidAboutC = (p) => {
  const lp = p.lastPeers;
  if (lp === undefined) return "no-post";
  if (!Array.isArray(lp)) return `post-failed: ${lp.error}`;
  const e = lp.find((d) => d.name === "Cy");
  return e === undefined ? "absent" : e.delivered ? "delivered" : `error: ${e.error}`;
};
while (now() - splitAt < SPLIT_S * 1000) {
  await sleep(PANEL_SAMPLE_S * 1000);
  for (const p of all) panels[p.name].push(p.panel.slice());
  for (const n of ["a", "b"]) peerC[n].push(saidAboutC(pages[n]));
}
await sh("_start", "c");
out.split_actual_s = (now() - splitAt) / 1000;
await sleep(10000);
stop2.stop = true;
const p2tokens = (await Promise.all(loops2)).flat();
out.p2_settle_s = await settle(240);
const svs2 = all.map((p) => p.sv());
out.partition = { sv_equal: svs2.every((s) => s === svs2[0]), text_equal: all.every((p) => p.text() === pages.a.text()),
  union: unionCheck([...p1tokens, ...p2tokens]), tokens: p1tokens.length + p2tokens.length,
  panel_nonempty: Object.fromEntries(Object.entries(panels).map(([n, xs]) => [n, xs.filter((g) => g.length > 0).length])),
  panel_samples: panels.a.length, peer_c: peerC,
  panel_examples: Object.fromEntries(Object.entries(panels).map(([n, xs]) => [n, (xs.find((g) => g.length) || []).slice(0, 2)])) };

// ── phase 3: B's page announces A's Yjs clientID and appends that update.
phase = 3;
const aClient = pages.a.ydoc.clientID;
const forger = new Y.Doc();
Y.applyUpdate(forger, Y.encodeStateAsUpdate(pages.b.ydoc));
forger.clientID = aClient;
const before = Y.encodeStateVector(forger);
forger.getXmlFragment(A.PROSE_FRAGMENT).get(0).get(0).insert(0, "FORGEDbo ");
const forgedUpdate = Y.encodeStateAsUpdate(forger, before);
pages.b.nextPara = 0; pages.b.forging = true;
Y.applyUpdate(pages.b.ydoc, forgedUpdate);
pages.b.forging = false;
out.p3_settle_s = await settle(120);
for (const p of all) await p.poll();
const forged = [...ledger].find(([, e]) => e.forged);
const forgedClients = forged ? [...new Set(Y.decodeUpdate(forged[1].update).structs.map((s) => s.id.client))] : [];
let lines = 0, right = 0;
const wrong = [];
for (const p of all) {
  // The oracle: the driver's own ledger of which act carried which paragraph,
  // walked in the rail's order — nothing read out of Yjs's observation.
  const expect = {};
  for (const act of A.decodeActs(p.lastLog, fold, A.DOC_ID).acts) {
    for (const para of (ledger.get(act.id) || { paras: [] }).paras) if (para !== null) expect[para] = act.actor;
  }
  const shown = p.attribution.lines(p.roster, now());
  shown.forEach((line, i) => {
    lines += 1;
    const name = line && (line.match(/^last edited by (\S+) \d+s ago$/) || [])[1];
    const want = A.personFor(p.roster, expect[i]);
    if (name && want && name === want) right += 1; else wrong.push({ node: p.name, para: i, line, want });
  });
}
out.attribution = { forged_act: forged ? forged[0] : null, forged_clients: forgedClients, a_client: aClient,
  forged_held_everywhere: !!forged && all.every((p) => p.applied.has(forged[0])),
  lines, right, wrong };

// ── phase 4: presence only, then B restarts. Nobody types.
phase = 4;
const j0 = await sh("_journals");
pages.b.draining = false;
pages.a.deliveries = []; pages.c.deliveries = [];
for (let k = 0; k < 10; k += 1) {
  for (const n of ["a", "c"]) pages[n].awareness.setLocalStateField("cursor", { para: 0, anchor: k, t: now() });
  await sleep(300);
}
await sleep(500);
const toB = [...pages.a.deliveries, ...pages.c.deliveries].filter((d) => d.name === "Bo" && d.delivered).length;
pages.a.presenceOn = false; pages.c.presenceOn = false;
await sh("_stop", "b");
await sh("_start", "b");
let afterRestart = null;
try { afterRestart = await pages.b.call("live-drain", {}); } catch (e) { afterRestart = { error: String(e.message || e) }; }
const j1 = await sh("_journals");
out.live = { cursor_p99: out.cursor.p99, delivered_to_b_before_stop: toB, drain_after_restart: afterRestart,
  journals_before: j0, journals_after: j1 };

writeFileSync(`${D}/session.json`, JSON.stringify(out, null, 2));
process.exit(0);
JS
}

run_session() {
  driver_js
  REPO="$REPO" D="$D" SCRIPT="$SCRIPT" PA="${DPORT[a]}" PB="${DPORT[b]}" PC="${DPORT[c]}" \
    node "$D/driver.mjs" > "$D/driver.out" 2> "$D/driver.err"
}

# The census leg, run rather than remembered: the ONE sender of replicated
# state is still ring-sync with the live lane in place, and the test is the
# one on origin/main.
#
# Beside $D, not in it: bring-up empties $D so every run starts cold.
CENSUS="$D-census"
run_census() {
  mkdir -p "$CENSUS"
  "$REPO/scripts/with-cargo-lock.sh" "$REPO/scripts/sovereign-test.sh" --human --package sovereign-mesh \
    --filter every_sender_of_replicated_state_is_declared > "$CENSUS/census.log" 2>&1
  echo $? > "$CENSUS/census.rc"
  git -C "$REPO" diff --quiet origin/main -- sovereign/crates/sovereign-mesh/tests/main/replication_sender_census.rs
  echo $? > "$CENSUS/census.diff"
  git -C "$REPO" diff --stat origin/main -- 'commonwealth/crates/commonwealth-rail*' > "$CENSUS/rail.diff"
  git -C "$REPO" log --format='%h %s' origin/main..HEAD -- 'commonwealth/crates/commonwealth-rail*' > "$CENSUS/rail.commits"
}

report() { # bar|all
  python3 - "$D" "$CAMPAIGN" "$1" "$CENSUS" <<'PY'
import json, os, re, sys, tomllib
d, campaign, want, cen = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
bars = {b["id"]: b for b in tomllib.load(open(campaign, "rb"))["bar"]}
art = os.path.join(d, "session.json")
s = json.load(open(art)) if os.path.exists(art) else {"fatal": f"the driver wrote no session.json (see {d}/driver.err)"}
census_rc = int(open(os.path.join(cen, "census.rc")).read().strip() or 1)
census_same = open(os.path.join(cen, "census.diff")).read().strip() == "0"
rail_diff = open(os.path.join(cen, "rail.diff")).read().strip()

def clean(u):  # every token exactly once on every page
    return bool(u) and all(v["lost"] == 0 and v["dup"] == 0 for v in u.values())

rows = {}
def row(bar, value, reason="", **extra):
    rows[bar] = dict(bar=bar, value=value, reason=reason, **extra)

fatal = s.get("fatal")
n = s.get("nudge") or {}
row("ra-doc-append-nudges-sync", None if fatal or not n.get("samples") else n["p99"],
    fatal or ("" if n.get("samples") else "no latency samples"),
    p50=n.get("p50"), p99=n.get("p99"), samples=n.get("samples"), missing=n.get("missing"), acts=n.get("acts"))

rail_commits = [l.split(" ", 1) for l in open(os.path.join(cen, "rail.commits")).read().splitlines() if l.strip()]
# A rail diff made only by commits outside this campaign says nothing about
# this campaign: the instrument could not judge, it did not fail.
ours = [h for h, *subj in rail_commits if (subj or [""])[0].startswith(("rd-1-", "REVIEW-", "ralph", "ring-doc"))]
foreign = bool(rail_diff) and bool(rail_commits) and not ours
c = s.get("converge") or {}
row("ra-doc-three-machines-converge", None if fatal or not c or foreign else
    1.0 if (c["sv_equal"] and c["text_equal"] and clean(c["union"]) and not rail_diff) else 0.0,
    fatal or (f"rail diff vs origin/main from commits outside this campaign: {' '.join(h for h, *_ in rail_commits)}"
              if foreign else ""),
    sv_equal=c.get("sv_equal"), text_equal=c.get("text_equal"), union=c.get("union"),
    rail_diff=rail_diff.splitlines(), rail_commits=[" ".join(x) for x in rail_commits])

lv = s.get("live") or {}
basis = bars["ra-doc-live-lane-non-durable"]["floor_basis"]
m = re.search(r"cursor render p99 <= ([\d.]+) s", basis)
drain = lv.get("drain_after_restart") or {}
if fatal or not lv or m is None or census_rc != 0 or not (s.get("cursor") or {}).get("samples") or not lv.get("delivered_to_b_before_stop") or "error" in drain:
    why = fatal or ("floor_basis names no cursor ceiling" if m is None else
          f"census could not be judged (rc={census_rc}, see {cen}/census.log)" if census_rc != 0 else
          "no cursor samples" if not (s.get("cursor") or {}).get("samples") else
          "no presence reached B before its stop, so the restart proves nothing" if not lv.get("delivered_to_b_before_stop") else
          f"drain after restart failed: {drain.get('error')}")
    row("ra-doc-live-lane-non-durable", None, why, p50=(s.get("cursor") or {}).get("p50"), p99=lv.get("cursor_p99"))
else:
    legs = {"a_cursor_p99": lv["cursor_p99"] <= float(m.group(1)),
            "b_census_unchanged_and_green": census_same,
            "c_restart_empty": not drain.get("payloads") and not drain.get("dropped"),
            # `<files> <fingerprint>`; zero files would be an equality over nothing.
            "c_journals_unchanged": lv["journals_before"] == lv["journals_after"]
                                    and int(lv["journals_before"].split()[0]) > 0}
    row("ra-doc-live-lane-non-durable", 1.0 if all(legs.values()) else 0.0, "",
        p50=s["cursor"]["p50"], p99=lv["cursor_p99"], legs=legs, delivered_to_b=lv["delivered_to_b_before_stop"])

p = s.get("partition") or {}
if fatal or not p:
    row("ra-doc-partition-drill", None, fatal or "the drill did not run")
else:
    panels_ok = all(v > 0 for v in p["panel_nonempty"].values())
    row("ra-doc-partition-drill", 1.0 if (p["sv_equal"] and p["text_equal"] and clean(p["union"]) and panels_ok) else 0.0, "",
        sv_equal=p["sv_equal"], union=p["union"], panel_nonempty=p["panel_nonempty"], panel_samples=p["panel_samples"],
        settle_s=s.get("p2_settle_s"))

at = s.get("attribution") or {}
if fatal or not at:
    row("ra-doc-attribution-from-signer", None, fatal or "attribution did not run")
else:
    # The forged update is the failing input; a run without it is a constant.
    forged_ok = at["forged_held_everywhere"] and at["forged_clients"] == [at["a_client"]]
    row("ra-doc-attribution-from-signer", (at["right"] / at["lines"]) if (forged_ok and at["lines"]) else 0.0,
        "" if forged_ok else "the forged-clientID update is not in the run", lines=at["lines"], wrong=at["wrong"][:3])

order = ["ra-doc-append-nudges-sync", "ra-doc-three-machines-converge", "ra-doc-live-lane-non-durable",
         "ra-doc-partition-drill", "ra-doc-attribution-from-signer"]
pick = order if want == "all" else [want]
ok = True
for bar in pick:
    r = rows[bar]; b = bars[bar]; v = r["value"]
    if v is None:
        verdict = "COULD-NOT-JUDGE"
    elif b["direction"] == "lower_is_better":
        verdict = "PASSED" if v <= b["floor"] else "FAILED"
    else:
        verdict = "PASSED" if v >= b["floor"] else "FAILED"
    ok = ok and verdict == "PASSED"
    r.update(floor=b["floor"], verdict=verdict, topology=s.get("topology"), artifact=art)
    print(json.dumps(r))
# `all` is the demo: exit 0 only when every row PASSED. One bar is co-lineage's
# instrument: a value is exit 0 whatever it reads (co-lineage judges it), no
# value is exit 4, a could-not-judge.
if want == "all":
    sys.exit(0 if ok else 1)
sys.exit(4 if rows[want]["value"] is None else 0)
PY
}

case "${1:-}" in
  up)   cmd_up ;;
  down) cmd_down; echo "stopped" ;;
  _stop)  stop_node "$2" ;;
  _start) start_node "$2" > /dev/null ;;
  _journals) journals ;;
  verdict)
    case "${2:-}" in
      all|ra-doc-append-nudges-sync|ra-doc-three-machines-converge|ra-doc-live-lane-non-durable|ra-doc-partition-drill|ra-doc-attribution-from-signer) ;;
      *) echo "verdict: name a bar or all — see quality/campaigns/ring-doc.toml" >&2; exit 2 ;;
    esac
    need_binaries
    run_census
    trap cmd_down EXIT
    cmd_up > "$D-up.log" 2>&1 || { echo "bring-up failed, see $D-up.log" >&2; exit 3; }
    run_session
    report "$2"
    ;;
  *) sed -n '2,37p' "$0"; exit 2 ;;
esac
