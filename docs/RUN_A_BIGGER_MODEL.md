# Run a model bigger than your machine

Some models won't fit on one machine. You can run them anyway, by pooling a
second machine — or a few — that you or people you trust already own. The model's
layers spread across the machines, and you talk to it as if it were running
locally. Three 64 GB machines can hold a model no one of them could.

<p align="center"><img src="diagrams/06-bigger-model.svg" alt="A model too big for one box has its layers split across a host and its workers; the host holds the file, splits it, and serves answers, while workers lend memory and GPU. Once loaded, each worker keeps its slice resident and only a few kilobytes of state cross the wire per answer." width="820"></p>

This is built in. There's no extra software and no separate server to babysit —
the same `svrn` daemon you already run takes on a role from a single setting.

## What you'll need

- **Two or more machines**, each running `svrn`.
- **A network path between them.** On a LAN you already have it; across locations,
  [Tailscale](https://tailscale.com) is the simplest way to give them a private
  address space. The mesh rides on top. If discovery doesn't cross the network —
  different sites, or a tailnet — [Running a mesh across networks](../commonwealth/docs/getting-started.md)
  has the relay workflow.
- **The model file on one machine** — the one that will host. The others don't
  need it on disk; they're handed their slice automatically.
- **The same `svrn` version on each machine.** The split has the machines
  talking in a shared low-level format, so a box on a different build can crash the
  host in the middle of an answer. Match versions across the mesh — different
  operating systems are fine, different *versions* aren't.

## The shape of it

There are two roles:

- The **host** holds the model file, splits it, and serves the answers. This is
  the machine you talk to.
- A **worker** lends its memory and GPU. It doesn't need the model on disk — the
  host seeds it the slice it's responsible for.

A machine can be both. Set nothing, and `svrn` runs exactly as it always
has, on one box.

## Set it up

**1. Put the machines on one mesh.**

On the host, create an invite:

```bash
svrn mesh create        # prints a key like cwth-a1b2-c3d4-e5f6
                             # (says "a mesh already exists"? setup founded a
                             #  solo mesh — read its key with `svrn mesh
                             #  status`, or `svrn mesh rotate` for a new one)
```

On each other machine, join with it:

```bash
svrn mesh join cwth-a1b2-c3d4-e5f6
```

`svrn mesh status` now lists them all.

**2. Start the lending machine as a worker.**

```bash
SOVEREIGN_RPC_SERVE=0.0.0.0:50052 svrn daemon run
```

That's the whole change — the daemon now offers its GPU to the mesh. Leave it
running.

**Check the fit before you commit.** You don't have to load the model to find out
whether it lands. From the host, once the workers are up:

```bash
svrn mesh plan /path/to/the-big-one.gguf --from-mesh
```

It reads the live mesh, lays the model's blocks across each machine's memory, and
tells you — per machine — whether that machine's share fits, without loading anything
or touching a GPU. The host itself only checks the *total* pooled memory when it
loads; this checks each machine individually, so it catches the case where the mesh
has room overall but one small box would overflow. If a machine comes up short it
says so and names the fix. To size a cluster you haven't built yet, pass its memory
directly instead: `--devices 64,32,32` (host last).

**3. On the host, point at the big model and turn on discovery.**

Set your primary model in `~/.sovereign/config.toml`:

```toml
[models]
primary = "/path/to/the-big-one.gguf"
```

Then start the host with discovery on (restart the daemon if it's already
running):

```bash
SOVEREIGN_RPC_DISCOVER=1 svrn daemon run
```

The host finds the worker, splits the model across both machines, and seeds the
worker its share. When it's ready, you query the host the way you always do.
That's it.

## What to expect

The first load moves each worker's share of the weights across the network, once.
After that the worker keeps its slice resident; per answer, only a few kilobytes
of intermediate state cross the wire. The cost is paid up front and amortizes —
which is why this earns its keep on a model that *can't* fit one machine, and
isn't worth it for one that already does.

It will be slower than the same model running wholly local — you've added a
network hop between layers. That's the trade: a model you couldn't run at all,
for one that runs at a measured pace. On a LAN or a direct Tailscale link, the
gap is small.

To see the split is real, watch the worker during a query — its memory rises by
roughly its share of the model and holds it for the length of the answer.

## If a machine drops

Workers come and go, and the mesh expects it. If one leaves — a closed laptop, a
dropped link — the host notices, sets it aside, and reloads on the machines still
present. You won't get a hung process or a wrong answer; you get the model running
on what's left, and the worker folded back in once it's been steadily reachable
again. A worker that keeps flapping is benched for a cooldown rather than trusted
straight back in.

For a setup you'd rather not watch, run the daemon as a service:

```bash
svrn install-service
```

A worker crashing in the middle of an answer can take the host process down with
it — an upstream limitation we don't paper over. Under a service the host restarts
in seconds and the cluster re-forms on its own. That's the intended way to run it
unattended.

---

Hand it a model your machine couldn't hold, and it runs.

*Pre-warming caches for metered links, byte-range shards for very large models,
and the tuning knobs live in [RPC_DISTRIBUTED_INFERENCE.md](./RPC_DISTRIBUTED_INFERENCE.md)
when you want them.*
