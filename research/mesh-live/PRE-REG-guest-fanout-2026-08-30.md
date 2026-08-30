# Pre-registration — does a guest grant reach the whole mesh's compute?

WRITTEN BEFORE ANY DATA. Bars are falsifiable and stated as the OBSERVATION.

## The question

A grant's TOKEN is bilateral: never gossiped, in-RAM on the issuing node, so
19 of 20 nodes would refuse it. Settled, not under test.

Under test: when a guest names a model the LENDER DOES NOT HOLD, does the
lender fan out to the peer that does — making the lender a GATEWAY to pooled
mesh compute rather than a lender of its own hardware?

Established already (2026-08-30, this host):
- FOX minted a grant for `Qwopus3.5-4B-v3-MTP-Q8_0`, which FOX does not hold.
  So MINT-time validation reads the mesh-wide `/v1/models` view
  (`owned_by: mesh`), not the lender's residents.
- With MAC OFFLINE, FOX still advertises that id. Todo `0b76b02f` live.
- FOX has served that exact id from `@ peer Alexs-MacBook-Pro-2` to a LOCAL
  caller. The fanout path works for local callers.

Unproven, and the object of this test: the same fanout for a GUEST caller.

## Design, and the confound it exists to avoid

Guest resolution runs AFTER local (`peer_inference.rs`: `if !local_has { …
guest.lender_for(model_id) … }`). MAC HOLDS Qwopus. So a normal
`svrn chat ask` on MAC would serve from MAC's own slot and never cross to
FOX — the result would be unreadable.

Therefore the probe MUST bypass MAC's daemon: MAC runs `dial_probe --alpn
guest`, which dials FOX's GUEST_ALPN listener directly over iroh. No local
resolution occurs. The request arrives at FOX's guest listener naming a model
FOX lacks.

Topology caveat, stated: the peer FOX would fan out to is MAC itself. Odd,
but it does not invalidate the finding — FOX's routing decision is the object
of study, not MAC's.

## PRECONDITION (checked on BOTH sides within the same minute)

P1  MAC reads **online** in FOX's roster AND FOX reads online in MAC's, at
    probe time. FOX's roster has flapped repeatedly today (1/7 ↔ 2/7).

If P1 fails the run is **could-not-judge**, NOT B2. An offline peer produces
a refusal that looks exactly like a serve-path guard. This guard exists
because that misreading is the easiest wrong conclusion available.

## BARS

B1  **SERVED-BY-FANOUT.** HTTP 200, a real completion, response `model` is
    `Qwopus3.5-4B-v3-MTP-Q8_0`, AND FOX's journal shows a peer dispatch to
    `node-37f17554b6c4ff29` for this request.
    MEANS: a grant reaches compute the lender does not own. The lender is a
    gateway. On a 20-node mesh one node can extend a bearer drawing on all 20.

B2  **REFUSED-AT-SERVE.** Non-200 naming the model unavailable/unknown
    (NOT 401/403).
    MEANS: mint-time validation is permissive but the serve path guards. The
    widening is cosmetic — a mint bug, not a capability hole.

B3  **SILENT SUBSTITUTION.** HTTP 200 but response `model` is anything other
    than the granted id.
    MEANS: §18.3 violation, serious, independent of B1/B2.

B4  **AUTH/TRANSPORT REFUSAL.** 401 or 403.
    MEANS: could-not-judge. Grant dead (lender restart voids RAM store) or
    tunnel down. Re-mint and re-run; do NOT read as B2.

Exactly one of B1/B2/B3 is the result, and only if P1 held.

## Procedure

1. FOX: confirm it does NOT hold the granted id; confirm MAC online.
2. FOX: `svrn mesh grant --model Qwopus3.5-4B-v3-MTP-Q8_0 --ttl 6h --label fanout-probe`
3. FOX: verify the token is PRESENT in `/internal/guest/grant/list`, record pid.
4. MAC: `dial_probe --dial <FOXDIAL> --alpn guest --bearer <token> \
        --path /v1/chat/completions \
        --body '{"model":"Qwopus3.5-4B-v3-MTP-Q8_0",
                 "messages":[{"role":"user","content":"Say: fanout ok"}],
                 "max_tokens":16,"temperature":0}'`
5. FOX: same pid still? If not, B4 — the grant died mid-run.
6. FOX: journal for the request — peer dispatch present or absent.
