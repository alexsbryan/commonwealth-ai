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
  <h1>Connect to your host</h1>
  <p class="hint">Reachable over your tailnet only. The token is stored in the device keychain.</p>
  <label>Name<input bind:value={displayName} placeholder="BeefyMac" /></label>
  <label>
    Tailnet address
    <input bind:value={tailnetAddress} placeholder="beefymac.tailXXXX.ts.net:8080" />
  </label>
  <label>Tenant<input bind:value={tenantId} placeholder="alex" /></label>
  <label>Token<input bind:value={token} type="password" /></label>
  {#if error}<p class="err">{error}</p>{/if}
  <button onclick={pair} disabled={busy || !tailnetAddress || !token}>
    {busy ? "Connecting…" : "Connect"}
  </button>
</div>

<style>
  .pair {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
    padding: 1.25rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: 0.85rem;
    color: var(--muted);
  }
  .hint {
    color: var(--muted);
    font-size: 0.85rem;
  }
  .err {
    color: var(--danger);
  }
</style>
