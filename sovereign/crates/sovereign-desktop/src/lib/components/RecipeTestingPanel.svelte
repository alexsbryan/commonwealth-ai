<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { recipeValidate, recipeTest } from "../api";
  import type { RecipeValidateResult, RecipeTestResult } from "../types";

  // ── State ─────────────────────────────────────────────────────────────────

  let recipePath = $state("");
  let sampleSize = $state(100);
  let offline = $state(false);

  let validating = $state(false);
  let testing = $state(false);

  let validateResult = $state<RecipeValidateResult | null>(null);
  let testResult = $state<RecipeTestResult | null>(null);
  let error = $state<string | null>(null);

  let showReport = $state(false);

  let busy = $derived(validating || testing);

  // ── Actions ───────────────────────────────────────────────────────────────

  async function browse() {
    const selected = await open({
      multiple: false,
      filters: [{ name: "TOML Recipe", extensions: ["toml"] }],
    });
    if (selected && typeof selected === "string") {
      recipePath = selected;
      validateResult = null;
      testResult = null;
      error = null;
      showReport = false;
    }
  }

  async function handleValidate() {
    if (!recipePath.trim() || busy) return;
    validating = true;
    validateResult = null;
    testResult = null;
    error = null;
    showReport = false;
    try {
      validateResult = await recipeValidate(recipePath, offline);
    } catch (e) {
      error = String(e);
    }
    validating = false;
  }

  async function handleTest() {
    if (!recipePath.trim() || busy) return;
    testing = true;
    validateResult = null;
    testResult = null;
    error = null;
    showReport = false;
    try {
      testResult = await recipeTest(recipePath, sampleSize, offline);
    } catch (e) {
      error = String(e);
    }
    testing = false;
  }

  function clearAll() {
    recipePath = "";
    validateResult = null;
    testResult = null;
    error = null;
    showReport = false;
  }

  // ── Helpers ───────────────────────────────────────────────────────────────

  function fileName(path: string): string {
    return path.split(/[\\/]/).pop() ?? path;
  }

  function pct(rate: number): string {
    return (rate * 100).toFixed(1) + "%";
  }

  async function copyPath(path: string) {
    try {
      await navigator.clipboard.writeText(path);
    } catch {}
  }

  function isTestResult(
    r: RecipeValidateResult | RecipeTestResult,
  ): r is RecipeTestResult {
    return "recipe_id" in r;
  }

  function displayNameOf(
    r: RecipeValidateResult | RecipeTestResult,
  ): string {
    return isTestResult(r)
      ? (r.recipe_name || r.recipe_id)
      : (r.corpus_name || r.corpus_id);
  }

  let activeResult = $derived(testResult ?? validateResult);
  let passed = $derived(activeResult?.passed ?? false);
  let hasErrors = $derived((activeResult?.errors?.length ?? 0) > 0);
  let hasWarnings = $derived((activeResult?.warnings?.length ?? 0) > 0);
  let displayName = $derived(activeResult ? displayNameOf(activeResult) : "");
</script>

<!-- ── File picker row ─────────────────────────────────────────────────────── -->
<p class="tab-intro">
  Point at a <code>recipe.toml</code> to validate its fields or run the full test harness. The harness downloads a small sample, runs extract → chunk, and writes a <code>TEST_REPORT.md</code> to the recipe's directory.
</p>

