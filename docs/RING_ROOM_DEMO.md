# The ring-room demo, and why one leg cannot pass on a CPU node

Written 2026-09-19 at the close of ring-room rr-1, for someone who has not seen
the instrument run. Part one is the run of show. Part two is the one red bar
traced to its root in a's daemon log, with the fixes ranked by how much of the
60-second window each one buys. Numbers are from the recorded run under
`target/ring-room-demo/` (verdict rows in commit `85eda3e49`, per-call timings
from `target/ring-room-demo/a/daemon.err` of the bring-up at 04:23Z).

## Part one: the run of show

A ring is a few machines that share things directly with each other, no server
in the middle. The ring-room demo stages the one-sitting version: three
machines in a room, and a person watching four things happen between them.
Someone asks their own machine a question and gets an answer that cites a file
living on a friend's machine, naming the friend. Everyone opens a shared
document and the edits say who typed them, with nobody ever having typed a
roster. One person offers their film library with one command, it appears on
the projector machine's Library rail, and a title plays through the mesh. Then
a fourth person walks in, joins by one link, and within a minute all three of
those things see them. The proposition under test is that the person typed
nothing they should not have to: no address, no port, no URL, no config line.
The demo is an instrument that measures whether that is true.

### Two scripts

`scripts/ring-doc-demo.sh` came first, from the ring-doc campaign. It brings up
three throwaway daemons on one host, as processes or as podman containers named
`ring-doc-a`, `b` and `c`, joins them into one mesh over real iroh transport,
and drives the shared-document page headlessly with a hundred concurrent edits.
`scripts/ring-room-demo.sh` sources it rather than copying it, inherits the
node door (every command on a node goes through one function, which is how the
command log exists), and adds the four legs and the census. Podman is the
rehearsed topology because the film leg needs Jellyfin as a sibling container
in b's network namespace.

From inside the `sovereign-vulkan` toolbox:

```bash
RING_DOC_BACKEND=podman scripts/ring-room-demo.sh up
RING_DOC_BACKEND=podman scripts/ring-room-demo.sh verdict all
scripts/ring-room-demo.sh down
```

Since rr-2 the same two scripts also carry a SECOND topology, chosen with
`RING_ROOM_TOPOLOGY=room`: four nodes named for the machines they stand in for
(`ring-room-beefy`, `halo`, `little`, `phone`) on TWO podman networks — `room`
is the venue's WiFi and is created `--internal`, `uplink` is its internet.
What makes `room` a room is one bit netavark sets with `--internal` —
`net.ipv4.conf.<room bridge>.forwarding = 0` — and not podman's `isolate`
flag, which installs no rule at all on an internal network. Because a bit
nobody reads is a poor guarantee, `up` also installs a drop each way between
the two bridges in the rootless network namespace, and then PROVES the
result on the wire before any leg runs: the keeper on the uplink must not
reach the wall's page at its room address, and the phone must. With that bit
flipped back to 1 and no rules installed, the keeper reaches it (measured
2026-09-20: HTTP 200, 5 of 5 UDP replies), which is what the proof is
watching for. The
wall is the one member in the room and holds the host's render node; the keeper
and the library holder sit behind the uplink; the phone is on the room's WiFi
only, runs a stock node image, and has no daemon and no key. Cutting `uplink`
from the wall is the venue's internet going down with the room still working.
Nothing of the node door was copied to do it: the node set, the container
prefix, the founder, the networks, the per-node image and the forwarder are
parameters of `ring-doc-demo.sh` now, and rr-1's three-node run is unchanged.

```bash
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh verdict all
```

`up` empties the run directory, starts the three nodes, joins them, and names
them at init so what appears on screen is a word. In the recorded run the
names were `ring-doc-a`, `Bo` and `Cy`. Everything the script does from here
is stamped `install` (bring-up, before the person walks in) or `walk` (what the
person would have done themselves). That stamp is what makes the census honest.

