<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  ScreenGallery — a no-backend "storybook" for the onboarding screens.

  Renders the REAL components (WelcomeThreshold, ConsentGate) and the pure
  SetupScreen view with per-phase fixtures, so you can audit the copy, click
  through the flow, and check the recommended models WITHOUT a daemon,
  models, or any setup side effects. Served by `npm run dev` (or
  `npm run screens`) at /screens.html — pure browser, no Tauri.

  Note: the setup phase strings come from `src-tauri/src/setup_flow.rs` and
  the model list from `sovereign/models.toml` (shown raw, below); both are
  the real source of truth, mirrored here so the preview can't drift on the
  models and matches the live copy on the phases.
-->
<script lang="ts">
  import WelcomeThreshold from "./WelcomeThreshold.svelte";
  import ConsentGate from "./ConsentGate.svelte";
  import SetupScreen from "./SetupScreen.svelte";
  import SetupPlanView from "./SetupPlanView.svelte";
  import type { Progress } from "./setupTypes";
  import type { RecommendedProfile, PrimaryOption, SlotConfig } from "../types";

  interface Props {
    /** Raw contents of sovereign/models.toml (imported `?raw`). */
    modelsToml: string;
  }
  let { modelsToml }: Props = $props();

  type ScreenId = "welcome" | "setup_plan" | "setup" | "consent" | "models";
  let screen = $state<ScreenId>("welcome");

  const SCREENS: { id: ScreenId; label: string }[] = [
    { id: "welcome", label: "1 · Welcome" },
    { id: "setup_plan", label: "2 · Setup plan (consent)" },
    { id: "setup", label: "3 · Setup (ledger)" },
    { id: "consent", label: "4 · Mesh consent" },
    { id: "models", label: "Recommended models" },
  ];

  // Fixtures for the Setup Plan view (the data it normally fetches via the
  // read-only commands). Values mirror the shipped models.toml so the preview
  // is representative.
  const PLAN_PROFILE: RecommendedProfile = {
    profile: "very_high",
    effective_memory_gb: 64,
    is_unified_memory: true,
  };
  const PLAN_CATALOG: PrimaryOption[] = [
    { profile: "very_high", recommended: true, file: "Qwen3.5-35B-A3B-Q4_K_M.gguf", base_name: "Qwen3.5-35B-A3B", family: "Qwen35", quant: "Q4_K_M", size_gb: 20.5, hf_url: "https://huggingface.co/unsloth/Qwen3.5-35B-A3B-GGUF", download_url: "" },
    { profile: "default", recommended: false, file: "gemma-4-12B-it-Q4_K_M.gguf", base_name: "gemma-4-12B", family: "Gemma4", quant: "Q4_K_M", size_gb: 7.38, hf_url: "https://huggingface.co/ggml-org/gemma-4-12B-it-GGUF", download_url: "" },
    { profile: "default", recommended: false, file: "Qwen3.5-9B-Q4_K_M.gguf", base_name: "Qwen3.5-9B", family: "Qwen35", quant: "Q4_K_M", size_gb: 5.9, hf_url: "https://huggingface.co/unsloth/Qwen3.5-9B-MTP-GGUF", download_url: "" },
  ];
  const PLAN_FAST: SlotConfig = { file: "Qwen3.5-2B-Q8_0.gguf", base_name: "Qwen3.5-2B", family: "Qwen35", quant: "Q8_0", size_gb: 1.9, hf_url: "https://huggingface.co/unsloth/Qwen3.5-2B-GGUF", download_url: "" };
  const PLAN_EMBED: SlotConfig = { file: "Qwen3-Embedding-4B-Q8_0.gguf", base_name: "Qwen3-Embedding-4B", family: "Qwen3Embedding", quant: "Q8_0", size_gb: 4.3, hf_url: "https://huggingface.co/Qwen/Qwen3-Embedding-4B-GGUF", download_url: "" };

  // Setup phases — message strings copied verbatim from setup_flow.rs so
  // the preview matches the live copy. Download fractions/ETAs are
  // illustrative; the failed messages are representative examples (real
  // ones come from the backend error).
  type PhaseFixture = {
    id: string;
    label: string;
    progress: Progress;
    failed: { message: string; recoverable: boolean } | null;
  };
  const base = (
    phase: Progress["phase"],
    message: string,
    extra: Partial<Progress> = {},
  ): Progress => ({
    phase,
    message,
    fraction: null,
    eta_seconds: null,
    indeterminate: true,
    ...extra,
  });
  const PHASES: PhaseFixture[] = [
    {
      id: "detect",
      label: "Detect HW",
      progress: base({ kind: "detecting_hardware" }, "Reading what this machine can do."),
      failed: null,
    },
    {
      id: "prep",
      label: "Prep storage",
      progress: base({ kind: "preparing_data_dir" }, "Preparing your storage."),
      failed: null,
    },
    {
      id: "dl_primary",
      label: "DL primary",
      progress: base(
        { kind: "downloading_primary", mb_total: 20480 },
        "Downloading the main responder.",
        { fraction: 0.42, eta_seconds: 185, indeterminate: false },
      ),
      failed: null,
    },
    {
      id: "dl_fast",
      label: "DL fast",
      progress: base({ kind: "downloading_fast" }, "Downloading the quick responder.", {
        fraction: 0.66,
        eta_seconds: 40,
        indeterminate: false,
      }),
      failed: null,
    },
    {
      id: "dl_embed",
      label: "DL embedder",
      progress: base({ kind: "downloading_embed" }, "Downloading the knowledge embedder.", {
        fraction: 0.9,
        eta_seconds: 8,
        indeterminate: false,
      }),
      failed: null,
    },
    {
      id: "db",
      label: "Open DB",
      progress: base({ kind: "opening_database" }, "Breaking ground on your library."),
      failed: null,
    },
    {
      id: "load",
      label: "Load model",
      progress: base({ kind: "loading_model" }, "Bringing a model online."),
      failed: null,
    },
    {
      id: "smoke",
      label: "Smoke test",
      progress: base({ kind: "smoke_testing" }, "Testing the connection."),
      failed: null,
    },
    {
      id: "fail_recoverable",
      label: "Failed (retry)",
      progress: base({ kind: "failed", recoverable: true }, ""),
      failed: { message: "Download interrupted — check your connection and try again.", recoverable: true },
    },
    {
      id: "fail_fatal",
      label: "Failed (fatal)",
      progress: base({ kind: "failed", recoverable: false }, ""),
      failed: {
        message: "Could not create ~/.svrnmesh/models: permission denied.",
        recoverable: false,
      },
    },
  ];
  let phaseIdx = $state(0);
  let phase = $derived(PHASES[phaseIdx]);
