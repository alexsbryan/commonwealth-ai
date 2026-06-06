# Run a model bigger than your machine

Some models won't fit on one machine. You can run them anyway, by pooling a
second machine — or a few — that you or people you trust already own. The model's
layers spread across the machines, and you talk to it as if it were running
locally. Three 64 GB machines can hold a model no one of them could.

This is built in. There's no extra software and no separate server to babysit —
the same `sovereign` daemon you already run takes on a role from a single setting.

## What you'll need

- **Two or more machines**, each running `sovereign`.
- **A network path between them.** On a LAN you already have it; across locations,
  [Tailscale](https://tailscale.com) is the simplest way to give them a private
  address space. The mesh rides on top.
- **The model file on one machine** — the one that will host. The others don't
  need it on disk; they're handed their slice automatically.

## The shape of it

There are two roles:

- The **host** holds the model file, splits it, and serves the answers. This is
  the machine you talk to.
- A **worker** lends its memory and GPU. It doesn't need the model on disk — the
  host seeds it the slice it's responsible for.

A machine can be both. Set nothing, and `sovereign` runs exactly as it always
has, on one box.

## Set it up

**1. Put the machines on one mesh.**

On the host, create an invite:

```bash
sovereign mesh create        # prints a key like cwth-a1b2-c3d4-e5f6
```

On each other machine, join with it:

```bash
sovereign mesh join cwth-a1b2-c3d4-e5f6
```

`sovereign mesh status` now lists them all.

**2. Start the lending machine as a worker.**

```bash
SOVEREIGN_RPC_SERVE=0.0.0.0:50052 sovereign daemon run
```

That's the whole change — the daemon now offers its GPU to the mesh. Leave it
running.

**3. On the host, point at the big model and turn on discovery.**

Set your primary model in `~/.sovereign/config.toml`:

```toml
[models]
primary = "/path/to/the-big-one.gguf"
```

Then start the host with discovery on (restart the daemon if it's already
running):

```bash
SOVEREIGN_RPC_DISCOVER=1 sovereign daemon run
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
sovereign install-service
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
