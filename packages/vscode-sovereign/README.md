# svrn fim — inline completion

Ghost-text code completion served by **your own machine**. No cloud, no
account, no telemetry — the model runs inside the Sovereign daemon on
localhost, and every suggestion carries an explanation of what produced
it (model, slot, stop rule, latency) that cloud completions structurally
cannot give you.

Works with any text editor language — Rust, TypeScript, Python, Go, …

---

## Setup (one command)

```bash
svrn setup --fim
```

That prints a plan — which model, which quant, what it downloads, which
config keys change, what it backs up — and asks before touching
anything. On approval it downloads Mellum2, writes `[models.edit]`,
restarts the daemon, round-trips a real completion to prove the slot is
live, and installs this extension into `code` / `cursor` / `windsurf`.
Add `--yes` for unattended runs, `--skip-editor` to stop at the daemon.

It runs **lean mode**: `[models].primary` and `[models.edit].path` point
at the same file, so completions come from the always-resident fast slot
with one copy in RAM. **That replaces the chat model on this machine** —
the old config is saved to `config.toml.pre-fim` and the closing banner
prints the one-line restore.

Pick a different quant with `--quant`:

| rung | size | for |
|---|---|---|
| `mxfp4_moe` | 7.0 GB | cpu-only / low-memory (the default there) |
| `q4_k_m` | 8.1 GB | 8–19 GB VRAM (the default there) |
| `q6_k` | 10.9 GB | ≥20 GB VRAM (the default there); the validated artifact |
| `q8_0` | 12.9 GB | when you have memory to spare |

All four are the same model — JetBrains Mellum2-12B-A2.5B-Instruct, an
MoE with 2.5B active params, so it generates at 2.5B speed at 12B
weights. Re-run `svrn setup --fim --quant q8_0` to move rungs; that
keeps `primary` and `models.edit.path` in sync, which `svrn model set`
would not.

<details>
<summary>Manual setup — what the flag automates</summary>

### 1. Get the Sovereign daemon

You need `sovereign-cli-daemon` installed and runnable:

```bash
curl -fsSL https://svrnme.sh/install.sh | sh
```

If you can run `sovereign daemon run` and it stays up, you're done with
this step.

### 2. Download two small models

A coder model (does the completing) and a tiny embed model (the daemon
requires one; ~50 MB). Into `~/.svrnmesh/models/` (or anywhere — the
config takes absolute paths):

```bash
mkdir -p ~/.svrnmesh/models

# Coder model — Mellum2-12B-A2.5B Q6_K (validated artifact; ~10.9 GB).
# Smaller rungs of the SAME model: -Q4_K_M (8.1 GB), -MXFP4_MOE (7.0 GB).
curl -L -o ~/.svrnmesh/models/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf \
  "https://huggingface.co/JetBrains/Mellum2-12B-A2.5B-Instruct-GGUF-Q6_K/resolve/main/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"

# Tiny embed model (~0.6 GB)
curl -L -o ~/.svrnmesh/models/Qwen3-Embedding-0.6B-Q8_0.gguf \
  "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/main/Qwen3-Embedding-0.6B-Q8_0.gguf"
```

Mellum2 is the family we ship and support. Mechanically, any coder GGUF
whose tokenizer carries atomic FIM markers will serve ghost text — the
daemon probes the vocab at boot and logs which lanes the slot got — but
`svrn setup --fim` only ever installs Mellum2, and that's what the
marker table, the smoke script, and these instructions are validated
against.

A GGUF **without** those markers is not a failure, just a narrower slot:
ghost text is withheld and next-edit suggestions (the Tab queue below)
keep working, since those need no special vocabulary.

### 3. One config block