### The five legs

The answer leg starts on b. It generates a small folder of made-up prose at run
time (a beekeeping cooperative on Larkspur Lane), ingests it with
`corpus ingest --share`, and never installs it on a. a's own corpus list is
captured as proof of absence, because the bar's goodhart clause says a citation
naming Bo is decoration if a secretly holds a copy. a is then asked five
questions from a bank, each a plain `chat ask`, and the report reads whether
the released evidence names Bo. In the judged run all five did.

The doc leg is ring-doc's own drive: a hundred edits across three nodes, then
the check that every "last edited by" line on b and c equals the mesh name of
the signer, a read of the roster origin (it must say derived from the mesh),
and a grep of the command log for `roster add` (there must be none).

The film leg puts Jellyfin beside b and runs the holder setup. b then types
exactly one verb, `mesh media offer`, with no origin, because the verb finds
the local Jellyfin itself. c polls the offers endpoint until the library appears
with "offered to: everyone here"; b narrows it with `mesh media admit`; c times
the first byte of a title through the mesh tunnel.

The join leg is the conversion. A fourth container d joins by the invite link
that a's `mesh status` prints, and the same three checks re-run against d.

The census is the accountant for the others. Every string the driver types on
a person's behalf goes to a tab-separated log with its stage, node, leg and a
class decided in one place from the string's shape: address, port, URL, config
line, credential, other. The bar counts only the walk stage. Two kinds of
string are excluded by name. A URL is `opened`, not typed, when the driver
shows it came verbatim from a tool's stdout line, and it records that stdout
file as provenance; the three doc page URLs and the invite link are in this
class. A credential that cannot be recognised by shape says so: the one in this
run is Jellyfin's own demo login. Separately, the census greps the script and
every node's config for any member name, port, host or corpus id. That grep
reading 0 of 22 is what stops the script cheating by pre-configuring d.

### The verdict rows, second cold run

| bar | verdict | what the number was |
|---|---|---|
| answer names the machine | PASSED 1.0 | 5/5 cited Bo, corpus absent on a |
| doc name from membership | PASSED 1.0 | p99 1.398 s over 100 acts, 9/9 lines attributed |
| film from the Library rail | PASSED 1.0 | listed 8.39 s, narrowed 10.47 s, first byte 0.33 s |
| plug-in live | FAILED 0.0 | three of four sub-legs true; `c_answer_names` false |
| nothing typed | PASSED 0 | walk count 0, census names 0 of 22 |

Four verdicts exist, not two: passed, failed, could-not-judge, never-ran. A leg
left out of `RING_ROOM_LEGS` reads could-not-judge rather than quietly passing.
The run directory is worth opening afterwards: `room-answer-N.json` holds each
answer with its citations and epistemic ledger, `members.json` maps node keys
to names, and the `-typed.tsv` sibling is the census raw. A full five-leg run
is about 25 minutes on this host, most of it the answer leg; a run without the
answer leg is faster but is not the row that counts.

## Part two: the failure at its root

The red sub-leg asks a, within 60 seconds of the join, a question only d's
corpus can answer, and reads whether the released evidence names d. The
fan-out worked (a reached Bo and d in 37 ms). What did not fit in 60 seconds
was the ask itself. Here is one ask on a, from a's daemon log, every model call
with its prompt size and wall time:

| call | prompt tokens | wall | served by |
|---|---|---|---|
| route classify (`wl-route`) | 1,288 | 38.4 to 38.9 s (four samples) | local, gated `not_offload_eligible` |
| knowledge fan-out to Bo | none | 0.037 s | Bo's corpus, 5 hits |
| synthesis (`wl-synthesize`) | 5,359 | 160.5 s to first token, 181.3 s total, 291 tokens out | local (ask 1); peer Bo 184.5 s (ask 2) |
| grounding judges (`wl-judge`), four calls | 376, 130, 118, and one more | 15.1 + 3.9 + 3.5 + 5.5 s | local |

