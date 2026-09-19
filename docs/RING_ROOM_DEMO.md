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
prefill rate, and the one call that could have left the node is gated home by
its latency class. On a GPU node the same volume is a few seconds and nobody
notices. On a CPU node, which a phone in the room will be, it is four minutes.

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

Nobody has measured whether the prefix cache is safe on this architecture, so
the declaration decides against it and every synthesis starts from token zero.
Two moves follow, in order. Run the slot measurement so the verdict is measured
rather than could-not-judge (principle 5: a gate that has never been watched is
not a gate). If the hybrid attention genuinely cannot reuse a prefix, the
stable layer is the thing to shrink, and the audit test already prints where
the tokens are.

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

**3. Let the classify leave the node, or skip it (about 39 s).** The route
classify is a 1,288-token prompt for a one-letter answer, and it runs before
retrieval. It is gated local by `OffloadVerdict::FastLatency` in
`sovereign-mesh/src/oicp_select.rs`, because `Workload::Route` in
`sovereign-contracts/src/slot_policy.rs` is class Fast, and Fast is taken to
mean "do not pay a network hop". On a CPU node the hop is milliseconds and the
local call is 39 seconds, so the rule encodes an assumption about hardware
that this node falsifies. Two fixes, either sufficient. The gate compares
predicted time rather than class: the scorer already has a predicted-time
objective and the `rtt_ms` and throughput inputs to feed it. Or the order
changes: the fan-out took 37 milliseconds and returned five hits from a hosted
corpus, which is a routing decision on its own; run it first and let the
heuristic floor decide when the pool is non-empty, calling the classifier only
when retrieval came back empty.

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

Fixes 1 and 2 are the same change seen from two sides, measure the node and
size to the measurement, and together they take a CPU node's ask from about
250 seconds to something near 60 before the classify is touched. Fix 3 is a
policy that should have been data. Fix 5 is the instrument, and it comes
first, because without it the other four cannot be watched failing and then
passing on the bar that minted them.
