# Run a mesh app — replicate the Enron demo

You saw the Enron explorer online and want to run it yourself. You have **Sovereign
Desktop** installed. This takes about two minutes, most of it a one-time download.

## Steps

1. **Open Sovereign Desktop** and find the **Mesh apps** section (in the sidebar /
   settings area).

2. Find **Enron Task Force**. Its card shows the data status:

   ```
   Enron Task Force
   Cross-inbox identity graph over thousands of Enron emails…
   Data: not downloaded · 0.35 GB · requests read corpus atoms
   [ Get data (0.35 GB) & Open ]
   ```

3. Click **Get data (0.35 GB) & Open**. The desktop:
   - records your consent (the app gets *read-only* access to that one corpus);
   - downloads the prebuilt index from HuggingFace (a progress bar shows on the card);
   - opens the sandboxed explorer when the data is ready.

   The download is one-time. Next launch the card just says **Open**.

4. **Explore.** You get the counterparty graph (drag the nodes), the collapse
   timeline (click a month), the reconciled identities (click to see the folded
   aliases + the signal that fired), and the cited drill-down — every relationship
   opens the **source email** it was extracted from.

That's it. Nothing left your machine, no CLI, no account.

## What just happened (for the curious)

The mesh app *declares* its data dependency in its manifest (`meshapp.json`):

```json
"corpus": "enron-sample-multi-wide",
"corpus_data": { "size_indexed_gb": 0.35, "recipe": "recipe.toml" }
```

When you clicked **Get data**, the desktop:
1. read the corpus **recipe** the app ships (it carries a `[prebuilt]` block — a
   HuggingFace snapshot ref + a SHA-256), and wrote it to `~/.svrnmesh/recipes/`
   so the daemon can resolve it;
2. ran the normal corpus install, which took the **prebuilt fast-path**: download
   the snapshot, verify the hash, restore the index under
   `~/.svrnmesh/indexes/enron-sample-multi-wide/` — no re-embedding, seconds not
   hours.

The app runs in a sandbox (strict CSP, no network egress); its *only* channel to the
host is the permission-gated `window.meshApp` bridge, and it was granted only
`mesh_store_read`. It can read the corpus and cite it; it can't touch anything else.

## Doing it from the terminal instead

If you prefer the CLI (or the desktop download fails), the same corpus installs with:

```bash
sovereign corpus install enron-sample-multi-wide
```

(This works from a repo checkout, where the recipe is on disk. The desktop's one-click
path is what makes it work *without* a checkout.)

## Troubleshooting

- **Download fails / stalls.** It's a ~0.35 GB fetch from HuggingFace — retry; the
  install is resume-aware. Check your network if it persists.
- **"corpus … not found".** The app's bundle is missing its `recipe.toml`, or the
  manifest has no `corpus_data`. That app isn't packaged for one-click acquire yet
  — fall back to the CLI command above.
- **Opens but is blank / errors.** The corpus installed but the index is incomplete.
  Remove and reinstall: `sovereign corpus remove enron-sample-multi-wide` then retry.

See **MESHAPP_AUTHORING.md** to build your own.