That is about 250 seconds for one answer, against a 60-second bar. Divide the
prefill columns by the wall and every call lands at the same rate: this node
prefills at about 33 tokens per second and decodes at about 14. So the ask
carries roughly 7,400 tokens of prompt before a single token of the answer
exists, and at 33 tokens per second that is 220 seconds. Prefill is nine tenths
of the wait. The decode of the answer is 21 seconds.

Stated as one sentence: the pipeline fixes the prompt volume of an ask (about
7,400 tokens for a five-passage answer) without ever reading the serving node's
prefill rate, and the one call that could have left the node never reached a
scheduler at all. On a GPU node the same volume is a few seconds and nobody
notices. On a CPU node, which a phone in the room will be, it is four minutes.

(That sentence ended "is gated home by its latency class" until 2026-09-19.
The latency class is the second of two gates and the classify never reached
it — see fix 3 below, where a's own log names the one that fired.)

The A28 decision kept the 60-second window and owed this leg to rr-2's GPU
walk. That was right for the campaign; it is not a fix. The fixes, ranked by
how much of the 60 seconds each buys on this node:

**1. Stop re-prefilling the stable layers every ask (up to about 120 s).** The
5,359-token synthesis prompt is mostly not evidence: five short passages from a
made-up folder are a few hundred tokens, and the rest is the synthesis system
prompt, the thinking and provenance directives, the epistemic contract, the
persona and the memories. The prompt-budget code in
`sovereign-core/src/runtime/handlers/knowledge_query.rs` (the ctx-aware bundle
budget near line 930) reserves 4,096 tokens for exactly this overhead, and the
`prefill_audit` test in `runtime/prompts.rs` exists to print each layer's size.
A prefix cache would pay that once per session. The log says why it does not:

```
capability: no measurement for this slot — the arch declaration decided
  model=Qwen3.5-2B.Q6_K arch=qwen35 reason="could-not-judge" prefix_cache_safe=false
prefix_cache: prefill scope cache_hit_tokens=0 raw_lcp=0 new_prefill_tokens=5359
```

**That paragraph read "nobody has measured whether the prefix cache is safe on
this architecture" until 2026-09-19, and it was wrong — which retires this
fix rather than ranking it.** The architecture HAS been measured, on this
host, and it fails: `sovereign/DEFAULTS_LEDGER.md` records `qwen35` 2B at
floor 19.9, signal 459-644, **ratio 23x** against the probe's `Safe` limit of
4x, and the same sweep found the measurement agreeing with the declared gate
on 12 of 12 local models. The 2B genuinely cannot survive a partial-KV
rollback, so `prefix_cache_safe=false` is the right answer and running the
measurement would only confirm it.

What the log line reports is narrower than it reads. `reason="could-not-judge"`
is the verdict LABEL, not a cause (`embedded/model_slot.rs:2714` logs
`gate.measured`, which is `PartialKvVerdict::label()`), and the cause string
the verdict actually carries is never logged — three different causes are
indistinguishable in the log. The cause here is that a daemon's PRIMARY slot
is constructed `distributable = true` and `model_slot.rs:2180` skips the probe
for it, despite the comment there saying the skip is for distributed children.
Two real items survive, neither of them a prefix cache: log the inner cause
(principle 1), and stop skipping the probe on the primary slot. The 120
seconds this fix was ranked for are not available on this model, and the
stable layer is the only thing left to shrink — which is a quality trade, not
a latency fix.

**2. Size the prompt to the node's rate, not only its window (the rest of the
synthesis prefill).** The bundle budget above is context-aware: it trims
evidence so the prompt fits the slot's window. It is not rate-aware: a window
of 8k tokens on a node that prefills at 33 tokens per second is a four-minute
window. The scheduler's own types already carry the number needed:
`BenchmarkResult.pp_tok_s` in `oicp-types/src/scoring.rs` is the
prompt-processing rate the startup probe measures. This node's log has no
benchmark line at all, so nothing could have sized against it. The change is
one decider (principle 8): a time budget per latency class, held as data
(principle 9), divided by the measured rate, giving the token budget the
assembler hands to the formatter. On this node a Normal-class budget of 30
seconds is about 1,000 tokens, which is the five passages, the question, and a
short system prompt. That is the answer a person on a phone should get: the
same evidence, fewer instructions.

