# The ring room — runbook

Every step is a `svrn` command or a browser. You do not need this repository:
install the CLI, run `svrn setup`, and the commands below work. The full
account of what a person watches and why each step is shaped this way is
`docs/RING_ROOM_RUN_OF_SHOW.md`.

---

## 0. Every machine

```bash
svrn setup                    # first run: detects the hardware, downloads models, starts the daemon
```

On the first machine:

```bash
svrn mesh create              # prints an invite
```

On the others:

```bash
svrn mesh join <invite>
```

**What you should see:** `svrn mesh status` lists every member, the models each
is running, and what knowledge each hosts.

---

## 1. The wall — the one machine in the room

It needs a screen and a folder to serve. Scaffold an app (or use one you have),
declare it to the room, and bind the door:

```bash
svrn ring new ./house-expenses
svrn ring serve house-expenses --dir ./house-expenses --bind 192.168.1.20:19947
svrn daemon restart          # the door serves what you declared
```

`--bind` is this machine's room-facing address, the one the door listens on;
the app is served at `/ring/house-expenses/`. Add `--read` for an app guests
may only look at. `svrn ring serve` with no arguments shows what this door
declares, and `svrn ring serve --clear house-expenses` stops serving one app.
One `--wall` link (below) then reaches every app declared this way.

The wall's own screen, for the people in the room:

```bash
svrn ring dev house-expenses --dir ./house-expenses     # serves it on loopback
```

**One QR for the whole wall:**

```bash
svrn mesh grant --wall --ttl 2h \
  --label wall --url http://192.168.1.20:19947/ring/ --qr-svg wall-qr.svg
```

`--model` may be omitted: the grant then reaches this daemon's primary slot
(`svrn model list` prints every id a grant accepts).

**What you should see:** one QR. A phone scans it, a page opens, the person
types a name, and within 5 s the name is on the wall's roster. The member list
is unchanged — the phone is a guest, not a member. `svrn mesh grant --list`
shows the guests with their expiry; `--revoke <token>` ends one.

---

## 2. The keeper — the machine with the folder

```bash
svrn corpus ingest <folder> --corpus <id> --share
```

`--share` is what lets the room's questions reach it. Check with
`svrn corpus status <id>`.

---

## 3. The holder — the machine with the library

```bash
svrn mesh media origin 8096   # where your media server answers (Jellyfin's default port)
svrn daemon restart
svrn mesh media offer         # offer it to the whole mesh; withdraw ends the offer
```

`svrn mesh media origin` with no arguments shows what this node declares;
`--clear` stops offering it.

**What you should see:** the library appears on the wall's Library rail as
"offered to: everyone here" within 30 s; a title's first byte within 5 s;
nobody typed a login. While the holder is watching it themselves the rail says
"in use" and starts nothing.

---

## 4. The phone — nothing installed

Scan the QR. Then:

| the person does | what should happen |
|---|---|
| types a name | it is in the wall's roster within 5 s |
| types a word into the doc | on the wall within 1.4 s: `<name>, guest of <wall>` — never the name alone |
| asks a question only the keeper's folder can answer | first token within 60 s; the answer cites the keeper; `svrn mesh transport` on the wall shows which path carried it (direct or relayed) |
| opens the second app | same name, no second prompt; every act reads `<name>, guest of <wall>` |

`svrn ring log <ns>` from the wall shows every act in the order every node
applies them.

---

## 5. The cut

Cut the wall's internet, leaving its WiFi up. **What you should see:** the doc
keeps working on the phones; a question needing the keeper's folder comes back
saying that folder is unavailable, with no citation of the keeper.
Reconnect — the two replicas are byte-equal within 60 s
(`svrn ring log <ns>` on both ends).

---

## 6. Carry the record to a machine that was never a member

Where the ring is held:

```bash
svrn ring checkpoint <namespace> --out checkpoint.json
```

Carry the file (AirDrop, USB, email to yourself). On a machine the mesh has
never seen:

```bash
svrn ring checkpoint --verify checkpoint.json
```

**What you should see:** exit 0, printing the document's marks and
`verified — N admitted act(s) … no gaps`. `--roster <file>` verifies against
the verifier's own roster instead of the one in the document. One flipped byte,
a truncated tail, or a repeated sequence number each exit 1 with one sentence
naming the failing step and the actor.

---

## 7. The guest from anywhere

A plain mobile browser on a network the daemon has never seen can reach it:
the page loads from one static origin, then dials the wall itself over iroh's
relay. Nothing of yours is exposed — no port, no tunnel, no tailnet — and
nothing proxies HTTP. Measured WORKED at the pinned iroh (`ralph/DECISIONS.md`
browser-dial-2).

On the wall, point the grant at the page's origin:

```bash
svrn mesh grant --wall --ttl 2h \
  --url https://<origin>/ --qr-svg wall-qr.svg
```

The QR's link now carries the wall's dial string (`iroh=`) beside the token;
the guest scans it and their browser connects. Same-network guests are
unaffected — the wall's own door serves the page directly, as in §1.

**What you should see:** the guest's page reaches the wall and returns the
grant-scoped answer, with no address of yours typed anywhere.

The page is `sovereign/apps/ring-runtime`, shipped with the landing deploy:
`landing/scripts/build-ring-runtime.sh` (run by `npm run deploy` from
`landing/`) builds it into `landing/ring/`, which Vercel serves at
`https://svrnme.sh/ring/` — the page path a `--url https://svrnme.sh/` link
composes.

---

## For developers: rehearse it on one machine

The test harness stands four containers in for the machines and judges every
check without any hardware — a repo checkout, `toolbox run -c sovereign-vulkan`:

```bash
./scripts/with-cargo-lock.sh ./scripts/dev-build.sh
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh verdict all
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh down
```

It prints fourteen rows: twelve PASSED and two COULD-NOT-JUDGE (their proofs
are cargo tests and the decisions ledger). Exit 4 is the passing shape. This is
CI, not the demo — nothing above it is needed for the walk.
