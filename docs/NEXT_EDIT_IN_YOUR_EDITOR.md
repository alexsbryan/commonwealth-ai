# Next edit: your editor finishes the change you started

You rename something. You fix the same call twice. Somewhere below the
fold there are eleven more sites, and you know it. This feature watches
the edits you actually made, works out the pattern, and offers the rest
as a queue you walk with **Tab** — including the ones off-screen.

It runs on your own machine. No account, no cloud round-trip, no code
leaving the building. And unlike a cloud completion, every suggestion
can tell you *why* it appeared.

At the end of this page you'll have:

- Repeated-edit suggestions working in VS Code (or Cursor, or Windsurf).
- A way to see the reasoning behind any suggestion — including the
  silences, which are explained rather than mute.
- An honest picture of what it will and won't do, and how we measured
  that.

## Before you start

**You need the Sovereign daemon.** It's one background process that
serves your editor over loopback. If you've never run it:

```sh
curl -fsSL https://svrnme.sh/install.sh | sh
svrn setup --fim
```

`svrn setup --fim` prints a plan before it touches anything — which
model, what it downloads, which config keys change — and asks. On
approval it sets up the daemon, downloads the completion model, and
installs the editor extension.

**There are two engines behind one Tab key**, and it's worth knowing
which one is talking to you:

| | Rule engine | Model engine |
|---|---|---|
| Handles | the same literal edit, repeated | patterns with per-site variation |
| Needs a model | **no** | yes — the FIM slot from `svrn setup --fim` |
| Speed | ~6 ms | ~1–2 s |
| Can it invent something? | structurally no — it only does string search | yes, which is why it's fenced hard |

The rule engine works on a machine with no model at all. If you skipped
the model download, everything in step 2 still works; step 3 stays
quietly inert and tells you so when asked.

## 1 — Check it's live

```sh
svrn daemon start
curl -s http://127.0.0.1:9741/status | grep -o '"fim":[^}]*}'
```

A `"fim"` object naming a model means both engines are available. `null`
means the rule engine alone — fine, and step 3 will say `unavailable`
rather than failing silently.

In your editor, the status bar reads **svrn fim**. Struck through means
the daemon is down; a warning icon means the daemon is up but has no
model slot.

## 2 — The rule engine, in about fifteen seconds

Open a file with a repeated pattern. Say this one:

```js
console.log("start");
console.log("mid");
console.log("end");
console.log("done");
```

Change the first `log` to `debug`. Change the second. **Pause.**

The third and fourth sites decorate: old text struck through, the
replacement in ghost text, and a hint at the end of the line. **Tab**
accepts and jumps to the next. **Esc** dismisses — and quiets that
particular pattern for the rest of the session, so it won't nag.

If the next site is off-screen you get a one-line hint at your cursor
instead of a viewport jump — the editor never scrolls uninvited. The
first Tab takes you there; the ones after that accept and advance.

Two edits is not a coincidence, but one is. One edit never fires
anything. Short or ambiguous patterns need three.

## 3 — The model engine, for the cases a rule can't express

Some patterns aren't one literal rewrite. Add a timeout argument to two
calls that don't otherwise look alike:

```go
conn   := dial(primaryHost, 8080, timeoutMS)
backup := dial(backupHost, altPort, timeoutMS)
mirror := dial(mirrorHost, 9090)
```

Edit the first two by hand, pause, and the third is offered — even
though no single find-and-replace describes what you did. The hint
reads `model · signature_fanout`, naming the shape it recognized.

The same goes for replacements that vary per site — `.unwrap()` →
`.expect("load config")` in one place and `.expect("bind socket")` in
the next — and for repeated multi-line insertions, like the same field
added to several struct literals.

The model is only consulted when the rule engine has already declined
**and** your last two edits are similar-but-not-identical in one of
those recognized shapes. It never overrides a rule-engine answer, and
it never queues: if the model is busy serving your chat, the consult is
dropped rather than made to wait.

## 4 — Ask it why

This is the part a cloud completion can't give you. Every response
carries its own reasoning, and you can read it directly:

```sh
curl -s http://127.0.0.1:9741/v1/edit_predictions \
  -H 'content-type: application/json' -d '{
  "history": [
    {"before":"log","after":"debug","left":"console.","right":"(\"a\");"},
    {"before":"log","after":"debug","left":"console.","right":"(\"b\");"}
  ],
  "text": "console.log(\"c\");\nconsole.log(\"d\");\n",
  "cursor": 0, "debug": true, "model_lane": true
}' | python3 -m json.tool
```

You get the proposed edits plus a `sovereign_debug` block: the rule it
induced (`console.log(` → `console.debug(`), how many edits supported
it, how many sites remain, and timings. When nothing is proposed, that
block says **why** — `no_rule`, `below_threshold`, `no_sites`.

The model engine explains itself the same way under `sovereign_debug.model`:

- `skipped` — the gate never consulted the model. `rule_fired` (the
  rule engine already answered), `gate` (your edits weren't a
  recognized shape), `casing_deferred` (see below).
