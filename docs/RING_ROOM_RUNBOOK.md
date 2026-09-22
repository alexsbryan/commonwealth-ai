# The ring room — runbook

One person in a room with a screen; the rest of the mesh anywhere; guests are
phones with nothing installed. This is the short version: who is which
machine, what to type once, and what should happen during the sitting. The
long account — what a person watches, and why each step is shaped this way —
is `docs/RING_ROOM_RUN_OF_SHOW.md`.

Every step below is a `svrn` command or a browser. You do not need this
repository.

---

## Who is who

| role | what it is | what it does |
|---|---|---|
| **the wall** | one member machine **in the room**, with a screen | shows the QR and the shared doc; serves the guest page; answers the room's questions by fetching them from the keeper |
| **the keeper** | a member machine somewhere else | holds a folder of knowledge and answers questions about it |
| **the holder** | a member machine somewhere else | holds a media library and offers it to the room |
| **the guests** | phones in the room, nothing installed | scan once, type a name, then edit / ask / watch |

The wall and the keeper are the minimum. The holder and a second app are for
the film and multi-app legs.

---

## Once, on each machine

```bash
svrn setup                 # first run: detects hardware, downloads models, starts the daemon
```

Form one mesh — on the first machine:

```bash
svrn mesh create           # prints an invite
```

on every other:

```bash
svrn mesh join <invite>
```

`svrn mesh status` on any machine shows the members, the models each runs, and
what knowledge each hosts.

### On the wall (the one in the room)

Declare the app the room will open. No address to type: the door binds every
interface on its own port, the address a guest link carries is derived by this
machine, and `ring serve` prints the one it will advertise.

```bash
svrn ring new ./house-expenses                          # or an app you already have
svrn ring serve house-expenses --dir ./house-expenses    # declare it to the room
svrn daemon restart                                      # the door serves what you declared
```

`svrn ring serve` with no arguments shows what this door declares;
`--clear house-expenses` stops serving one app; `--read` makes an app
look-only. Each app is served at `/ring/<name>/`.

**If this machine is on more than one network** (a room network *and* a
tailnet, say), only you know which one the guests are on: pass
`--bind <room address:port>` so the door advertises that one.

The wall's own screen, for the people in the room:

```bash
svrn ring dev house-expenses --dir ./house-expenses
```

**One QR for every app declared** (or use the desktop app's Room panel
instead):

```bash
svrn mesh grant --wall --ttl 2h --label wall --qr-svg wall-qr.svg
```

Display `wall-qr.svg` on the wall's screen. `--model` is optional too — the
grant reaches the daemon's primary slot (`svrn model list` prints the ids a
grant accepts). `svrn mesh grant --list` shows outstanding guests with their
expiry; `--revoke <token>` ends one early.

### On the keeper (the machine with the folder)

```bash
svrn corpus ingest <folder> --corpus <id> --share
```

`--share` is what lets the room's questions reach it. `svrn corpus status <id>`
checks.

### On the holder (the machine with the library)

```bash
svrn mesh media origin 8096     # the port your media server answers on (Jellyfin's default)
svrn daemon restart
svrn mesh media offer           # withdraw ends the offer
```

`svrn mesh media origin` alone shows what this node declares; `--clear` stops.

---

## The sitting

1. **A phone scans the QR.** A page opens, served by the wall. The person
   types a name — in the wall's roster within 5 s. The mesh's member list does
   not change: a guest, not a member.
2. **They type into the doc.** On the wall within ~1.4 s: `<name>, guest of
   <wall>` — never the name alone. `svrn ring log <ns>` on the wall shows
   every act, in the order every node applies them.
3. **They ask something only the keeper's folder answers.** First token on the
   phone within 60 s; the answer cites the keeper; `svrn mesh transport` on the
   wall shows whether iroh carried it direct or through a relay.
4. **The holder offers the library.** It appears on the wall's Library rail as
   "offered to: everyone here" within 30 s, and a title plays with no login
   typed (first byte within 5 s). The desktop's Library rail opens a title in
   the browser; `svrn mesh media <holder>` prints the same URL for any player.
   While the holder is watching it themselves, the rail says "in use" and
   starts nothing.
5. **The wall's internet is cut** (WiFi stays up). The doc keeps working on the
   phones; a question needing the keeper's folder comes back saying it is
   unavailable, with no citation of the keeper. Reconnect: the two replicas are
   byte-equal within 60 s.
6. **Nobody became a member by accident.** `svrn mesh status` on the wall and
   the keeper is unchanged; `svrn mesh grant --list` shows the guests with
   their expiry.
7. **The record survives the cut.** Where a ring is held:

   ```bash
   svrn ring checkpoint <namespace> --out checkpoint.json
   ```

   Carry the file (AirDrop, USB, email to yourself) to any machine — even one
   the mesh has never seen — and:

   ```bash
   svrn ring checkpoint --verify checkpoint.json
   ```

   Exit 0, printing the document's marks and `verified — N admitted act(s) … no
   gaps`. One flipped byte, a truncated tail, or a repeated sequence number is
   refused by name with the failing step and the actor. `--roster <file>`
   verifies against the verifier's own roster instead of the document's.

---

## The guest from anywhere (not the room's WiFi)

A plain mobile browser on a network the wall has never seen can still reach
it: the page loads from one static origin, then dials the wall itself over
iroh's relay. Nothing of yours is exposed — no port, no tunnel, no tailnet —
and nothing proxies HTTP. (Measured WORKED at the pinned iroh:
`ralph/DECISIONS.md` browser-dial-2.)

Point the grant at the page's origin instead of the door:

```bash
svrn mesh grant --wall --ttl 2h --url https://svrnme.sh/ --qr-svg wall-qr.svg
```

The link now carries the wall's dial string (`iroh=`) beside the token, so the
guest's browser connects wherever it is. **What you should see:** the guest's
page reaches the wall and returns the grant-scoped answer.

The page is `sovereign/apps/ring-runtime`; it ships with the landing deploy
(`landing/scripts/build-ring-runtime.sh`, run by `npm run deploy`), served at
`https://svrnme.sh/ring/` — the page path a `--url https://svrnme.sh/` link
composes.

---

## For developers: rehearse it on one machine

The test harness stands four containers in for the machines and judges every
check with no hardware. From a repo checkout, in the `sovereign-vulkan`
toolbox:

```bash
./scripts/with-cargo-lock.sh ./scripts/dev-build.sh
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh verdict all
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh down
```

It prints fourteen rows — twelve PASSED and two COULD-NOT-JUDGE (their proofs
are cargo tests and the decisions ledger; exit 4 is the passing shape). This is
CI, not the demo: nothing above it is needed for the walk.
