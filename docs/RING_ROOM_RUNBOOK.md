# The ring room — runbook

The commands, in order, and what each one should print. This is the short
version: the account of what a person watches, why each step is shaped this
way, and the known limits is `docs/RING_ROOM_RUN_OF_SHOW.md`.

Everything runs in the `sovereign-vulkan` toolbox. On the host, prefix each
line with `toolbox run -c sovereign-vulkan`.

---

## 1. The stand-in (one machine, four containers)

Build first — the demo measures binaries and refuses stale ones. Cold is
minutes; warm, seconds.

```bash
./scripts/with-cargo-lock.sh ./scripts/dev-build.sh
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh verdict all
RING_ROOM_TOPOLOGY=room scripts/ring-room-demo.sh down
```

Takes about four minutes after the build. `verdict all` prints fourteen JSON
rows at the end.

**What should happen:** twelve rows `"verdict": "PASSED"`, and two
`"verdict": "COULD-NOT-JUDGE"` (`tl-link-carries-its-couriers`,
`tl-dial-measured` — their proofs are cargo tests and the decisions ledger,
not this run). The command exits **4** when the run is clean: that is the
passing shape, not a failure. Exit **1** is a real FAILED, and it says which.

**If the offline row reads COULD-NOT-JUDGE with reason `cut-not-a-cut`:** the
run was not cold. Rerun it.

**When a row surprises you, the daemon logs are the ground truth:**
`target/ring-room-rr2-demo/*/daemon.err`.

Under the ralph loop (not needed just to watch):

```bash
RING_ROOM_TOPOLOGY=room scripts/ralph-check.sh demo-bg scripts/ring-room-demo.sh
scripts/ralph-check.sh demo-wait     # repeat until it returns; 3 = still running
```

The script name is required — a bare `demo-bg` runs a different demo.

---

## 2. The checkpoint, carried to a machine that was never a member

Where the ring is held:

```bash
svrn ring checkpoint <namespace> --out checkpoint.json
```

Carry the file (AirDrop, USB, email to yourself). On a machine the mesh has
never seen:

```bash
svrn ring checkpoint --verify checkpoint.json
```

**What should happen:** exit 0, printing the document's marks and
`verified — N admitted act(s) … no gaps`. `--roster <file>` verifies against
the verifier's own roster instead of the one in the document. One flipped byte,
a truncated tail, or a repeated sequence number each exit 1 with one sentence
naming the failing step and the actor.

---

## 3. The walk on real machines

One machine in the room (the wall), the others elsewhere on the mesh, phones
that have nothing installed. The wall needs a screen and, on its room-facing
address, these two lines in its config:

```toml
[daemon]
guest_bind = "<room address>:<port>"
guest_pages = { ring-doc = "/path/to/ring-doc", house-expenses = { dir = "/path/to/house-expenses", guests = "read" } }
```

(`guest_bind` is off unless set; a `guests = "read"` app is read-only for the
room. Each app is served at `/ring/<name>/`.)

| # | do this | what you should see |
|---|---|---|
| 1 | `svrn mesh grant --model <id from /v1/models> --wall --ttl 2h --label wall --url http://<wall room address>:<port>/ring/ --qr-svg wall-qr.svg` | one QR. A phone scans it, a page opens, the person types a name, and within 5 s the name is in the doc's roster on the wall. The member list is unchanged. |
| 2 | the person types a word into the shared doc | on the wall within 1.4 s: `<name>, guest of <wall>` — never the name alone. |
| 3 | the person asks a question only the keeper's folder answers | first token on the phone within 60 s; the citation names the keeper; the wall's daemon log names which node served it and by which iroh path. |
| 4 | on the holder: `svrn mesh media offer` (then later `svrn mesh media withdraw`) | the library appears on the wall's Library rail as "offered to: everyone here" within 30 s; a title's first byte within 5 s; no login typed. While the holder is watching it themselves the rail says "in use" and starts nothing. |
| 5 | cut the wall's internet, leaving its WiFi up | the doc keeps working on the phones; a question needing the keeper's folder comes back saying that folder is unavailable, with no citation of the keeper. Reconnect: the two replicas are byte-equal within 60 s. |
| 6 | `svrn mesh status` on the wall and the keeper, before and after; `svrn mesh grant --list` | the member lists are unchanged and the guests are listed with their expiry. |

The wall's own screen is `svrn ring dev ring-doc --dir sovereign/apps/ring-doc`
(loopback). On the keeper, `svrn corpus ingest <folder> --share` makes the
folder answerable from the room.

A second app on the wall: scaffold it (`svrn ring new ./house-expenses`), add
its line to `guest_pages`, restart the daemon, reuse the same `--wall` QR. A
phone that named itself on the doc should be the same person there, and every
act it makes should read `<name>, guest of <wall>` — without editing the
scaffold.

**What can differ from the stand-in:** whether the path between the room and
the keeper is direct or relayed (a relayed run is the one that counts), and
what a real outage looks like to the phones — a real one is rarely under a
minute.

---

## 4. The guest from anywhere — not runnable yet

Today the phone has to be on the wall's network: the QR points at the wall's
room address. The stronger version — a plain mobile browser on a network the
daemon has never seen, no shared LAN, no domain, no tunnel, dialling the daemon
over iroh's relay from the guest link itself — is being measured (campaign
`browser-dial`, bar `bd-browser-dials-relay`). Already on the record: the
locked iroh does build for the browser, and the relay path works for members.
Unmeasured: which channel a guest arrives on, and what the door does with a
guest there.

**When it measures WORKED its command lands in this section.** If it measures
a negative, the failing layer is named here instead.
