# svrn fim — inline completion

Ghost-text code completion served by **your own machine**. No cloud, no
account, no telemetry — the model runs inside the Sovereign daemon on
localhost, and every suggestion carries an explanation of what produced
it (model, slot, stop rule, latency) that cloud completions structurally
cannot give you.

Works with any text editor language — Rust, TypeScript, Python, Go, …

---

## Setup (about 10 minutes)

### 1. Get the Sovereign daemon

You need `sovereign-cli-daemon` built and runnable. See
[`GETTING_STARTED.md`](../../docs/GETTING_STARTED.md) in the repo. If you
can run `sovereign daemon run` and it stays up, you're done with this step.

### 2. Download two small models

A coder model (does the completing) and a tiny embed model (the daemon
requires one; ~50 MB). Into `~/.sovereign/models/` (or anywhere — the
config takes absolute paths):

```bash
mkdir -p ~/.sovereign/models

# Coder model — Mellum2-12B-A2.5B (validated artifact; ~10.9 GB).
# On a smaller machine, Qwen2.5-Coder-1.5B-Q8_0 (~1.6 GB) works the same.
curl -L -o ~/.sovereign/models/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf \
  "https://huggingface.co/JetBrains/Mellum2-12B-A2.5B-Instruct-GGUF-Q6_K/resolve/main/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"

# Tiny embed model (~0.6 GB)
curl -L -o ~/.sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf \
  "https://huggingface.co/Qwen/Qwen3-Embedding-0.6B-GGUF/resolve/main/Qwen3-Embedding-0.6B-Q8_0.gguf"
```

Any coder GGUF whose tokenizer carries FIM markers works — the daemon
probes the vocab at boot and tells you in the log if your model doesn't
qualify. Validated: Mellum2 Instruct (JetBrains, recommended above),
Mellum2 Thinking, Qwen2.5-Coder (base).

### 3. One config block

Edit `~/.svrnmesh/config.toml` (create it if the daemon hasn't yet):

```toml
[models]
primary = "~/.sovereign/models/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"
embed   = "~/.sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf"

[models.fim]
path    = "~/.sovereign/models/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"
```

Because `primary` and `models.fim.path` are the **same file**, the daemon
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

---

## Seeing what's going on (glassbox)

This is the part no other completion product gives you:

- **`svrn fim: Explain Last Suggestion`** (Ctrl/Cmd+Shift+P) — the model
  id, the slot, which stop rule ended the completion, prompt size, and
  ttft/total timings for your last suggestion.
- **`svrn fim: Diagnose Completion Setup`** — runs three probes
  (daemon up → FIM slot live → round-trips a real completion) and prints
  PASS/FAIL with the copy-pasteable fix for the first failure.
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

## Troubleshooting ladder

1. **`svrn fim: Diagnose Completion Setup`** — it names the failing rung.
2. `curl http://127.0.0.1:9741/status` — `inference.fim` should be non-null.
3. `sovereign doctor` — daemon-side self-checks.

Status bar says **"svrn fim" with a warning icon** = the daemon is up
but `[models.fim]` is missing (hover it for the exact config block).
**"svrn fim" struck through** = the daemon itself is down.

## Upgrading: a dedicated FIM model

If you chat with the daemon while you type, chat traffic shares the fast
slot and your keystrokes queue behind it. Give FIM its own model — any
coder GGUF **different** from `primary` gets its own pinned, always-
resident slot (~1–2 GB for a 1.5–3B coder):

```toml
[models]
primary = "~/.sovereign/models/<your-chat-model>.gguf"
embed   = "~/.sovereign/models/Qwen3-Embedding-0.6B-Q8_0.gguf"

[models.fim]
path    = "~/.sovereign/models/Qwen2.5-Coder-1.5B-Q8_0.gguf"
```

## How it works (one paragraph)

The extension sends the code before/after your cursor to
`POST /v1/completions` on your daemon. The daemon assembles a FIM prompt
in the model's own special-token format, serves it from a pinned slot
with prefix caching (typing only re-processes the delta), and applies
structural stop rules (single-line vs block-body, dedupe against the text
after your cursor) — all visible via the debug payload. Cancellation is
real: superseded keystrokes close the socket and the model stops
mid-token. Full design: [`sovereign/docs/INLINE_COMPLETION.md`](../../sovereign/docs/INLINE_COMPLETION.md).
