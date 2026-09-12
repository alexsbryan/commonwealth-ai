# Publish an app to your mesh

A port on your machine can be a service for everyone on your mesh, reached by
mesh key. No port forwarded, no VPN, no reverse proxy, no DNS, no certificate.
Requests arrive carrying the caller's verified identity, so the app never
writes a login page.

You need a mesh first — [join one](./JOIN_A_MESH.md) — and `svrn mesh status`
listing more than yourself before any of this is interesting.

## The short version

```sh
svrn run --as chores -- python app.py
```

Published while that command runs, gone when it stops. Someone else then:

```sh
svrn mesh app                      # who publishes what
svrn mesh app alex chores          # the URL, probed once so you see a status
svrn mesh app fanout chores /      # ask EVERYONE running it, one row each
```

## What the app sees

Three headers, already verified by the time your handler runs:

```
X-Mesh-Member: LittleMac
X-Mesh-Node:   node-44ae7614
X-Mesh-Pubkey: 8f2c…
```

A client cannot forge them. Your daemon strips any `x-mesh-*` header it was
sent before adding the ones it checked in the QUIC handshake, so exactly one
of each reaches your app and it is the one your daemon chose.

The whole app is the part you were going to write anyway:

```python
from flask import Flask, request
import os

app = Flask(__name__)

@app.get("/")
def whose_turn():
    return f"hello {request.headers['X-Mesh-Member']}"

app.run(port=int(os.environ.get("PORT", 5000)))
```

Your app's name is the first path segment on the way in and is stripped before
your app sees it: a peer's `GET /chores/tasks` arrives at your server as
`GET /tasks`. Use relative URLs, which a one-file app does anyway — an app that
emits absolute links (`/static/app.css`) emits them without the prefix.

## The port

With no `--port`, `svrn run` takes a free one and hands it to your command as
`PORT`. An app that ignores `PORT` binds its own instead, so nothing answers on
the one that was published — which is why the runner **waits for the port to
accept a connection before publishing anything**, and says so when it never
does:

```
run: nothing is listening on 127.0.0.1:53277 after 30s, so "chores" is NOT published.
  The runner picked that port and passed it as PORT. An app that ignores PORT
  binds its own instead — pass `--port <the app's port>`.
```

Name `--port 5000` for an app with a fixed port of its own.

## Two tiers, and why the default is the one with a TTL

`svrn run` holds a **claim**: registered when your app answers, renewed while
it lives, released when it exits, and dropped by its TTL if the runner is
killed outright. A published app cannot outlive the process serving it.

That matters more than it sounds. The alternative — a durable entry in a config
file — only ever accumulates. Nobody deletes the line for the thing they ran
once in March, and by June a house-wide fan-out returns eight `connection
refused` rows from apps that stopped existing months ago, reading the whole
time like an authoritative list.

So the durable tier is for a service that is genuinely always up and that
somebody owns keeping true:

```sh
svrn publish jellyfin 8096      # writes [iroh.apps] in your config
svrn daemon restart             # the config tier is read at start
svrn unpublish jellyfin         # stop
```

`svrn publish` with no arguments says what you are publishing, both tiers, with
the time left on each claim. It asks the daemon rather than reading the config,
because a claim is published and is in no file.

`--ttl` sets how long a claim survives without a heartbeat: `30s`, `10m`, `2h`,
`1d`, or bare seconds. Default an hour, maximum a day.

## When the daemon is down

Your command still runs. The runner says plainly that nothing is published and
retries every tick, so starting your app before the daemon — or restarting the
daemon under a running app — heals on its own. Claims live in the daemon's
memory by construction, so a daemon that restarts has forgotten them all; the
runner re-takes its claim within thirty seconds because it is the thing that
still exists.

## Who may reach it

`[iroh] app_allow` in your config, by member name or node-id prefix. Empty —
the default — is every member of your mesh. It is **separate** from
`media_allow`, so publishing a print queue to the group does not open your film
library, and a stranger holding your dial string is refused outright: an app
written in an afternoon authenticates nothing, so there is no safe downgrade.

## Asking everyone at once

```sh
svrn mesh app fanout chores /tasks
```

One request to every member publishing `chores`, through each one's own
bridge, concurrently, one attributed row back each — what their app answered,
or why they were not asked. A member whose laptop is closed comes back as a row
saying so, never as silence. Merging is yours, because only you know what your
items are.
