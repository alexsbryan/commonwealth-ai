<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  The install-time form for a recipe that declares `[parameters]`.

  RENDERED FROM THE SCHEMA, never hand-coded per recipe. `kind` exists
  precisely to drive the affordance (SCHEMA.md:50), so a recipe that
  declares a different parameter set gets a correct form with no new
  code here. Hand-coding a ticker input would put authored, recipe-
  specific content in a shared layer — the failure FINANCIAL_CORPORA
  §7.4 forbids one layer up, in exactly the same shape.

  A parameter carrying a `default` stays VISIBLE and EDITABLE rather
  than being sent invisibly: `sec-filings-company`'s `contact` is the
  address SEC is told to reach the user at, and the recipe declared it
  a parameter instead of a hidden constant so the user can see and
  replace it.

  Absence is reported, never defaulted (ARCH §18.3): a schema that
  cannot be read renders the error, and Install stays unavailable —
  it never falls back to a parameterless install that would reach the
  acquirer with an un-interpolated `{ticker}`.
-->
<script lang="ts">
  import {
    corpusGetRecipeParameters,
    corpusInstallWithParameters,
    type RecipeParameter,
  } from "../../api";

  let {
    corpusId,
    corpusName,
    onInstalled,
    onCancel,
  }: {
    corpusId: string;
    corpusName: string;
    /** Fired after the daemon ACCEPTS the install, so the caller can
     *  flip its row to "installing". Progress then arrives on the
     *  shared `corpus-progress` event like any other install. */
    onInstalled: () => void;
    onCancel: () => void;
  } = $props();

  let parameters = $state<RecipeParameter[] | null>(null);
  /// Always STRINGS, whatever the control's `type` is. `bind:value` on
  /// a `type="number"` input hands back a `number` (or `undefined` while
  /// the field is mid-edit), which would make every `.trim()` below a
  /// runtime type error on the `int` kind. The control is therefore
  /// explicit rather than bound, and `typedValue` does the ONE
  /// conversion, at submit, per the parameter's declared kind.
  let values = $state<Record<string, string>>({});
  let error = $state<string | null>(null);
  let loading = $state(true);
  let submitting = $state(false);

  /** `default` is `unknown` on the wire; render it as the text the
   *  control shows, and an absent default as an empty field. */
  function initialText(p: RecipeParameter): string {
    if (p.default === null || p.default === undefined) return "";
    if (Array.isArray(p.default)) return p.default.join(", ");
    return String(p.default);
  }

  $effect(() => {
    const cid = corpusId;
    loading = true;
    error = null;
    corpusGetRecipeParameters(cid)
      .then((schema) => {
        if (cid !== corpusId) return; // a newer request owns the form
        parameters = schema.parameters;
        const next: Record<string, string> = {};
        for (const p of schema.parameters) next[p.name] = initialText(p);
        values = next;
        loading = false;
      })
      .catch((e) => {
        if (cid !== corpusId) return;
        parameters = null;
        error = `Could not read this recipe's parameters: ${String(e)}`;
        loading = false;
      });
  });

  /** Every required parameter must carry a non-blank value. A blank
   *  optional one is OMITTED rather than sent as "", so the recipe's
   *  own default applies instead of an empty override. */
  let missing = $derived(
    (parameters ?? [])
      .filter((p) => p.required && (values[p.name] ?? "").trim() === "")
      .map((p) => p.name),
  );
  let canInstall = $derived(
    !loading && !submitting && parameters !== null && missing.length === 0,
  );

  /** Text control → the JSON type the daemon's `json_params_to_toml`
   *  accepts for this `kind`. An `int` that does not parse is a
   *  refusal here, not a silently-coerced 0. */
  function typedValue(p: RecipeParameter, raw: string): string | number | string[] {
    const text = raw.trim();
    if (p.kind === "list") {
      return text
        .split(",")
        .map((s) => s.trim())
        .filter((s) => s.length > 0);
    }
    if (p.kind === "int") {
      const n = Number(text);
      if (!Number.isInteger(n)) {
        throw new Error(`${p.name} must be a whole number, got "${text}"`);
      }
      return n;
    }
    return text;
  }

  async function submit() {
    if (!canInstall || parameters === null) return;
    submitting = true;
    error = null;
    try {
      const payload: Record<string, string | number | string[]> = {};
      for (const p of parameters) {
        const raw = values[p.name] ?? "";
        if (raw.trim() === "") continue; // let the recipe's default stand
        payload[p.name] = typedValue(p, raw);
      }
      await corpusInstallWithParameters(corpusId, payload);
      onInstalled();
    } catch (e) {
      // The daemon's 400 for invalid parameters lands here, and it
      // names the parameter. Shown, not swallowed to console.
      error = String(e);
    } finally {
      submitting = false;
    }
  }

  function controlType(kind: RecipeParameter["kind"]): string {
    if (kind === "int") return "number";
    if (kind === "date") return "date";
    return "text";
  }
