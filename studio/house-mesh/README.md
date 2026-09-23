# house-mesh

Your laptop stays your laptop. The people you trust can reach the things you
choose to share, by name, with their verified identity already attached.

No central cabinet for everyone's files. No port forwarding. No VPN. No login
page. No deploy step.

This is a small example of a larger idea:

- **The hardware mesh is the workshop's hallways.** Routers and wireless links
  let machines in different rooms reach one another.
- **Commonwealth is the trusted messenger and shared notebook.** It checks who
  is asking, carries requests, and keeps the group's record consistent.
- **Your app is your workbench.** It owns its data and decides what that data
  means. A chore list, media server, printer queue, or experiment can use the
  same messenger.

The word **rail** means the dependable shared software channel underneath the
apps. It is not another physical network. The point is to make the shared
workshop a normal place to run an app, not a special remote-inference feature.

Four ways in, depending on what you came for:

- **Run something a housemate published** — the five-minute setup below, then
  `svrn mesh app <person> chores`.
- **Publish your own** — `chores.py` in this repository is the whole app; run it
  with `svrn run --as chores -- python3 chores.py` and edit the task list.
- **Merge everyone's collections** — `library.py` asks each media server the same
  question; `python3 library.py list`.
- **Go under the pattern** — the three headers in the first demo are the seam:
  any HTTP server on localhost plus those headers is a house app.

## See the idea in five minutes

Install the same prebuilt `svrn` command on each machine. Nothing is built from
source.

```sh
curl -fsSL https://svrnme.sh/install.sh | sh
```

**One machine founds the mesh.** If the house will also share inference, make it
the machine with the GPU; otherwise any always-on box will do.

```sh
svrn setup
svrn mesh create
svrn mesh rotate                    # read the join key out to the room
```

**Everyone else joins.** If the house shares inference, the first block above is
the GPU box; everyone else downloads nothing and routes inference to it:

```sh
svrn setup --terminal http://<the-gpu-box>:9741
svrn mesh join '<the join key>'
```

Already joined? Skip to the first demo. The app and media demos keep working
when the GPU box is down. The GPU is one workbench, not the house's central
cabinet.

## First demo: an app with no login page

Start this repository's tiny example on one laptop:

```sh
svrn run --as chores -- python3 chores.py
```

The command runs the app, gives it a free localhost port, and publishes it only
while it is running. No config file is changed and no daemon restart is needed.
Ctrl-C stops the app and unpublishes it; nothing is left behind.

On another member's machine:

```sh
svrn mesh app <publisher> chores
```

This prints a URL, probes it through the mesh, and is ready to paste into a
browser. The app is still running on the publisher's laptop.

Your own machine cannot ask itself through the mesh: run that command against
your own name and it answers `'…' is this node — its published app is already
local`. `svrn publish` is the local view — the app and its remaining lifetime —
and the other machine's command is the end-to-end proof.

The app sees the caller as ordinary HTTP headers:

```text
X-Mesh-Member: Alex
X-Mesh-Node:   node-44ae7614
X-Mesh-Pubkey: 8f2c…
```

The caller did not provide these values. The daemon proved them during the
connection handshake, removes any client-supplied copies, and adds the values
it verified. That is why `chores.py` can say hello by name without a users table
or OAuth flow.

The app still serves its own paths. A request for `chores /tasks` arrives at
the app as `/tasks`, not `/chores/tasks`; use relative URLs for CSS and other
assets.

## The useful interface: one or everyone

There are two questions, expressed by one app interface:

```sh
# Reach one person's workbench.
svrn mesh app <person> chores

# Ask every member that publishes apps the same question.
svrn mesh app fanout chores /
```

The second command returns one attributed row per member it asked. A row says
what the app answered, or why it could not answer:

```text
/chores/ → 2 member(s) asked
  Alex              node-1f2e…    41 ms  200 1204B  application/json
  Dave              node-44ae…     0 ms  failed — connection refused
```

App names are not gossiped, so a member that publishes something else answers
`404` here — a row, not a failure. And a row that says a machine could not
answer is not an error to hide: laptops close. A house app should render
partial answers rather than pretending an absent machine said nothing.

For a JSON endpoint, ask the same question without changing the model:

```sh
svrn mesh app fanout chores /tasks --json
```

The rail carries the request and attribution. Your app decides how to combine
the answers. It does not merge them for you, because only your app knows
whether two records mean the same thing.

