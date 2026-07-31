# Run GLM-5.2 on the mesh

GLM-5.2 is a 744-billion-parameter open-weight model — one of the strongest you
can run yourself, and far too big for any single machine most of us own (it's over
400 GB). But here's the trick that makes it runnable: only about 40 billion of
those parameters do any work on a given word. The rest sit in memory waiting their
turn. So you don't need one impossible machine — you need enough *memory* spread
across a few ordinary ones to hold the model all at once, while the actual work per
word stays light. Pool four 128 GB machines with people you trust and, together,
you can run a frontier model none of you could run alone.

This is the [Run a model bigger than your machine](./RUN_A_BIGGER_MODEL.md) path,
made concrete for GLM-5.2: one host, a few workers lending memory, the model split
across them, and you talk to the host as if it were all running on your own desk.
Hand this page to the people you want to pool with.

## What the group needs to bring

The whole model has to live in memory across the machines at once, with a little
room to spare for working space. Budget around **480–500 GB of memory pooled
across everyone.** A few ways that adds up:

- **Four 128 GB machines** — the clean target. Strix Halo boxes, 128 GB Macs, or
  a mix. A fifth machine gives you breathing room for longer conversations.
- **Five ~100 GB machines** — same place, smaller boxes.
- **One very large machine** (say, a 512 GB Mac Studio) plus one more — the fewest
  network hops, which is the fastest this gets.

One machine is the **host** — the one you talk to. It holds the model file on disk
(about **440 GB**, downloaded once) and is usually your largest box. Everyone else
just lends memory; they don't need the file at all — the host hands each of them
only their slice automatically.

And a network path between the machines. Splitting one model across several boxes
is the one case that needs them on a **shared IP network**: the layers talk over
raw TCP between the GPUs, so a LAN or VPC is ideal and a wired local network is
noticeably snappier. On a home or office network you already have this. Across
locations, put the GPU boxes on a shared overlay — [Tailscale](https://tailscale.com)
gives everyone a shared private address and the split rides on top. (Ordinary mesh
use — sharing one host's model, knowledge search, gossip — needs no VPN at all;
see [getting-started](../commonwealth/docs/getting-started.md). It's specifically
the cross-box tensor split here that wants shared IP locality.)

And **the same version on every machine.** The split has the GPUs talking in a
shared low-level format, so a box running a different build can crash the host in
the middle of an answer. Everyone installs the same release for their operating
system — matched versions, no exceptions. (Different OSes are fine; different
*versions* are not.)

## Set it up

**1. Get the model onto the host.**

```bash
huggingface-cli download unsloth/GLM-5.2-GGUF \
  --include "UD-Q4_K_S/*" \
  --local-dir ~/.sovereign/models/GLM-5.2
```

**2. Point the host at it.** The download is a set of numbered files; point at the
first one and the rest come along automatically. In `~/.sovereign/config.toml`:

```toml
[models]
primary = "/home/you/.sovereign/models/GLM-5.2/UD-Q4_K_S/GLM-5.2-UD-Q4_K_S-00001-of-00010.gguf"
```

**3. Put everyone on one mesh.** [Join a mesh](./JOIN_A_MESH.md) is the
walk: read the host's join key with `svrn mesh status`, `svrn mesh join
<key>` on every other machine, until `svrn mesh status` lists them all.

**4. Start each lending machine as a worker.**

```bash
SOVEREIGN_RPC_SERVE=0.0.0.0:50052 svrn daemon run
```

**5. Start the host.**

```bash
SOVEREIGN_RPC_DISCOVER=1 \
SOVEREIGN_RPC_SHARD_FETCH=ranges \
  svrn daemon run
```

The host finds the workers, splits the model across everyone's memory by how much
each one has, and hands each worker its slice. `SOVEREIGN_RPC_SHARD_FETCH=ranges`
keeps that tidy at this size — each machine takes only its own slice (around 110 GB
across four) instead of the whole file. When it's ready, you query the host the way
you always do.

**On a metered connection?** Hand a worker the file on a drive and prime it
offline, so nothing crosses their internet link when the cluster starts:

```bash
svrn mesh warm-cache <model-file>
```

## What to expect

**The first start is the slow part, once.** Each worker pulls its slice across the
network the first time (or reads it from a drive you primed). After that it stays
put — every answer afterward sends only a trickle between machines. The big cost is
paid once, up front, which is exactly why this is worth it for a model that can't
fit one machine and wouldn't be worth it for one that can.

**It runs at a steady, readable pace** — an answer that arrives smoothly rather
than instantly. The model is split into a relay across the machines, so each word
makes a lap of the group; the network between them, more than any one machine's
speed, sets the tempo. A wired local network is the single biggest thing you can do
to make it quicker. You haven't really run it until you've tried it — so pool the
machines and see.

**Keep the context window modest to start.** There's an enormous number on the
spec sheet, but very long conversations eat memory and slow the relay down. You
don't need anywhere near the maximum to do something genuinely impressive.

**Want to see it's real?** Watch a worker while you ask a question — its memory
rises to hold its share of the model and stays there for the length of the answer.

## If a machine drops

Machines come and go, and the mesh expects it. If one leaves — a closed laptop, a
dropped connection — the host notices, sets it aside, and brings the model back up
on whatever's still there; you get an answer from the machines that remain, not a
hang or a wrong reply. The one that left rejoins on its own once it's been steady
again for a while.

One thing worth saying plainly to the people you recruit: if a machine crashes in
the *middle* of an answer, it can briefly take the host down with it. The host
comes right back and the group re-forms on its own — but only if it's running as a
service, so set that up on the host: `svrn install-service`
([keeping it running](./START_THE_DAEMON.md#keep-it-running)).

That's the difference between a machine rebooting being a non-event and being a
phone call.

---

Four machines, one mesh, and a model you were never supposed to be able to run
answers from your own hardware.

*The plain, any-model version of this is
[Run a model bigger than your machine](./RUN_A_BIGGER_MODEL.md). The deeper
knobs — priming caches, tuning the split — live in
[RPC_DISTRIBUTED_INFERENCE.md](./RPC_DISTRIBUTED_INFERENCE.md).*