</script>

<div class="param-form" data-testid="param-form" data-corpus-id={corpusId}>
  <div class="pf-head">
    <span class="pf-title">Install {corpusName}</span>
    <button class="pf-close" data-testid="param-form-cancel" onclick={onCancel}>
      Cancel
    </button>
  </div>

  {#if loading}
    <p class="pf-loading">Reading this recipe's parameters…</p>
  {:else if parameters === null}
    <p class="pf-error" data-testid="param-form-error">{error}</p>
  {:else if parameters.length === 0}
    <!-- Reported, not silently converted into a plain install: the
         caller only opens this form for a recipe that declared
         parameters, so an empty schema means the two disagree. -->
    <p class="pf-error" data-testid="param-form-error">
      This recipe declares no parameters. Nothing was installed.
    </p>
  {:else}
    {#each parameters as p (p.name)}
      <label class="pf-field" for={`param-${p.name}`}>
        <span class="pf-label">
          {p.name}
          {#if p.required}<span class="pf-req" title="Required">*</span>{/if}
          <span class="pf-kind">{p.kind}</span>
        </span>
        <input
          id={`param-${p.name}`}
          data-testid={`param-${p.name}`}
          data-kind={p.kind}
          type={controlType(p.kind)}
          placeholder={p.kind === "list" ? "comma-separated" : ""}
          value={values[p.name] ?? ""}
          oninput={(e) => (values[p.name] = e.currentTarget.value)}
        />
        <span class="pf-desc">{p.description}</span>
      </label>
    {/each}

    {#if error}
      <p class="pf-error" data-testid="param-form-error">{error}</p>
    {/if}

    <div class="pf-actions">
      <button
        class="pf-install"
        data-testid="param-form-install"
        disabled={!canInstall}
        title={missing.length > 0 ? `Needs: ${missing.join(", ")}` : ""}
        onclick={submit}
      >
        {submitting ? "Installing…" : "Install"}
      </button>
      {#if missing.length > 0}
        <span class="pf-missing" data-testid="param-form-missing">
          Needs: {missing.join(", ")}
        </span>
      {/if}
    </div>
  {/if}
</div>

<style>
  .param-form {
    border: 1px solid var(--border, #333);
    border-radius: 8px;
    padding: 0.85rem 1rem;
    margin: 0.5rem 0;
    background: var(--bg-elevated, rgba(255, 255, 255, 0.03));
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
  }
  .pf-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .pf-title {
    font-weight: 600;
  }
  .pf-close {
    background: none;
    border: none;
    color: var(--text-dim, #999);
    cursor: pointer;
    font-size: 0.85rem;
  }
  .pf-field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .pf-label {
    font-size: 0.85rem;
    font-weight: 500;
    display: flex;
    align-items: baseline;
    gap: 0.4rem;
  }
  .pf-req {
    color: var(--accent-warn, #e0a030);
  }
  .pf-kind {
    font-size: 0.7rem;
    color: var(--text-dim, #999);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .pf-field input {
    padding: 0.4rem 0.55rem;
    border-radius: 6px;
    border: 1px solid var(--border, #333);
    background: var(--bg-input, rgba(0, 0, 0, 0.2));
    color: inherit;
    font: inherit;
  }
  .pf-desc {
    font-size: 0.75rem;
    color: var(--text-dim, #999);
    line-height: 1.35;
  }
  .pf-actions {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  .pf-install {
    padding: 0.4rem 0.9rem;
    border-radius: 6px;
    border: 1px solid var(--border, #333);
    cursor: pointer;
  }
  .pf-install:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .pf-missing,
  .pf-loading {
    font-size: 0.75rem;
    color: var(--text-dim, #999);
  }
  .pf-error {
    font-size: 0.8rem;
    color: var(--accent-error, #e06060);
    margin: 0;
  }
</style>
