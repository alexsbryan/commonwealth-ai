<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  SetupPlanView — the PURE view for the consent-before-mutation Setup Plan.
  Takes the read-only plan data as props (so the dev screen gallery can render
  it with fixtures, no backend) and owns the interactive state (customize,
  starter-corpus opt-in). `SetupPlan.svelte` is the thin container that fetches
  the data and feeds it here. See SetupPlan for the full rationale.
-->
<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import BrandMark from "../components/BrandMark.svelte";
  import type { RecommendedProfile, PrimaryOption, SlotConfig } from "../types";
  import type { PrimarySource } from "./setupTypes";

  interface Props {
    loading: boolean;
    loadError: string | null;
    profile: RecommendedProfile | null;
    catalog: PrimaryOption[];
    fast: SlotConfig | null;
    embed: SlotConfig | null;
    modelsDir: string;
    dataDir: string;
    onConfirm: (opts: {
      installStarterCorpus: boolean;
      primaryFile?: string;
      primarySource?: PrimarySource;
    }) => void;
    onBack: () => void;
  }
  let {
    loading,
    loadError,
    profile,
    catalog,
    fast,
    embed,
    modelsDir,
    dataDir,
    onConfirm,
    onBack,
  }: Props = $props();

  let installStarterCorpus = $state(true);
  let customizing = $state(false);
  let chosenPrimaryFile = $state<string | undefined>(undefined);

  // "Advanced — bring your own": when `useByom` is on, `byomInput` (a local
  // .gguf path or a pasted .gguf URL) overrides the catalog pick entirely.
  let useByom = $state(false);
  let byomInput = $state("");

  const HTTP_RE = /^https?:\/\//i;
  let byomSource = $derived.by((): PrimarySource | null => {
    const v = byomInput.trim();
    if (!useByom || !v) return null;
    return HTTP_RE.test(v) ? { kind: "url", url: v } : { kind: "local_path", path: v };
  });
  // Non-blocking guidance; the backend does the authoritative GGUF check.
  let byomHint = $derived.by((): string | null => {
    if (!useByom) return null;
    const v = byomInput.trim();
    if (!v) return "Paste a .gguf link or pick a local file.";
    if (HTTP_RE.test(v)) {
      const ok = /\.gguf(\?|#|$)/i.test(v) || /show_file_info=[^&]*\.gguf/i.test(v);
      return ok
        ? null
        : "That doesn't look like a .gguf file link — paste the file (or its HF quant page), not the repo root.";
    }
    return /\.gguf$/i.test(v) ? null : "A local model should be a .gguf file.";
  });
  let byomReady = $derived(!useByom || (byomSource !== null && byomHint === null));

  async function browseLocal() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "GGUF Models", extensions: ["gguf"] }],
    });
    if (typeof selected === "string") {
      byomInput = selected;
      useByom = true;
    }
  }

  const PROFILE_LABEL: Record<string, string> = {
    cpu_only: "CPU-only",
    low_mem: "low-memory",
    default: "standard",
    high: "high-memory",
    very_high: "high-end",
  };

  let recommendedPrimary = $derived(
    catalog.find((o) => o.recommended) ?? catalog[0] ?? null,
  );
  let effectivePrimary = $derived(
    (chosenPrimaryFile
      ? catalog.find((o) => o.file === chosenPrimaryFile)
      : recommendedPrimary) ?? recommendedPrimary,
  );

  // What the "Main responder" ledger row shows: the BYOM pick when active,
  // else the catalog/recommended slot. BYOM has no known size up front, and
  // an empty `hf_url` signals the row to drop the "from repo → dir" line.
  let primaryDisplay = $derived.by(
    (): { base_name: string; file: string; quant: string; size_gb: number; hf_url: string } | null => {
      if (useByom && byomSource) {
        const name =
          byomSource.kind === "url"
            ? byomSource.url.split(/[?#]/)[0].split("/").pop() || "your model"
            : byomSource.path.split(/[/\\]/).pop() || "your model";
        return {
          base_name: name,
          file: name,
          quant: byomSource.kind === "url" ? "custom · link" : "custom · local",
          size_gb: 0,
          hf_url: "",
        };
      }
      return effectivePrimary;
    },
  );
  let primaryWhy = $derived(
    useByom && byomSource
      ? byomSource.kind === "url"
        ? "your model · downloaded from the link you gave"
        : "your model · used in place from your disk"
      : "research, long writing, deep analysis",
  );

  function fmtGb(n: number | undefined | null): string {
    if (!n || n <= 0) return "";
    return n < 1 ? `${Math.round(n * 1024)} MB` : `${n.toFixed(n < 10 ? 1 : 0)} GB`;
  }
  function repoOf(slot: { hf_url: string } | null): string {
    if (!slot?.hf_url) return "HuggingFace";
    return slot.hf_url
      .replace(/^https?:\/\/huggingface\.co\//, "")
      .replace(/\/$/, "");
  }

  let modelTotalGb = $derived(
    (primaryDisplay?.size_gb ?? 0) + (fast?.size_gb ?? 0) + (embed?.size_gb ?? 0),
  );

  function confirm() {
    // BYOM (Advanced) wins over the catalog radio; otherwise pass the
    // "Customize" filename (or nothing, for the hardware recommendation).
    onConfirm({
      installStarterCorpus,
      primaryFile: useByom ? undefined : chosenPrimaryFile,
      primarySource: useByom ? (byomSource ?? undefined) : undefined,
    });
  }
</script>

<div class="plan">
  <div class="plan-scroll">
    <div class="plan-body">
      <div class="mark"><BrandMark size={48} /></div>
      <h1>Setting up svrnmesh</h1>
      <p class="intro">
        Here's exactly what this will do. Nothing is downloaded or changed on
        your machine until you choose to proceed — and you can change all of it
        later.
      </p>

      <ul class="values" aria-label="How svrnmesh works with you">
        <li><b>Provenance</b> — where everything comes from and lives.</li>
        <li><b>Transparency</b> — every action named before it happens.</li>
        <li><b>Consent</b> — we ask first; you stay in control.</li>
      </ul>

      {#if loading}
        <p class="status">Reading what this machine can do…</p>
      {:else if loadError}
        <p class="status error">
          Couldn't read the setup plan ({loadError}). You can still proceed —
          setup will pick sensible defaults for your hardware.
        </p>
      {:else}
        <section class="card">
          <div class="card-label">On this machine</div>
          <p class="card-text">
            {#if profile}
              {profile.effective_memory_gb.toFixed(0)} GB
              {profile.is_unified_memory ? "unified memory" : "GPU / RAM"}
              · <span class="tier">{PROFILE_LABEL[profile.profile] ?? profile.profile}</span> tier
            {:else}
              hardware profile unavailable
            {/if}
          </p>
        </section>

        <section class="card">
          <div class="card-label">
            Models it will download <span class="chip">{fmtGb(modelTotalGb)}</span>
          </div>
          {#each [{ role: "Main responder", slot: primaryDisplay, why: primaryWhy }, { role: "Quick responder", slot: fast, why: "short turns, instant replies" }, { role: "Knowledge embedder", slot: embed, why: "makes your library searchable" }] as row (row.role)}
            {#if row.slot}
              <div class="model">
                <div class="model-head">
                  <span class="model-role">{row.role}</span>
                  <span class="model-name">{row.slot.base_name || row.slot.file}</span>
                  <span class="model-quant">{row.slot.quant}</span>
                  <span class="model-size">{fmtGb(row.slot.size_gb)}</span>
                </div>
                <div class="model-meta">
                  {#if row.slot.hf_url}
                    {row.why} · from <code>{repoOf(row.slot)}</code> → <code>{modelsDir}</code>
                  {:else}
                    {row.why}
                  {/if}
                </div>
              </div>
            {/if}
          {/each}

          {#if catalog.length >= 1}
            {#if !customizing}
              <button type="button" class="link-btn" onclick={() => (customizing = true)}>
                Customize the main model →
              </button>
            {:else}
              <div class="customize">
                <div class="customize-label">Choose your main model:</div>
                {#each catalog as opt (opt.file)}
                  <label class="choice">
                    <input
                      type="radio"
                      name="primary-model"
                      checked={!useByom && effectivePrimary?.file === opt.file}
                      onchange={() => {
                        chosenPrimaryFile = opt.file;
                        useByom = false;
                      }}
                    />
                    <span class="choice-name">{opt.base_name || opt.file}</span>
                    <span class="choice-quant">{opt.quant}</span>
                    <span class="choice-size">{fmtGb(opt.size_gb)}</span>
                    {#if opt.recommended}<span class="choice-rec">recommended</span>{/if}
                  </label>
                {/each}

                <label class="choice">
                  <input
                    type="radio"
                    name="primary-model"
                    checked={useByom}
                    onchange={() => (useByom = true)}
                  />
                  <span class="choice-name">Bring your own model</span>
                  <span class="choice-quant">advanced</span>
                </label>
                {#if useByom}
                  <div class="byom">
                    <div class="byom-row">
                      <input
                        class="byom-input"
                        type="text"
                        placeholder="Paste a .gguf link, or a local file path"
                        bind:value={byomInput}
                        oninput={() => (useByom = true)}
                        spellcheck="false"
                        autocomplete="off"
                      />
                      <button type="button" class="byom-browse" onclick={browseLocal}>
                        Browse…
                      </button>
                    </div>
                    <div class="byom-hint" class:warn={byomHint !== null}>
                      {#if byomHint}
                        {byomHint}
                      {:else if byomSource?.kind === "url"}
                        Will download your model during setup.
                      {:else if byomSource?.kind === "local_path"}
                        Will use this file in place — no download, no copy.
                      {:else}
                        A HuggingFace resolve/blob link, an HF quant page URL, or a
                        local .gguf path. Fast + embedder still auto-pick for your
                        hardware.
                      {/if}
                    </div>
                  </div>
                {/if}

                <button type="button" class="link-btn" onclick={() => (customizing = false)}>
                  Done
                </button>
              </div>
            {/if}
          {/if}
        </section>

        <section class="card">
          <div class="card-label">Starter knowledge</div>
          <label class="opt">
            <input type="checkbox" bind:checked={installStarterCorpus} />
            <span>
              <b>Install Wikipedia Core</b> so you can ask grounded questions right
              away. Downloads in the background after setup — uncheck to start with
              an empty library.
            </span>
          </label>
        </section>

        <section class="card subtle">
          <div class="card-label">Where it lives</div>
          <p class="card-text mono">{dataDir}</p>
          <p class="card-meta">
            Models, your library, and config stay on this machine. Change models
            in Settings → Models, knowledge in Settings → Knowledge, and mesh
            sharing in Settings → Mesh — any time.
          </p>
        </section>
      {/if}
    </div>
  </div>

  <footer class="plan-actions">
    <button type="button" class="btn-back" onclick={onBack}>← Back</button>
    <button
      type="button"
      class="btn-go"
      onclick={confirm}
      disabled={loading || !byomReady}
    >
      Set up svrnmesh
    </button>
  </footer>
</div>

<style>
  .plan {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: var(--bg-primary);
    color: var(--text-primary);
    font-family: var(--font-sans);
  }
  .plan-scroll {
    flex: 1 1 auto;
    overflow-y: auto;
    display: flex;
    justify-content: center;
  }
  .plan-body {
    width: 100%;
    max-width: 560px;
    padding: 36px 32px 16px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 12px;
  }
  .mark {
    margin-bottom: 4px;
  }
  h1 {
    font-size: 1.4rem;
    font-weight: 600;
    margin: 0;
  }
  .intro {
    font-size: 0.95rem;
    line-height: 1.55;
    color: var(--text-secondary);
    margin: 0;
  }
  .values {
    list-style: none;
    padding: 12px 14px;
    margin: 6px 0 4px;
    display: flex;
    flex-direction: column;
    gap: 5px;
    border: 1px solid color-mix(in srgb, var(--accent) 28%, transparent);
    background: var(--accent-glow, color-mix(in srgb, var(--accent) 7%, transparent));
    border-radius: var(--radius-lg, 8px);
    border-left-width: 3px;
    width: 100%;
    box-sizing: border-box;
    font-size: 0.86rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .values b {
    color: var(--accent-light, #dfc068);
    font-weight: 600;
  }
  .status {
    font-size: 0.9rem;
    color: var(--text-muted);
    margin: 6px 0;
  }
  .status.error {
    color: var(--warning, var(--accent));
  }
  .card {
    width: 100%;
    box-sizing: border-box;
    border: 1px solid var(--border);
    border-radius: var(--radius, 6px);
    background: var(--bg-elevated, transparent);
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .card.subtle {
    background: transparent;
  }
  .card-label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.1em;
    font-weight: 600;
    color: var(--text-muted);
  }
  .chip {
    text-transform: none;
    letter-spacing: 0;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    color: var(--text-secondary);
    background: var(--bg, #15171c);
    border: 1px solid var(--border-mid);
    border-radius: 999px;
    padding: 1px 8px;
  }
  .card-text {
    margin: 0;
    font-size: 0.92rem;
    line-height: 1.5;
  }
  .card-text.mono {
    font-family: var(--font-mono);
    font-size: 0.8rem;
    color: var(--text-secondary);
    word-break: break-all;
  }
  .card-meta {
    margin: 0;
    font-size: 0.8rem;
    line-height: 1.5;
    color: var(--text-muted);
  }
  .tier {
    color: var(--accent-light, #dfc068);
    font-weight: 600;
  }
  .model {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 7px 0;
    border-top: 1px solid var(--border);
  }
  .model:first-of-type {
    border-top: none;
    padding-top: 0;
  }
  .model-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px;
  }
  .model-role {
    font-size: 0.74rem;
    color: var(--text-muted);
    min-width: 9.5rem;
  }
  .model-name {
    font-weight: 600;
    font-size: 0.9rem;
  }
  .model-quant {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .model-size {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: var(--text-secondary);
  }
  .model-meta {
    font-size: 0.78rem;
    line-height: 1.5;
    color: var(--text-muted);
  }
  .model-meta code {
    font-family: var(--font-mono);
    font-size: 0.92em;
    color: var(--text-secondary);
    word-break: break-all;
  }
  .link-btn {
    align-self: flex-start;
    background: none;
    border: none;
    padding: 4px 0 0;
    color: var(--accent-light, #dfc068);
    font-family: var(--font-sans);
    font-size: 0.8rem;
    cursor: pointer;
  }
  .link-btn:hover {
    text-decoration: underline;
  }
  .customize {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 6px;
    border-top: 1px solid var(--border);
  }
  .customize-label {
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .choice {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 0.86rem;
    cursor: pointer;
  }
  .choice input {
    accent-color: var(--accent);
  }
  .choice-name {
    font-weight: 500;
  }
  .choice-quant {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .choice-size {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 0.76rem;
    color: var(--text-secondary);
  }
  .choice-rec {
    font-size: 0.64rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--accent-light, #dfc068);
    border: 1px solid color-mix(in srgb, var(--accent) 40%, transparent);
    border-radius: 999px;
    padding: 0 6px;
  }
  .byom {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 2px 0 2px 22px;
  }
  .byom-row {
    display: flex;
    gap: 6px;
  }
  .byom-input {
    flex: 1 1 auto;
    min-width: 0;
    font-family: var(--font-mono);
    font-size: 0.76rem;
    padding: 6px 8px;
    color: var(--text-primary);
    background: var(--bg, #15171c);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius, 6px);
  }
  .byom-input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--accent) 55%, transparent);
  }
  .byom-browse {
    flex: 0 0 auto;
    font-family: var(--font-sans);
    font-size: 0.78rem;
    padding: 6px 12px;
    color: var(--text-secondary);
    background: transparent;
    border: 1px solid var(--border-mid);
    border-radius: var(--radius, 6px);
    cursor: pointer;
  }
  .byom-browse:hover {
    border-color: var(--accent);
    color: var(--text-primary);
  }
  .byom-hint {
    font-size: 0.74rem;
    line-height: 1.45;
    color: var(--text-muted);
  }
  .byom-hint.warn {
    color: var(--warning, var(--accent-light, #dfc068));
  }
  .opt {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    font-size: 0.88rem;
    line-height: 1.5;
    color: var(--text-secondary);
    cursor: pointer;
  }
  .opt input {
    margin-top: 3px;
    accent-color: var(--accent);
  }
  .opt b {
    color: var(--text-primary);
  }
  .plan-actions {
    flex: 0 0 auto;
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 14px 32px;
    border-top: 1px solid var(--border);
    background: var(--bg-primary);
  }
  .btn-back,
  .btn-go {
    font-family: var(--font-sans);
    font-size: 0.88rem;
    padding: 9px 22px;
    border-radius: var(--radius);
    cursor: pointer;
    border: 1px solid var(--border-mid);
    background: transparent;
    color: var(--text-secondary);
  }
  .btn-back:hover {
    border-color: var(--accent);
    color: var(--text-primary);
  }
  .btn-go {
    background: var(--lavender-dim, color-mix(in srgb, var(--accent) 20%, transparent));
    border-color: color-mix(in srgb, var(--accent) 50%, transparent);
    color: var(--text-primary);
    font-weight: 500;
  }
  .btn-go:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 30%, transparent);
  }
  .btn-go:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
</style>