</script>

<div class="gallery">
  <aside class="nav">
    <div class="brand">Onboarding screens</div>
    <div class="sub">dev preview · no backend</div>
    {#each SCREENS as s (s.id)}
      <button class="nav-item" class:active={screen === s.id} onclick={() => (screen = s.id)}>
        {s.label}
      </button>
    {/each}
    <p class="hint">
      Actions are inert here — this is a copy/flow preview with no daemon or
      models. The setup phases are fixtures; the model list is the real
      <code>models.toml</code>.
    </p>
  </aside>

  <main class="stage">
    {#if screen === "setup"}
      <div class="phasebar">
        {#each PHASES as p, i (p.id)}
          <button class="chip" class:active={phaseIdx === i} onclick={() => (phaseIdx = i)}>
            {p.label}
          </button>
        {/each}
      </div>
    {/if}

    <div class="frame" class:scroll={screen === "models"}>
      {#if screen === "welcome"}
        <WelcomeThreshold onBegin={() => (screen = "setup_plan")} />
      {:else if screen === "setup_plan"}
        <SetupPlanView
          loading={false}
          loadError={null}
          profile={PLAN_PROFILE}
          catalog={PLAN_CATALOG}
          fast={PLAN_FAST}
          embed={PLAN_EMBED}
          modelsDir="~/.svrnmesh/models"
          dataDir="~/.svrnmesh"
          onConfirm={() => (screen = "setup")}
          onBack={() => (screen = "welcome")}
        />
      {:else if screen === "setup"}
        <SetupScreen progress={phase.progress} failed={phase.failed} onRetry={() => {}} />
      {:else if screen === "consent"}
        <ConsentGate onChoice={() => (screen = "models")} />
      {:else if screen === "models"}
        <div class="models">
          <h2>Recommended models</h2>
          <p class="models-note">
            Source of truth: <code>sovereign/models.toml</code> (the same file the
            Rust setup planner reads). The auto-setup picks each profile's
            <code>thoughtful</code> primary plus a <code>fast</code> and
            <code>embed</code> slot for your hardware tier.
          </p>
          <pre class="toml">{modelsToml}</pre>
        </div>
      {/if}
    </div>
  </main>
</div>

<style>
  .gallery {
    display: flex;
    height: 100vh;
    width: 100vw;
    font-family: var(--font-sans);
    color: var(--text-primary);
    background: var(--bg-primary);
  }
  .nav {
    flex: 0 0 220px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 18px 14px;
    border-right: 1px solid var(--border);
    background: var(--bg-surface);
    overflow-y: auto;
  }
  .brand {
    font-weight: 600;
    font-size: 0.92rem;
  }
  .sub {
    font-family: var(--font-mono);
    font-size: 0.66rem;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-muted);
    margin-bottom: 14px;
  }
  .nav-item {
    text-align: left;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: var(--text-secondary);
    padding: 8px 10px;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .nav-item:hover {
    color: var(--text-primary);
    border-color: var(--border-mid);
  }
  .nav-item.active {
    color: var(--text-primary);
    background: var(--bg-elevated);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }
  .hint {
    margin-top: auto;
    font-size: 0.72rem;
    line-height: 1.5;
    color: var(--text-muted);
  }
  .hint code,
  .models-note code {
    font-family: var(--font-mono);
    background: var(--bg-elevated);
    padding: 1px 4px;
    border-radius: 3px;
  }
  .stage {
    flex: 1 1 auto;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .phasebar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-surface);
  }
  .chip {
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: 999px;
    color: var(--text-secondary);
    padding: 4px 12px;
    font-size: 0.76rem;
    cursor: pointer;
  }
  .chip:hover {
    color: var(--text-primary);
    border-color: var(--accent);
  }
  .chip.active {
    color: var(--text-primary);
    background: var(--lavender-dim, color-mix(in srgb, var(--accent) 18%, transparent));
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  }
  .frame {
    flex: 1 1 auto;
    min-height: 0;
    position: relative;
  }
  .frame.scroll {
    overflow-y: auto;
  }
  .models {
    padding: 28px 32px;
    max-width: 860px;
  }
  .models h2 {
    margin: 0 0 8px;
    font-size: 1.1rem;
  }
  .models-note {
    font-size: 0.85rem;
    line-height: 1.55;
    color: var(--text-secondary);
    margin: 0 0 18px;
  }
  .toml {
    font-family: var(--font-mono);
    font-size: 0.76rem;
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-word;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px 18px;
    color: var(--text-secondary);
  }
</style>