Both commands print the URL before the probe result; the probe line under it is
the verdict, because the bridge answers even when the far side has no such app.

## Second demo: media that nobody copies

Media is the easiest example because everyone understands a film library. Each
person keeps Jellyfin on their own disk. Declare yours once — the command edits
your config for you, comments intact, and refuses to leave a config that will
not load:

```sh
svrn mesh media origin 8096      # Jellyfin on this machine
svrn daemon restart              # the config tier is read at start
```

Check the bridge before setting up any API key:

```sh
python3 library.py check
```

Build the house list and see who holds each title:

```sh
python3 library.py list
```

Play from a particular person's disk:

```sh
python3 library.py play <person> <item-id>
```

Nothing was copied to a central server. The list is the union of what members
already have, and playback comes from whichever member holds the file. If one
person's laptop is asleep, the other titles still work.

`library.py` is intentionally the media-friendly interface. Underneath it asks
each media origin the same question. For a new application, use the generic
form instead:

```sh
svrn mesh app fanout <app-name> <path>
```

Media has a separate viewer command because players such as Jellyfin need an
unmodified, unprefixed HTTP origin for absolute asset paths and byte-range
seeking. That special case should not become the shape every application has
to learn.

## Your key stays with you

Jellyfin has its own API keys. Do not distribute twenty-four copies of your key
around a house. Declare it once on the machine that owns that Jellyfin:

```sh
printf 'MediaBrowser Token="%s"' "$KEY" | svrn mesh media declare authorization
svrn daemon restart
```

The value is read from stdin, stored privately on that machine, and added only
on the way to that machine's own media server. A viewer never receives it.

For an always-on app, the durable form writes a config entry:

```sh
svrn publish chores 5000
svrn daemon restart
```

Use the ephemeral form, `svrn run --as ...`, for experiments. It leaves no
stale entry behind when the process exits. See what the daemon actually
publishes with:

```sh
svrn publish
```

Publishing an app does not publish media. App access and media access are
separate choices.

## Sharing knowledge is two separate choices

The same ownership rule applies to documents and local AI:

- **May someone ask a question and receive a cited answer?** That is
  `query_sharing`.
- **May the underlying index bytes be copied to their machine?** That is
  `mesh_sharing`.

You can let housemates search a licensed collection while keeping its files on
your disk. You can keep a private collection invisible to the mesh. Sharing an
answer is not the same as handing over the cabinet.

## What this gives up

This is not a cloud with a better logo. You give up some cloud assumptions:

- A laptop is not always on. Keep one always-on peer for services that must
  answer at 4am.
- There is no universal username. Names belong to a trusted group, so two
  groups may both have an Alex.
- There is no automatic application merge. The app author writes the small
  piece that understands their records.
- There is no single administrator who can silently remove somebody. Membership
  changes follow the group's consent mechanism.

In return, the house does not need to copy every file into one person's box or
make every side project pass through an OAuth tutorial.

## Reachability is not permission

Whatever physical network carries the packets — home wifi, a radio link between
buildings, a VPN — being able to reach a machine must never be the same thing as
being allowed to use its services. Reachability answers "can I try this door?"
Membership and cryptographic admission answer "may I enter?" The two are kept
separate on purpose, and the app you write inherits the separation for free: a
stranger who can route to your laptop is still refused before your server sees a
byte.

## The real test

Media is the wedge, not the point. The point is the next app:

- a chore rotation;
- a fridge inventory;
- a 3D-printer queue that keeps root on its owner's machine;
- a house knowledge helper that works with the WAN unplugged;
- something someone writes at 1am and shares ninety seconds later.

If only Jellyfin works, this is a media feature with a mesh around it. If
someone else writes a small non-media app and it works, the substrate has done
its job.

## If something is wrong

`svrn mesh status` shows who is in the mesh. `svrn mesh app` with no arguments
shows the OTHER members publishing apps; `svrn mesh media` shows who offers
media. A member missing from those lists has not published that kind of service
or has not restarted after changing config. Your own name does not appear in
them — they are the view of everyone else; `svrn publish` is the view of
yourself.

A `never_asked` or `failed` row is an explicit reason, such as no matching
origin, a sleeping machine, or a refused connection. Render it; do not turn it
into silence. If the daemon is not running:

```sh
svrn daemon start
svrn doctor
```

This is pre-release software. The useful thing to report is the smallest step
where the workshop stopped feeling simple.