<div class="picker-row">
  <input
    class="path-input"
    type="text"
    bind:value={recipePath}
    placeholder="No recipe selected"
    readonly
  />
  <button class="btn-browse" onclick={browse} disabled={busy}>
    Browse…
  </button>
  {#if recipePath}
    <button class="btn-clear" onclick={clearAll} disabled={busy} aria-label="Clear">
      <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true">
        <path d="M1 1l10 10M11 1L1 11" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>
      </svg>
    </button>
  {/if}
</div>

<!-- ── Options ─────────────────────────────────────────────────────────────── -->
<div class="options-row">
  <label class="option-label">
    <span>Sample size</span>
    <input
      class="num-input"
      type="number"
      min="1"
      max="1000"
      bind:value={sampleSize}
      disabled={busy}
    />
    <span class="option-hint">records</span>
  </label>

  <label class="option-label option-label--check">
    <input type="checkbox" bind:checked={offline} disabled={busy} />
    <span>Offline</span>
    <span class="option-hint">(skip URL check)</span>
  </label>
</div>

<!-- ── Action buttons ──────────────────────────────────────────────────────── -->
<div class="action-row">
  <button
    class="btn-action btn-validate"
    onclick={handleValidate}
    disabled={!recipePath.trim() || busy}
  >
    {#if validating}
      <span class="spinner" aria-hidden="true"></span>
      Validating…
    {:else}
      <svg width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
        <circle cx="6.5" cy="6.5" r="5.5" stroke="currentColor" stroke-width="1.3"/>
        <path d="M4 6.5l2 2 3-3.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
      </svg>
      Validate
    {/if}
  </button>

  <button
    class="btn-action btn-test"
    onclick={handleTest}
    disabled={!recipePath.trim() || busy}
  >
    {#if testing}
      <span class="spinner" aria-hidden="true"></span>
      Testing… (may take a minute)
    {:else}
      <svg width="13" height="13" viewBox="0 0 13 13" fill="none" aria-hidden="true">
        <path d="M4.5 1.5v4L1.5 11h10L8.5 5.5V1.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/>
        <path d="M4 1.5h5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>
      </svg>
      Run Test
    {/if}
  </button>
</div>

<!-- ── Error ───────────────────────────────────────────────────────────────── -->
{#if error}
  <div class="result-card result-card--fail">
    <div class="result-head">
      <span class="status-badge status-badge--fail">Error</span>
    </div>
    <p class="error-text">{error}</p>
  </div>
{/if}

<!-- ── Result ──────────────────────────────────────────────────────────────── -->
{#if activeResult}
  <div class="result-card" class:result-card--pass={passed} class:result-card--fail={!passed}>

    <!-- Status row -->
    <div class="result-head">
      {#if passed}
        <span class="status-badge status-badge--pass">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true">
            <path d="M2 5l2.5 2.5L8 3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
          PASS
        </span>
      {:else}
        <span class="status-badge status-badge--fail">
          <svg width="10" height="10" viewBox="0 0 10 10" fill="none" aria-hidden="true">
            <path d="M2 2l6 6M8 2L2 8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
          FAIL
        </span>
      {/if}
      <span class="result-name">{displayName}</span>
    </div>

    <!-- Errors -->
    {#if hasErrors}
      <div class="result-section">
        <p class="result-section-label">Errors</p>
        <ul class="issue-list issue-list--error">
          {#each activeResult.errors as err}
            <li>{err}</li>
          {/each}
        </ul>
      </div>
    {/if}

    <!-- Warnings -->
    {#if hasWarnings}
      <div class="result-section">
        <p class="result-section-label">Warnings</p>
        <ul class="issue-list issue-list--warn">
          {#each activeResult.warnings as w}
            <li>{w}</li>
          {/each}
        </ul>
      </div>
    {/if}

    <!-- Test metrics (only shown after full test) -->
    {#if testResult}
      <div class="result-section">
        <p class="result-section-label">Sample metrics</p>
        <div class="metrics-grid">
          <div class="metric">
            <span class="metric-value">{testResult.records_succeeded}<span class="metric-of">/{testResult.records_attempted}</span></span>
            <span class="metric-label">records extracted</span>
          </div>
          <div class="metric">
            <span class="metric-value">{pct(testResult.extraction_rate)}</span>
            <span class="metric-label">extraction rate</span>
          </div>
          <div class="metric">
            <span class="metric-value">{testResult.total_chunks}</span>
            <span class="metric-label">chunks produced</span>
          </div>
          <div class="metric">
            <span class="metric-value">{Math.round(testResult.avg_chars)}</span>
            <span class="metric-label">avg chars/chunk</span>
          </div>
        </div>
      </div>

      <!-- Report path -->
      <div class="result-section">
        <p class="result-section-label">Report written to</p>
        <div class="report-path-row">
          <code class="report-path">{testResult.report_path}</code>
          <button
            class="btn-copy"
            onclick={() => copyPath(testResult!.report_path)}
            title="Copy path"
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none" aria-hidden="true">
              <rect x="4" y="4" width="7" height="7" rx="1" stroke="currentColor" stroke-width="1.2"/>
              <path d="M8 4V2a1 1 0 00-1-1H2a1 1 0 00-1 1v5a1 1 0 001 1h2" stroke="currentColor" stroke-width="1.2"/>
            </svg>
          </button>
        </div>
      </div>

      <!-- Report preview toggle -->
      <button
        class="toggle-report"
        onclick={() => showReport = !showReport}
      >
        {showReport ? "Hide" : "Show"} report preview
        <svg
          width="10" height="10" viewBox="0 0 10 10" fill="none"
          style="transform: rotate({showReport ? 180 : 0}deg); transition: transform 0.15s;"
          aria-hidden="true"
        >
          <path d="M2 3.5l3 3 3-3" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
      </button>

      {#if showReport}
        <div class="report-preview">
          <pre class="report-text">{testResult.report_markdown}</pre>
        </div>
      {/if}
    {/if}

  </div>
{/if}

<style>
  .tab-intro {
    font-size: 0.82rem;
    color: var(--text-muted);
    line-height: 1.5;
    margin-bottom: 16px;
  }

  .tab-intro code {
    font-family: monospace;
    background: var(--bg-surface);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.8em;
    color: var(--text-secondary);
  }

  /* ── File picker ── */
  .picker-row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 12px;
  }

  .path-input {
    flex: 1;
    min-width: 0;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 0.8rem;
    padding: 6px 10px;
    cursor: default;
  }

  .path-input::placeholder {
    color: var(--text-muted);
  }

  .btn-browse {
    flex-shrink: 0;
    padding: 6px 12px;
    font-size: 0.8rem;
    background: var(--bg-surface);
    border: 1px dashed var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
    cursor: pointer;
    white-space: nowrap;
    transition: border-color 0.12s, color 0.12s;
  }

  .btn-browse:hover:not(:disabled) {
    border-color: var(--accent);
    color: var(--accent);
  }

  .btn-browse:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .btn-clear {
    flex-shrink: 0;
    padding: 6px;
    color: var(--text-muted);
    border-radius: var(--radius);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .btn-clear:hover:not(:disabled) {
    color: var(--error);
    background: color-mix(in srgb, var(--error) 12%, transparent);
  }

  /* ── Options ── */
  .options-row {
    display: flex;
    align-items: center;
    gap: 20px;
    margin-bottom: 16px;
    flex-wrap: wrap;
  }

  .option-label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.8rem;
    color: var(--text-secondary);
  }

  .num-input {
    width: 64px;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 0.8rem;
    padding: 4px 8px;
    text-align: center;
  }

  .num-input:disabled {
    opacity: 0.5;
  }

  .option-label--check {
    cursor: pointer;
  }

  .option-hint {
    color: var(--text-muted);
    font-size: 0.75rem;
  }

  /* ── Action buttons ── */
  .action-row {
    display: flex;
    gap: 8px;
    margin-bottom: 20px;
    flex-wrap: wrap;
  }

  .btn-action {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 7px 16px;
    font-size: 0.82rem;
    font-weight: 500;
    border-radius: var(--radius);
    cursor: pointer;
    transition: background 0.12s, opacity 0.12s;
  }

  .btn-action:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .btn-validate {
    background: var(--bg-surface);
    border: 1px solid var(--border);
    color: var(--text-secondary);
  }

  .btn-validate:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 10%, var(--bg-surface));
    border-color: var(--accent);
    color: var(--accent);
  }

  .btn-test {
    background: var(--accent);
    border: 1px solid transparent;
    color: #fff;
  }

  .btn-test:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 85%, #000);
  }

  /* ── Spinner ── */
  .spinner {
    display: inline-block;
    width: 11px;
    height: 11px;
    border: 1.5px solid currentColor;
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 0.65s linear infinite;
    flex-shrink: 0;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  /* ── Result card ── */
  .result-card {
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    overflow: hidden;
  }

  .result-card--pass {
    border-color: color-mix(in srgb, var(--success) 40%, var(--border));
    background: color-mix(in srgb, var(--success) 5%, var(--bg-surface));
  }

  .result-card--fail {
    border-color: color-mix(in srgb, var(--error) 40%, var(--border));
    background: color-mix(in srgb, var(--error) 5%, var(--bg-surface));
  }

  .result-head {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    border-bottom: 1px solid var(--border);
  }

  .result-name {
    font-size: 0.82rem;
    font-weight: 500;
    color: var(--text-primary);
  }

  /* ── Status badges ── */
  .status-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 3px 8px;
    border-radius: 999px;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.05em;
    flex-shrink: 0;
  }

  .status-badge--pass {
    background: color-mix(in srgb, var(--success) 18%, transparent);
    color: var(--success);
    border: 1px solid color-mix(in srgb, var(--success) 35%, transparent);
  }

  .status-badge--fail {
    background: color-mix(in srgb, var(--error) 15%, transparent);
    color: var(--error);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
  }

  /* ── Result sections ── */
  .result-section {
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
  }

  .result-section:last-child {
    border-bottom: none;
  }

  .result-section-label {
    font-size: 0.68rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-muted);
    margin-bottom: 6px;
  }

  /* ── Issue lists ── */
  .issue-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .issue-list li {
    font-size: 0.8rem;
    line-height: 1.45;
    padding-left: 14px;
    position: relative;
  }

  .issue-list li::before {
    content: "×";
    position: absolute;
    left: 0;
    font-weight: 700;
  }

  .issue-list--error li {
    color: var(--error);
  }

  .issue-list--error li::before {
    color: var(--error);
  }

  .issue-list--warn li {
    color: color-mix(in srgb, var(--accent) 85%, var(--text-secondary));
  }

  .issue-list--warn li::before {
    content: "⚠";
    font-size: 0.72em;
  }

  /* ── Metrics ── */
  .metrics-grid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
  }

  @media (max-width: 420px) {
    .metrics-grid { grid-template-columns: repeat(2, 1fr); }
  }

  .metric {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    background: var(--bg-primary);
    border-radius: var(--radius);
    padding: 8px 10px;
  }

  .metric-value {
    font-size: 1.05rem;
    font-weight: 600;
    color: var(--text-primary);
    font-variant-numeric: tabular-nums;
  }

  .metric-of {
    font-size: 0.75rem;
    color: var(--text-muted);
    font-weight: 400;
  }

  .metric-label {
    font-size: 0.7rem;
    color: var(--text-muted);
  }

  /* ── Report path ── */
  .report-path-row {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--bg-primary);
    border-radius: var(--radius);
    padding: 6px 10px;
  }

  .report-path {
    flex: 1;
    min-width: 0;
    font-size: 0.75rem;
    font-family: monospace;
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .btn-copy {
    flex-shrink: 0;
    color: var(--text-muted);
    padding: 4px;
    border-radius: var(--radius);
    display: flex;
    align-items: center;
  }

  .btn-copy:hover {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }

  /* ── Report preview toggle ── */
  .toggle-report {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 9px 14px;
    font-size: 0.79rem;
    color: var(--text-secondary);
    background: none;
    border: none;
    border-top: 1px solid var(--border);
    cursor: pointer;
    text-align: left;
    transition: color 0.12s;
  }

  .toggle-report:hover {
    color: var(--accent);
  }

  /* ── Report text ── */
  .report-preview {
    border-top: 1px solid var(--border);
    max-height: 320px;
    overflow-y: auto;
  }

  .report-text {
    margin: 0;
    padding: 12px 14px;
    font-family: monospace;
    font-size: 0.72rem;
    line-height: 1.55;
    color: var(--text-secondary);
    white-space: pre-wrap;
    word-break: break-word;
  }

  .error-text {
    font-size: 0.8rem;
    color: var(--error);
    margin: 12px 14px;
  }
</style>
