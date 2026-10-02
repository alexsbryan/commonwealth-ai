# RuggedFox side — terminal node on a SECOND machine, 2026-08-31

Paste this to the session on RuggedFox. MAC has already run everything that does
not need this host. **Bars below are pre-registered: they were written before any
FOX data existed.** Report the OBSERVED value for each, including — especially —
when it disagrees.

## Why this run exists

`scripts/terminal-e2e.sh --encrypt` passes on MAC and proves the terminal logic
co-located. It cannot prove the thing the feature is for. Both daemons are on one
box there, so every forward is on-box and the prompt never crosses a machine
boundary. Two claims are therefore still untested against reality:

- a turn and an embedding genuinely leaving this host for its bound entry node
- `local_only` being refused *because* honouring it is impossible off-box

Note what is NOT the point: `ServingLocus::ForwardsOffBox` is passed to
`OicpProvider::resolved()` **by construction** ("a terminal always passes
ForwardsOffBox" — `oicp-client/src/lib.rs:1587`), so the enum is already right
co-located. What a second machine adds is that the bytes really travel.

## Do not disturb this host's real node

RuggedFox is a live mesh member on `:9741`/`:9742`. Everything below runs under a
**sandboxed HOME** in a scratch dir on different ports. `SetupConfig::default_path()`
resolves through `dirs::home_dir()` (`sovereign-contracts/src/rebrand.rs:143`), so
`HOME=$SB` fully redirects it — this is the same mechanism `terminal-e2e.sh` runs on.

**Do not pass `--reset`.** It wipes config, and if HOME is ever not sandboxed it
takes RuggedFox's real config with it.

## MAC's coordinates, read fresh at handoff

    MAC node id (full 32-hex):  37f17554b6c4ff292af4844ad4dbc43c
    MAC node id (Display form): node-37f17554b6c4ff29
    MAC advertises:             Qwopus3.5-4B-v3-MTP-Q8_0  (loaded)
    MAC embed slot:             qwen-embedding-0.6b
    MAC peer_requests:          []   <- EMPTY, the baseline F6 is measured against

MAC's dial string (UDP ports move on every daemon restart; the 64-hex endpoint id
is stable):

    86627fd55ae64350a9dd2c1509d525f344fdf95fb356a76f4f256c58532c32d9@https://usw1-1.relay.n0.iroh.link./,69.181.167.209:59210,100.104.36.28:59210,192.168.1.3:59210

Robust relay-only form if a dial fails (both nodes are relay-homed):

    86627fd55ae64350a9dd2c1509d525f344fdf95fb356a76f4f256c58532c32d9@https://usw1-1.relay.n0.iroh.link./

**Join link — this is the whole onboarding** (minted 2026-08-31, ~22h TTL):

    sovereign://join/cwth-bf7e-d2dd-8efc?name=Meshsonics&iroh=86627fd55ae64350a9dd2c1509d525f344fdf95fb356a76f4f256c58532c32d9%40https%3A%2F%2Fusw1-1.relay.n0.iroh.link.%2F%2C69.181.167.209%3A59210%2C100.104.36.28%3A59210%2C192.168.1.3%3A59210&exp=1788301576

---

## F0 — parity. Nothing below is readable until this passes.

    cd ~/dev/commonwealth-ai
    git fetch && git log --oneline -1
    git merge-base --is-ancestor 3405fb64f HEAD && echo "HAS 3405fb64f"
    cargo build --workspace --features corpus-engine/treesitter
    sovereign mesh status

REPORT: the commit, whether `3405fb64f` (the engine-factory merge) is an ancestor,
and whether **Alexs-MacBook** reads `online`.

BAR: `3405fb64f` is an ancestor and MAC is online. If MAC is offline, STOP — every
bar below would be could-not-judge, not fail.

## F1 — the onboarding. One pasted join link, nothing else.

This is the product claim: a non-technical teammate completes this unaided.

    export SB=$(mktemp -d /tmp/fox-terminal.XXXXXX)
    mkdir -p "$SB/.svrnmesh"
    HOME="$SB" sovereign setup --terminal 'sovereign://join/cwth-bf7e-d2dd-8efc?name=Meshsonics&iroh=86627fd55ae64350a9dd2c1509d525f344fdf95fb356a76f4f256c58532c32d9%40https%3A%2F%2Fusw1-1.relay.n0.iroh.link.%2F%2C69.181.167.209%3A59210%2C100.104.36.28%3A59210%2C192.168.1.3%3A59210&exp=1788301576'

BAR: **exit 0**, and the output names a model that served one real turn.
`run_terminal_setup` probes the entry node and proves a served turn before it
reports success (`setup_cmd/terminal.rs:163`), so a 0 here is already one
off-box completion.

REPORT: exit code, the model id it names, and the full stdout.

Then move it off the real daemon's ports BEFORE starting it:

    python3 - <<'PY'
    import os,re,pathlib
    p=pathlib.Path(os.environ["SB"])/".svrnmesh"/"config.toml"
    s=p.read_text()
    s=re.sub(r'client_port\s*=\s*\d+','client_port = 9771',s)
    s=re.sub(r'internal_port\s*=\s*\d+','internal_port = 9772',s)
    if 'client_port' not in s: s+="\n[daemon]\nclient_port = 9771\ninternal_port = 9772\nautostart = false\n"
    p.write_text(s); print(s)
    PY

REPORT: the resulting `config.toml` in full. BAR: it has `[node] entry_node = ...`
(a 32-hex IDENTITY) and **no `entry = ` address key**. An address binding here is
a FAIL — it is the exact thing an encrypted mesh cannot route.

