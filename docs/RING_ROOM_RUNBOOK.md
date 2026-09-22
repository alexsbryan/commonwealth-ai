# The ring room — runbook

Every step is a `svrn` command or a browser. You do not need this repository:
install the CLI, run `svrn setup`, and the commands below work. The full
account of what a person watches and why each step is shaped this way is
`docs/RING_ROOM_RUN_OF_SHOW.md`.

Three things are config lines rather than verbs today — the wall's guest door,
the wall's app list, and the holder's media origin. They are called out where
they appear; everything else is CLI.

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

It needs a screen and a folder to serve. Scaffold an app (or use one you have):

```bash
svrn ring new ./house-expenses
```

**Config, once** (`~/.svrnmesh/config.toml`), then restart the daemon: the
room-facing address the door listens on, and the apps the room may open.

```toml
[daemon]
guest_bind = "192.168.1.20:19947"
guest_pages = { house-expenses = "/path/to/house-expenses" }
```

A bare path means the room may read and write that app; write
`guests = "read"` for one it may only look at. Each app is served at
`/ring/<name>/`.

The wall's own screen, for the people in the room:

```bash
svrn ring dev house-expenses --dir ./house-expenses     # serves it on loopback
```

**One QR for the whole wall:**

```bash
svrn mesh grant --model <id from /v1/models> --wall --ttl 2h \
  --label wall --url http://192.168.1.20:19947/ring/ --qr-svg wall-qr.svg
```

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

**Config, once** (`[iroh]` in `~/.svrnmesh/config.toml`): where the media
server answers locally.

```toml
[iroh]
media_origin = "127.0.0.1:8096"        # Jellyfin's default
```

```bash
svrn mesh media offer        # offer it to the whole mesh; withdraw ends the offer
```

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

## 7. The guest from anywhere — not runnable yet

Today the phone has to be on the wall's network: the QR points at the wall's
room address. The stronger version — a plain mobile browser on a network the
daemon has never seen, no shared LAN, no domain, no tunnel, dialling the daemon
over iroh's relay from the guest link itself — is being measured (campaign
`browser-dial`, bar `bd-browser-dials-relay`). Already measured: the locked
iroh builds for the browser, and relays carry members today. Unmeasured: which
channel a guest arrives on, and what the door does with a guest there.

**When it measures WORKED, its command lands in this section.** If it measures
a negative, the failing layer is named here instead.

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