Edit `~/.svrnmesh/config.toml` (create it if the daemon hasn't yet):

```toml
[models]
primary = "~/.svrnmesh/models/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"
embed   = "~/.svrnmesh/models/Qwen3-Embedding-0.6B-Q8_0.gguf"

[models.edit]
path    = "~/.svrnmesh/models/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"
```

(`[models.fim]` is the old name for this section and still works, but
write `[models.edit]` in new configs — the section covers both editing
lanes, not just fill-in-the-middle.)

Because `primary` and `models.edit.path` are the **same file**, the daemon
serves completions from its always-resident fast slot — one copy of the
model in RAM, nothing extra loaded. (This is "lean mode". If you also
chat with the daemon heavily, see *Upgrading* below.)

### 4. Restart the daemon

```bash
sovereign daemon restart
```

Cold model load takes 30–60s the first time (mmap warmup).

### 5. Verify — 10 seconds

```bash
curl http://127.0.0.1:9741/v1/completions \
  -H 'content-type: application/json' \
  -d '{"prefix": "fn fibonacci(n: u32) -> u32 {\n    match n {\n        0 => 0,\n        1 => 1,\n        _ => ", "suffix": "\n    }\n}\n"}'
```

You should get back something like
`"text":"fibonacci(n - 1) + fibonacci(n - 2)\n    }"`.
If instead you get a 503, the response body tells you exactly what to fix.

### 6. Install the extension

```bash
code --install-extension sovereign-fim-0.1.0.vsix
```

(The `.vsix` is attached to the GitHub release, or build it yourself:
`npm install && npm run package` in `packages/vscode-sovereign`.)

### 7. Type

Open a code file, start a function, pause a beat. Ghost text appears;
`Tab` accepts. The status bar (bottom right) shows the serving model.

</details>

---

## Next edit (repeated-edit suggestions)

Make the same small edit twice — say, two `console.log` lines turned
into `console.debug` — pause, and the extension proposes the
remaining occurrences as a tab-through queue: the old text struck
through, the replacement as ghost text, **Tab** to accept and jump
to the next site, **Esc** to dismiss (which also stops that pattern
from re-nagging this session). If the next site is off-screen you
get a one-line hint at your cursor instead of a viewport jump; the
first Tab takes you there.

Two engines sit behind that one Tab key, and the debug payload always
names which one answered:

- **Rule engine** — the daemon induces a literal rewrite rule from
  your last few edits and finds the remaining sites by string search.
  Deterministic, ~6 ms, incapable of inventing anything, and it works
  with **no model at all** (no `[models.edit]` slot needed). It speaks
  only past a confidence threshold: two supporting edits for a
  specific pattern, three for a short one.
- **Model engine** — for patterns no single literal rule describes:
  the same argument added to differently-shaped call sites, a
  replacement that varies per site (`.unwrap()` → `.expect("…")` with
  a different message each time), the same field added to several
  struct literals. It is consulted only when the rule engine has
  declined *and* your last two edits match one of those recognized
  shapes, it never overrides a rule-engine answer, and it never
  queues for the slot — a busy model means the consult is dropped,
  not delayed. Needs a resident editing model — but **not a coder
  one**: unlike ghost text, this engine rides the model's ordinary
  prompt surface, so an everyday chat model serves it (measured, our
  60-case bank: a general 35B scored 21/30 useful with zero wrong
  edits against 19/30 for a purpose-built 1.5B; the specialist wins on
  speed, ~0.8 s vs ~2.6 s, not on correctness). Silently inert when
  there is no editing model at all, and says `unavailable` when asked.

A model answer that fails validation is **dropped whole, never
repaired** — no suggestion beats a wrong one. Cross-casing renames
(`getUserData` edited, `get_user_data` still present) are detected but
deliberately **not offered**: the model measured as destructive on
exactly that shape, so it is withheld pending a deterministic engine.

Full walkthrough, including how to read the reasoning behind any
suggestion or silence: [Next edit in your
editor](../../docs/NEXT_EDIT_IN_YOUR_EDITOR.md). Design + policy:
`sovereign/docs/NEXT_EDIT.md`.

## Seeing what's going on (glassbox)

This is the part no other completion product gives you:

- **`svrn fim: Explain Last Suggestion`** (Ctrl/Cmd+Shift+P) — the model
  id, the slot, which stop rule ended the completion, prompt size, and
  ttft/total timings for your last suggestion.
- **`svrn fim: Diagnose Completion Setup`** — walks the probes (daemon
  up → editing slot live, naming which of the two lanes it serves →
  round-trips a real completion) and prints PASS/FAIL with the
  copy-pasteable fix for the first failure. A model that serves next-edit
  but not ghost text reports the round-trip as **SKIP**, not FAIL — that
  is a supported arrangement, and the daemon's own one-line advice is
  printed alongside it.
- **Output → svrn fim** — a rolling log of your last 20 suggestions
  with their timings.

## Settings

| setting | default | what |
|---|---|---|
| `sovereign-fim.enable` | `true` | master switch |
| `sovereign-fim.endpoint` | `http://127.0.0.1:9741` | daemon base URL |
| `sovereign-fim.debounceMs` | `120` | keystroke debounce |
| `sovereign-fim.maxPrefixLines` | `60` | context lines before cursor |
| `sovereign-fim.maxSuffixLines` | `20` | context lines after cursor |
| `sovereign-fim.disabledLanguages` | `["markdown", "plaintext"]` | no-ghost zones |
| `sovereign-fim.nextEdit.enable` | `true` | repeated-edit suggestions (Tab queue) |
| `sovereign-fim.nextEdit.settleMs` | `600` | idle time before the daemon is consulted |
| `sovereign-fim.nextEdit.modelLane` | `true` | the model engine for next-edit; `false` keeps the deterministic rule engine only |

## Troubleshooting ladder

1. **`svrn setup --fim --yes`** — re-runs the whole thing. It is
   idempotent: files already on disk aren't re-downloaded, and it walks
   the same three probes as the Diagnose command, from the shell.
2. **`svrn fim: Diagnose Completion Setup`** — it names the failing rung.
3. `curl http://127.0.0.1:9741/status` — `inference.edit` should be
   non-null, and `fim_style` should be present inside it (that field is
   what says ghost text specifically is live; `next_edit_format` is the
   Tab queue). If `advice` is set, it names the fix in one sentence.
   `inference.fim` is still emitted as a deprecated copy of the same
   object for one release.
4. `sovereign doctor` — daemon-side self-checks.

The status bar icon names the arrangement, and only the first two are
faults — hover it for the details and, when the daemon has something to
say, its one-line advice:

| icon | shows | means |
|---|---|---|
| slashed circle | `svrn fim` | the daemon itself is down |
| warning triangle | `svrn fim` | daemon up, no editing model at all — `[models.edit]` is missing |
| info circle | the model id | next-edit works off your resident chat model; no `[models.edit]` was chosen, and a specialist would answer roughly 3x faster |
| lightbulb | the model id | next-edit only — this model's vocabulary has no FIM markers, so there is no ghost text |
| lightning bolt | the model id | both lanes: ghost text and next-edit |

## Keeping a separate chat model

Lean mode's one tradeoff: chat and completions share a slot, so if you
chat with the daemon while you type, keystrokes queue behind chat
traffic. Pointing `[models.edit].path` at a GGUF **different** from
`primary` gives editing its own pinned, always-resident slot and removes
the contention.

```toml
[models]
primary = "~/.svrnmesh/models/<your-chat-model>.gguf"
embed   = "~/.svrnmesh/models/Qwen3-Embedding-0.6B-Q8_0.gguf"

[models.edit]
path    = "~/.svrnmesh/models/Mellum2-12B-A2.5B-Instruct-MXFP4_MOE.gguf"
```

Do the memory arithmetic before you commit to it, which is why
`svrn setup --fim` does not offer this arrangement: the smallest Mellum2
artifact is 7.0 GB, and that is 7.0 GB **on top of** your chat primary,
resident all the time. Against the curated primaries in
`sovereign/models.toml` that is ~3.7 GB of headroom on the 20–23 GB tier
and ~3.5 GB on the ≥24 GB tier — it does not fit. It works when you pair
it with a small chat model, or on a large unified-memory machine.

## How it works (one paragraph)

The extension sends the code before/after your cursor to
`POST /v1/completions` on your daemon. The daemon assembles a FIM prompt
in the model's own special-token format, serves it from a pinned slot
with prefix caching (typing only re-processes the delta), and applies
structural stop rules (single-line vs block-body, dedupe against the text
after your cursor) — all visible via the debug payload. Cancellation is
real: superseded keystrokes close the socket and the model stops
mid-token. The full design lives in `sovereign/docs/INLINE_COMPLETION.md`
in the source tree, which opens with the beta.
