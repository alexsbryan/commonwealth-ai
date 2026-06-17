<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  SetupFlow — the only multi-step screen in the entire app.

  Listens to a single `setup-progress` Tauri event channel and invokes
  `complete_setup_auto`, which runs the whole setup chain (hardware probe
  → 3-model download → DB open → model load → smoke test) with no user
  choices. The actual rendering lives in `SetupScreen.svelte` (a pure,
  backend-free view) — this file is just the wiring that feeds it live
  progress. The dev screen gallery drives the same SetupScreen with
  per-phase fixtures, so you can audit every phase's copy without setup.

  No cancel button: closing the window pauses (downloads resume from
  `.part` on relaunch).
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import {
    completeSetupAuto,
    primaryCatalog,
    slotRecommendation,
    getConfig,
  } from "../api";
  import SetupScreen from "./SetupScreen.svelte";
  import type { Progress, Provenance, SlotProvenance } from "./setupTypes";

  interface Props {
    onComplete: () => void;
    /// The user's "Customize" primary-model choice from the Setup Plan
    /// screen (a catalog GGUF filename). Undefined = hardware-recommended.
    primaryFile?: string;
  }

  let { onComplete, primaryFile }: Props = $props();

  const initialProgress: Progress = {
    phase: { kind: "detecting_hardware" },
    message: "Reading what this machine can do.",
    fraction: null,
    eta_seconds: null,
    indeterminate: true,
  };

  let progress = $state<Progress>({ ...initialProgress });
  let failed = $state<{ message: string; recoverable: boolean } | null>(null);
  let unlisten: UnlistenFn | null = null;

  // Read-only provenance for the ledger — what WILL download, from where, to
  // where. The setup-progress event carries only a generic message, so we
  // fetch this and SetupScreen joins it to the current phase. Best-effort:
  // if it fails the screen falls back to the plain phase messages.
  let provenance = $state<Provenance | null>(null);

  function slim(s: {
    base_name: string;
    file: string;
    quant: string;
    size_gb: number;
    hf_url: string;
  }): SlotProvenance {
    return {
      name: s.base_name || s.file,
      quant: s.quant,
      size_gb: s.size_gb,
      repo:
        (s.hf_url || "")
          .replace(/^https?:\/\/huggingface\.co\//, "")
          .replace(/\/$/, "") || "HuggingFace",
    };
  }

  async function loadProvenance() {
    try {
      const [cat, f, e, cfg] = await Promise.all([
        primaryCatalog(),
        slotRecommendation("fast"),
        slotRecommendation("embed"),
        getConfig().catch(() => null),
      ]);
      const prim =
        (primaryFile ? cat.find((o) => o.file === primaryFile) : null) ??
        cat.find((o) => o.recommended) ??
        cat[0] ??
        null;
      provenance = {
        modelsDir: cfg?.data_dir ? `${cfg.data_dir}/models` : "~/.sovereign/models",
        primary: prim ? slim(prim) : null,
        fast: f ? slim(f) : null,
        embed: e ? slim(e) : null,
      };
    } catch {
      // Ledger degrades to the generic phase messages — non-fatal.
    }
  }

  onMount(async () => {
    unlisten = await listen<Progress>("setup-progress", (e) => {
      progress = e.payload;
      if (e.payload.phase.kind === "failed") {
        failed = {
          message: e.payload.message,
          recoverable: e.payload.phase.recoverable,
        };
      } else {
        failed = null;
      }
    });
    void loadProvenance();
    try {
      await completeSetupAuto(primaryFile);
      onComplete();
    } catch (e) {
      // Backend will already have emitted Failed; this catch handles
      // the pathological case where it didn't.
      if (!failed) {
        failed = { message: String(e), recoverable: false };
      }
    }
  });

  onDestroy(() => {
    unlisten?.();
  });

  async function retry() {
    failed = null;
    progress = { ...initialProgress };
    try {
      await completeSetupAuto(primaryFile);
      onComplete();
    } catch (e) {
      if (!failed) {
        failed = { message: String(e), recoverable: false };
      }
    }
  }
</script>

<SetupScreen {progress} {failed} {provenance} onRetry={retry} />
