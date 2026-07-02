# Commonwealth AI

[![CI](https://github.com/alexsbryan/commonwealth-ai/actions/workflows/ci.yml/badge.svg)](https://github.com/alexsbryan/commonwealth-ai/actions/workflows/ci.yml)
[![docs reconciled](https://github.com/alexsbryan/commonwealth-ai/actions/workflows/docs-reconcile.yml/badge.svg)](https://github.com/alexsbryan/commonwealth-ai/actions/workflows/docs-reconcile.yml)

An AI assistant that runs on your own computer — and, when one machine isn't enough, across a few you trust. The model that answers you lives on your machine, not in someone's cloud. Nothing leaves your device unless you ask it to.

It comes in two parts:

- **svrnmesh** is the assistant you run. Ask it to write, to search what you already know, or to think a problem through — the model answering you lives on the machine in front of you.
- **Commonwealth** is the optional mesh. Pool a few machines you trust and you can run a model none of them could hold alone, or share a knowledge base across the group. There's no central server, and nothing leaves the group.

## What you get

Your conversations, documents, and memory stay on your machine. Answers come grounded in sources you choose to keep — Wikipedia, the Stanford Encyclopedia of Philosophy, Stack Exchange, scholarly abstracts, your own files — searched before each reply and cited, so you can trace a claim back to where it came from. It remembers what mattered from earlier instead of starting cold every time. Web search is there if you want it, off by default and labelled plainly when it runs. There's no telemetry; nothing was built to phone home.

## Get started

```sh
curl -fsSL https://svrnme.sh/install.sh | sh
svrn setup          # finds models that fit your hardware, downloads them, starts the daemon
svrn chat session   # start talking
```

That's the whole loop. There's a desktop app too, and the daemon serves an OpenAI-compatible API so you can point your own tools at it — both in [the svrnmesh guide](./sovereign/README.md).

## Run a model bigger than your machine

Some models won't fit on one computer. You can run them anyway by pooling a second — or a few — that you or people you trust already own. The model's layers spread across the machines, and you talk to it as if it were running locally. Three 64 GB machines can hold a model no one of them could.

```sh
svrn mesh create        # on the host — prints a key like cwth-a1b2-c3d4-e5f6
svrn mesh join <key>    # on each machine you're pooling in
```

[Run a model bigger than your machine](./docs/RUN_A_BIGGER_MODEL.md) walks through the whole thing.
(Already ran `svrn setup`? Your machine quietly founded a solo mesh — read its key with `svrn mesh status` instead of `create`.)

Pooling **knowledge** instead of compute works the same way: the
[two-node quickstart](./docs/TWO_NODE_QUICKSTART.md) gets one machine a
cited answer from a corpus that never leaves the other.

## Build a pipeline

Have an idea shaped like a pipeline — read a folder of things, run a model over each, call a tool in between, save the results? Write it as a small TOML file and run it, no code. Mix in any tool you've connected, and swap a single line to repurpose the whole thing into something else.

```sh
svrn workflow run my-pipeline.toml
```

[Write a workflow](./docs/WRITE_A_WORKFLOW.md) shows you how.

## Going deeper

The [svrnmesh guide](./sovereign/README.md) covers the desktop app, knowledge bases, code intelligence, and troubleshooting. If you came to read or build the code, [SYSTEM_OVERVIEW.md](./sovereign/SYSTEM_OVERVIEW.md) maps every subsystem and [ARCH_PRINCIPLES.md](./sovereign/ARCH_PRINCIPLES.md) lays out the design rules behind it.

## Contributing

Commonwealth AI is opening up to contributions. If you'd like to help — a bug report, a fix, a doc correction — start with [CONTRIBUTING.md](./CONTRIBUTING.md). Security or privacy issues go through [SECURITY.md](./SECURITY.md), not the public issue tracker.

---

*Free software under [AGPL-3.0-or-later](./LICENSE).*