## F2 — start it, holding nothing

    HOME="$SB" RUST_LOG=info,transport=debug \
      ./target/debug/sovereign-cli-daemon daemon run > "$SB/terminal.log" 2>&1 &
    # wait for /status to answer on :9771

BAR: `/status` answers within 180s.

## F3 — it advertises NOTHING to peers

    curl -s http://127.0.0.1:9771/oicp/v1/capabilities

BAR: models list is **`[]`**. A node holding nothing must never attract routed
work. Anything else is a FAIL, not a curiosity.

## F4 — but local clients still see the mesh alias

    curl -s http://127.0.0.1:9771/v1/models

BAR: the ids include **`primary`**. This is gossip-fed, so poll up to ~40s before
calling it — empty immediately after join is "not yet", not "never".

## F5 — class and binding are reported

    curl -s http://127.0.0.1:9771/v1/mesh/status

BAR: `node_class: terminal`, and the entry node named is
**`37f17554b6c4ff292af4844ad4dbc43c`** (MAC), not an address.

## F6 — A REAL TURN, and it must leave this machine

**Read this before firing, or you will book a false FAIL.** MAC runs with
`yield_peers_to_foreground: true` and `ceiling: 1` (its
`/internal/contribution/status`, read at handoff). Any local activity on MAC
stamps a ~15s foreground window, and a peer dispatch landing inside it is
refused with **HTTP 503 `yielded_to_local`**. That is a busy peer, NOT a broken
binding.

    503 + `yielded_to_local`  ->  COULD-NOT-JUDGE. Wait 20s and re-fire.
    503 + any other reason    ->  report the reason verbatim, do not classify.
    200                       ->  proceed to the bar below.

Never convert a 503 into a FAIL here. The refusal is structured precisely so a
caller can branch on it instead of guessing.


    curl -s http://127.0.0.1:9771/v1/chat/completions \
      -H 'content-type: application/json' \
      -d '{"model":"primary","messages":[{"role":"user","content":"reply with the single word: ready"}],"max_tokens":16}'

BAR: HTTP 200 with real content. REPORT the whole response including the `model`
field — on the fanout path it carries peer identity (`... @ peer <name>`), which
is itself the off-box evidence.

**MAC will corroborate this one from its own side**, which is the half that makes
it a two-machine result rather than FOX's self-report: MAC's `peer_requests` is
`[]` right now and must gain an entry naming this terminal, and MAC's contribution
ledger must record an `InferenceServed` for its node id.

## F7 — AN EMBEDDING. The path that MUST use the binding.

    curl -s http://127.0.0.1:9771/v1/embeddings \
      -H 'content-type: application/json' \
      -d '{"model":"qwen-embedding-0.6b","input":"terminal binding check"}' \
      | python3 -c "import sys,json;d=json.load(sys.stdin);print('dims',len(d['data'][0]['embedding']))"

BAR: **1024 dims**. This is the load-bearing one. Chat on a joined terminal can
resolve from the holder's advertised manifest via `provider_for_peer` and never
touch `EntryNodeEndpoint`; embeddings have no such path, so only this proves the
binding itself.

## F8 — local_only is REFUSED, and this is the new bar

A present OICP envelope defaults to `LocalOnly` (`oicp-types/src/requirements.rs:198`).
Sent explicitly here so the intent is unambiguous:

    curl -s -i http://127.0.0.1:9771/v1/chat/completions \
      -H 'content-type: application/json' \
      -d '{"model":"primary","messages":[{"role":"user","content":"hello"}],"max_tokens":16,
           "oicp":{"oicp_version":"0.4.0","privacy":{"sharding":"local_only"}}}'

BAR: **REFUSED, not served.** A terminal owns no weights, so honouring `local_only`
is impossible — forwarding would ship the prompt off-box while claiming it stayed.
A 200 with a completion here is the most serious possible failure in this brief:
it means the privacy envelope is decorative.

Then confirm it refused for the RIGHT reason, not by accident:

    grep -o 'gate=[a-z_]*' "$SB/terminal.log" | sort | uniq -c

BAR: `gate=forwarder_cannot_serve_local_only` is present
(`peer_inference.rs:2503`). A refusal under any other gate is a DIFFERENT bug
wearing this one's clothes — report the gate name you actually see.

## F9 — glassbox: which transport carried it?

    grep -E 'resolved entry node' "$SB/terminal.log" | tail -3

BAR: an `endpoint=` on **loopback** (`http://127.0.0.1:<high-port>/v1`). That is
the iroh bridge — the encrypted path. Counter-intuitive and correct: the address
is local, the far end is MAC. A direct `192.168.1.3:9741` here would mean the
plaintext path was used and the encrypted posture was NOT exercised.

## F10 — send these back to MAC

    xxd -p "$SB/.svrnmesh/node_id" | tr -d '\n'; echo
    curl -s http://127.0.0.1:9771/v1/mesh/status | python3 -c "import sys,json;print(json.load(sys.stdin).get('self_reachability',{}).get('dial','(absent)'))"

MAC needs the terminal's node id to match its `peer_requests` and ledger entries
against F6, and the dial string to probe the terminal over iroh directly.

## Teardown

    kill %1 2>/dev/null; rm -rf "$SB"

RuggedFox's real node is untouched throughout — different HOME, different ports.

---

## Reporting

Four verdicts, not two: **pass / fail / could-not-judge / never-ran**. A bar you
did not reach is `never-ran`, not a pass. A bar whose precondition was missing
(MAC offline, build failed) is `could-not-judge`. Report the observed value
beside every bar, and paste `$SB/terminal.log` if any of F6-F9 disagree.