**3. Let the classify leave the node (about 38 s). Two gates, not one — and
this paragraph named the wrong one.** The route classify is a 1,288-token
prompt for a one-letter answer, and it runs before retrieval.

Until 2026-09-19 this read "gated local by `OffloadVerdict::FastLatency` …
because `Workload::Route` is class Fast". A fresh run says otherwise. a's own
decision line carries the answer in a field this document had not read:

```
routing decision (gated) — stayed local before scoring
  oicp_request_id=wl-route-05c1c73f gate=not_offload_eligible
  latency=Fast sharding=LocalOnly
routing outcome … served_by=local_fallback:Qwen3.5-2B.Q6_K total_ms=Some(38159.45)
```

`sharding=LocalOnly`. `offload_verdict` checks privacy FIRST, so it returned
`LocalOnlyPrivacy` and the latency gate was never consulted at all — the two
verdicts share the reported gate name `not_offload_eligible`, which is what let
the misreading stand. The privacy posture is hardcoded: `Workload::request`
(`sovereign-contracts/src/slot_policy.rs`) passes `ShardingPrivacy::LocalOnly`,
and all three of `LlmRouter`'s classify calls used it, while the same turn's
`Judge` and `Synthesize` envelopes thread the session's posture through
`Workload::requirements(posture)`. SLOT_POLICY §2.4 requires the threading; the
router was the one hot-path workload not doing it.

So the fix is both gates, in series, and neither alone moves the classify:

- **Privacy.** The classify threads `SkillRegistry::session_sharding()` — the
  same accessor, the same source, the same posture the turn's synthesis
  already carries, so nothing new crosses the wire and a `local_only` session
  still keeps it home.
- **Latency.** `LatencyClass::Fast`'s rule ("a hop is a net loss") is a
  prediction about hardware, and this node falsifies it by three orders of
  magnitude — 38.2 s local against a 37 ms hop. The gate now asks its own
  premise: a node whose measured `tg_tok_s_ewma` sits below
  `THROUGHPUT_REFERENCE_TG_TOK_S` (the existing "good for interactive use"
  inflection, 20 tok/s) stands the rule down, reported as
  `gate=fast_latency_yielded` rather than hidden inside `eligible`. An
  unmeasured node keeps the standing rule — absence is reported, not
  defaulted — and standing down only lets the scorer LOOK at peers; local
  still ranks and still wins when no peer is better.

**And it was not enough for the bar, which is the part worth writing down.**
With both gates open the classify reaches the scorer — its decision line loses
the `(gated)` prefix and reads `scored=3` — and then the scorer ranks local
anyway, 38.2 s of it, inside a 60-second window. The same run shows the scorer
sending three of five syntheses to the GPU peer and two to the CPU local, with
nothing in the `info` line to say why either way. Opening a gate does not make
a scorer see.

The line now carries `scores` and `decided_by`, and the same fleet run through
the Tier-1 simulator (`scenario::ring_room_gpu_keeper`, every rate measured on
this host) says exactly what it could not before:

```
local          final 1.092 = claim 0.950 × obs 1.000 × load 1.000 × loc 1.150 × cold 1.000 × tput 1.000 (neutral)
keeper-gpu     final 0.698 = claim 0.950 × obs 1.000 × load 1.000 × loc 1.050 × cold 0.700 × tput 1.000 (neutral)
plus-one-cpu   final 0.698 = claim 0.950 × obs 1.000 × load 1.000 × loc 1.050 × cold 0.700 × tput 1.000 (neutral)
-> local chosen; decided_by cold_start_weight(1.000>0.700) vs plus-one-cpu
```

