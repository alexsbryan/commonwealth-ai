<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { addHostConnection } from "../api";

  let { onpaired }: { onpaired: () => void } = $props();

  let displayName = $state("");
  let tailnetAddress = $state("");
  let tenantId = $state("");
  let token = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function pair() {
    busy = true;
    error = null;
    try {
      await addHostConnection(displayName, tailnetAddress, tenantId, token);
      onpaired();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="pair">
  <header class="masthead">
    <div class="crest" aria-hidden="true">◈</div>
    <h1>Connect to your host</h1>
    <p class="hint">Reachable over your tailnet only. The token is stored in the device keychain.</p>
  </header>

  <div class="fields">
    <label><span>Name</span><input bind:value={displayName} placeholder="mac-peer" /></label>
    <label>
      <span>Tailnet address</span>
      <input bind:value={tailnetAddress} placeholder="beefymac.tailXXXX.ts.net:8080"
             autocapitalize="off" autocorrect="off" spellcheck="false" />
    </label>
    <label><span>Tenant</span><input bind:value={tenantId} placeholder="alex" autocapitalize="off" autocorrect="off" /></label>
    <label><span>Token</span><input bind:value={token} type="password" autocapitalize="off" autocorrect="off" /></label>
  </div>

  {#if error}<p class="err">{error}</p>{/if}

  <button class="connect" onclick={pair} disabled={busy || !tailnetAddress || !token}>
    {busy ? "Connecting…" : "Connect"}
  </button>
</div>

<style>
  .pair {
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
    padding: 2rem var(--pad-r) calc(2rem + env(safe-area-inset-bottom)) var(--pad-l);
    min-height: 100%;
    justify-content: center;
    /* A form is most legible narrow — cap it and centre so the inputs
       don't span a whole tablet. */
    width: 100%;
    max-width: 26rem;
    margin-inline: auto;
  }
  .masthead {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .crest {
    font-size: 1.5rem;
    line-height: 1;
    color: var(--lavender);
    text-shadow: 0 0 18px var(--lavender-glow);
  }
  h1 {
    font-family: var(--font-sans);
    font-size: 1.7rem;
    font-weight: 600;
    letter-spacing: -0.02em;
    color: var(--text-primary);
  }
  .hint {
    color: var(--text-secondary);
    font-size: 0.86rem;
    line-height: 1.5;
    max-width: 32ch;
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }
  label span {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    font-weight: 500;
    color: var(--text-muted);
  }
  input {
    background: var(--bg-input);
    border: 1px solid var(--border-mid);
    border-radius: var(--radius);
    padding: 0.72rem 0.8rem;
    color: var(--text-primary);
    font-size: 0.95rem;
    transition: border-color 0.15s, background 0.15s;
  }
  input::placeholder { color: var(--text-muted); }
  input:focus {
    outline: none;
    border-color: color-mix(in srgb, var(--lavender) 55%, transparent);
    background: var(--bg-surface);
  }
  .connect {
    margin-top: 0.25rem;
    background: var(--accent);
    color: var(--text-on-accent);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    padding: 0.85rem;
    font-weight: 600;
    font-size: 0.95rem;
    letter-spacing: 0.01em;
    transition: background 0.15s, border-color 0.15s, opacity 0.15s;
  }
  .connect:active:not(:disabled) { background: var(--accent-hover); }
  .connect:disabled { opacity: 0.4; }
  .err {
    color: var(--error);
    font-size: 0.85rem;
  }
</style>