- `dropped` — the model *was* consulted but its answer didn't survive.
  `unavailable` (no model slot), `busy`, `timeout`, `truncated`,
  `region_empty` / `region_too_large` / `region_has_markers` (the
  window wasn't safe to rewrite), `invalid` (the reply wasn't a clean
  region), `noop` (it proposed nothing).

A dropped prediction is reported as dropped. It is never patched up
into a suggestion — see the next section for why that matters.

In the editor, **Output → svrn fim** carries the same story, and
`svrn fim: Diagnose Completion Setup` walks the three probes (daemon →
slot → real round-trip) and prints the fix for the first failure.

## What it will and won't do

The expensive failure here isn't a missed suggestion — it's a **wrong
edit**, because you might accept it. Everything is tuned around that
asymmetry, and the honest consequences are:

- **It stays quiet a lot.** Silence is the default and the healthy case.
- **A model answer that looks off is discarded whole**, never repaired
  into a partial edit. If the reply arrives truncated, wrapped in chat
  prose, or rewriting far more of the file than the pattern justifies,
  you get nothing rather than something.
- **Cross-casing renames are deliberately not offered.** If you rename
  `getUserData` and `get_user_data` still exists elsewhere, the system
  detects it and declines by name (`casing_deferred`). We measured the
  model as actively destructive on that one shape — reversing renames,
  deleting unrelated blocks — so it's switched off until a
  deterministic engine handles it. The detection already works; the
  suggestion is what's withheld.
- **History is per-file.** Switching files starts the pattern over.
- **Sites are single-file.** Nothing crosses file boundaries yet.
- Undo isn't treated as a pattern — undoing an edit won't teach it to
  propose the reverse.

Before applying anything, the extension re-checks that the text at the
target still matches what the suggestion was computed against. If your
document moved on, the suggestion is dropped rather than applied at a
stale position.

## How we know

Both engines are gated by eval banks in this repo, with the pass marks
written down *before* the runs — so a bad result is a bug to fix, not a
number to move.

- **Rule engine** — 120 cases mined from this repository's real commit
  history plus hand-written probes (`gym/next-edit/`). Latest run:
  120/120, zero malformed or wrong edits, p95 **6 ms**.
- **Model engine** — 60 hand-written cases across 11 languages
  (`gym/next-edit/gen/`), including 20 negatives where the correct
  answer is silence. Latest run: 30/30 fired-and-correct, **zero** wrong
  edits, all negatives silent, p95 **1.8 s**.

Both banks earned their keep. The rule bank's first run caught a real
bug: an insertion-shaped rule re-proposing sites it had already edited
(`await await fetch(`). The model bank's first two runs are why casing
renames are switched off — that decision came from measurement, not
taste, and the evidence is recorded in `gym/next-edit/gen/README.md`.

Run them yourself against your own daemon:

```sh
python3 scripts/next_edit_eval.py          # rule engine, no model needed
python3 scripts/next_edit_gen_eval.py      # model engine, needs the FIM slot
```

Each exits non-zero if its gates fail.

The caveat worth stating: a 60-case bank is a floor, not a proof. It
says the failure modes we know about are handled and measured. It does
not say the model will never surprise you — which is why the drop-rather-
than-repair posture exists underneath it.

## Settings

| setting | default | what |
|---|---|---|
| `sovereign-fim.nextEdit.enable` | `true` | the whole feature |
| `sovereign-fim.nextEdit.modelLane` | `true` | the model engine only; `false` leaves the rule engine running |
| `sovereign-fim.nextEdit.settleMs` | `600` | how long you pause before it thinks |
| `sovereign-fim.disabledLanguages` | `["markdown","plaintext"]` | languages it stays out of |

Turning off `modelLane` is the cheap way to get a purely deterministic
experience — no model involved in anything you're offered.

## If it's not firing

1. **Nothing appears at all.** You need two edits of the same shape, and
   a pause. Check `sovereign-fim.nextEdit.enable`, and that the status
   bar isn't struck through.
2. **The rule engine works, the model engine never does.** Ask it: the
   `curl` above will say `unavailable` if there's no FIM slot resident.
   `svrn setup --fim` sets one up.
3. **It fires but the suggestion is wrong.** That's the failure we care
   about most — the debug block plus the file is a genuinely useful bug
   report.
4. `svrn doctor` for daemon-side self-checks.

## Under the hood

The extension is a thin capture-and-render shell: it coalesces your
keystrokes into edit units, ships them with the document to
`POST /v1/edit_predictions`, and renders whatever queue comes back.
Every policy decision — context expansion, induction, the firing
threshold, whether to consult a model — lives in the daemon, so any
other editor client inherits identical behaviour by speaking the same
HTTP route.

The full design, the trigger policy, and the reasoning behind the
precision posture are in
[`sovereign/docs/NEXT_EDIT.md`](../sovereign/docs/NEXT_EDIT.md). The
ghost-text completion this feature sits beside is documented in
[`sovereign/docs/INLINE_COMPLETION.md`](../sovereign/docs/INLINE_COMPLETION.md).
