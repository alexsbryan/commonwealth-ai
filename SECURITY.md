# Security

Commonwealth AI is built on a simple promise: your data stays on your machine
unless you ask it to leave. A security issue here is anything that breaks that
promise, or that puts a user's machine, keys, or data at risk. Reports are
taken seriously — including from people new to the project.

## Reporting a vulnerability

**Please don't open a public issue for security problems.** Report privately,
either way:

- **GitHub (preferred)** — open a draft advisory from the repository's
  [Security tab](https://github.com/alexsbryan/commonwealth-ai/security/advisories/new)
  (Security → Report a vulnerability). It opens a private thread with the
  maintainer, and nothing is public until a fix is ready.
- **Email** — svrnmesh@proton.me.

Please include enough to reproduce: what you did, what happened, the version
(`svrn --version`, or `git rev-parse --short HEAD` for a source build), and
your platform. A proof of concept helps, but a clear description is enough to
start.

## What to expect

This is a small project, so responses come from a person, not a queue:

- An acknowledgement, usually within a few days.
- An honest assessment of whether it's a vulnerability and how serious it is.
- Updates as a fix comes together, and credit in the release notes when it
  ships — unless you'd rather stay anonymous.

Please give a reasonable window to ship a fix before disclosing publicly. We'll
work with you on timing.

## What we care about most

Everything matters, but a few areas map directly onto the project's promises and
get extra attention:

- **Data leaving the machine.** Any path where conversations, documents, memory,
  or telemetry leave a device without the user asking — the sharpest kind of bug
  this project can have.
- **The mesh.** Peer authentication, key handling, and the gossip layer in
  cmnwlth. A peer should never reach data or compute it wasn't granted.
- **Secrets.** How API tokens and keys are stored and resolved (the file-backed
  secret store), and anything that could expose them.
- **The local API and daemon.** The OpenAI-compatible server and the daemon's
  local surface — anything that lets an unintended caller drive inference or
  read state.

The full surface-by-surface posture — what listens where, what is
authenticated, what is encrypted, and the honest list of known gaps — lives in
[docs/THREAT_MODEL.md](./docs/THREAT_MODEL.md). Reading it first will tell you
whether something is a vulnerability or a documented boundary.

## Supported versions

Fixes land on `main` and go out in the next release. Please test against the
latest published release (or `main`) before reporting — the issue may already be
fixed.

Because this is self-hosted software, "supported" is about the code, not a
hosted service: there's no server we operate on your behalf. Running the latest
release is the best way to stay current.