**The GPU node and the CPU node score identically, to three decimals.** A
machine that prefills at 2,200 tokens a second and one that manages 33 are the
same candidate as far as the scheduler is concerned. What keeps work home is
the peer's frozen cold-start weight (0.7, never ramping on the ranked path)
times the local locality bonus (1.15 against 1.05) — and what eventually sends
it away is the origin's OWN queue depth eroding `load_penalty`, 1.000 → 0.952
→ 0.909 → …, until local finally drops below a constant 0.698. That is the
three-two split, and it is why it looks arbitrary: it is arbitrary. Nothing in
the decision is a measurement of the peer.

On that fleet the shipped scheduler runs at **5% of the efficiency** a
perfect-information oracle reaches (p95 2,594 s against the oracle's 37 s).

Note what this fix is NOT. The scorer has no prefill rate to divide by, and
the obvious way to get one — reviving `run_baseline_benchmark` — is a measured
quality regression (canon `dc3c9856`, `SCHEDULER_QUALITY.md` §4.5 / F10: −56%
mean latency, bought with capability, declined upgrades doubled). The decode
EWMA is the speed signal this fleet actually collects, and it is a rate, so it
does not conflate job sizes the way a measured TTFT would.

**4. Judge the pool once, not the claims one at a time (about 28 s, after the
first token).** Four judge calls follow every answer. They run after the
stream, so they do not delay the first token, but the bar reads the released
evidence and the release waits on them. Batching them into one call is the
same lever the batched reranker already pulls.

**5. Give the rehearsal one fast machine.** All three podman nodes are CPU, so
no scheduling decision can pass this bar here; in ask 2 the scorer sent
synthesis to Bo, identical hardware, and it came back three seconds slower. A room has a
laptop with a GPU in it. Passing the host's Vulkan device into one container
makes the podman rehearsal the same shape as the room, and is the only way to
watch fixes 2 and 3 move this bar before the rr-2 walk on real machines.

That ranking is superseded by what fixing it actually took. Fix 1 is retired:
the prefix cache is measured unsafe on this architecture and buys nothing here.
Fix 3 turned out to be two gates rather than one, and is the only one that has
shipped. Fix 2 remains real and unbuilt — sizing the prompt to the node's rate
is a design change and a quality trade, not a latency fix to be taken quietly.

A sixth fix is now named and deliberately NOT taken. **The scorer has no
measurement of a peer at all**, and the one term that decides — the peer's
frozen cold-start weight — was measured in both directions before anything was
changed. Lifting it (`Arm::WarmStart`) is worth 5× on the ring-room fleet
(efficiency 0.05 → 0.25, p95 2,594 s → 606 s) and costs 33% on `mixed-hubs`,
the fleet the freeze exists to protect (efficiency 0.55 → 0.37, mean latency
+51%, declined upgrades 30 → 1). Opposite signs, same one-line change, so it
is not a change anyone may make on this campaign's evidence — and it would not
fix the room anyway: with the floor lifted the scheduler still cannot tell the
GPU from the CPU, it merely offloads to both more often. What this fleet needs
is a signal that does not exist, and the obvious way to mint one is the rate
card canon forbids. That is a scheduler-design question with its own
measurement, not a bar to be flipped.
Fix 5 came first in practice, because without one fast machine in the room no
scheduling change could be watched failing and then passing on the bar that
minted it: `RING_DOC_GPU_NODES` on the podman backend passes the host's render
node into one named container, measured against a control
(`vulkaninfo` vendorID `0x1002` with the device, Mesa's lavapipe `0x10005`
without). Even with both gates open, a room whose every machine is CPU cannot
pass this bar — the scorer can only choose the best machine present, and three
identical slow ones have no best.
