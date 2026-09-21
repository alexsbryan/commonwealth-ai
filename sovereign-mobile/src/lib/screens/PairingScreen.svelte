<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { addHostConnection } from "../api";

  let { onpaired }: { onpaired: () => void } = $props();

  let displayName = $state("");
  let tailnetAddress = $state("");
  let tenantId = $state("");
  let token = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);

  // An iroh pairing string (host's GET /status → iroh.dial) is
  // unambiguous: `<64-hex-endpoint-id>@…`. Auto-detect so pairing
  // needs no transport toggle — paste either kind of address.
  const isIrohAddress = (addr: string) => /^[0-9a-fA-F]{64}@/.test(addr.trim());

  // One-paste pairing. Two equivalent payload forms fill every field:
  //  - the desktop's "Copy pairing code" JSON: {"address":…,"tenant":…,"token":…}
  //  - the QR deep link: sovereign://pair#<base64url(JSON)> — scanning
  //    with the camera opens the app directly (handled below), but the
  //    raw link also works pasted into the address field.
  function applyPayload(p: unknown): boolean {
    if (typeof p !== "object" || p === null) return false;
    const o = p as Record<string, unknown>;
    if (typeof o.address !== "string" || typeof o.token !== "string") return false;
    tailnetAddress = o.address;
    token = o.token;
    if (typeof o.tenant === "string") tenantId = o.tenant;
    if (!displayName) displayName = "My host";
    return true;
  }

  function decodePairLink(link: string): unknown | null {
    const m = link.trim().match(/^sovereign:\/\/pair#(.+)$/);
    if (!m) return null;
    try {
      const b64 = m[1].replace(/-/g, "+").replace(/_/g, "/");
      return JSON.parse(atob(b64));
    } catch {
      return null;
    }
  }

  function trySmartPaste(value: string): boolean {
    const v = value.trim();
    const fromLink = decodePairLink(v);
    if (fromLink) return applyPayload(fromLink);
    if (!v.startsWith("{")) return false;
    try {
      return applyPayload(JSON.parse(v));
    } catch {
      return false; /* not JSON — normal input */
    }
  }

  function onAddressInput() {
    if (trySmartPaste(tailnetAddress)) return;
  }

  // QR scan → "Open in Sovereign": the deep link arrives either as a
  // live `pair-link` event (app already running) or stashed Rust-side
  // before this screen mounted (cold launch) — drain both.
  onMount(() => {
    void invoke<string | null>("take_pending_pair_link").then((link) => {
      if (link) {
        const p = decodePairLink(link);
        if (p) applyPayload(p);
      }
    });
    const unlisten = listen<string>("pair-link", (e) => {
      const p = decodePairLink(e.payload);
      if (p) applyPayload(p);
    });
    return () => {
      void unlisten.then((f) => f());
    };
  });

  async function pair() {
    busy = true;
    error = null;
    try {
      const kind = isIrohAddress(tailnetAddress) ? "iroh" : "tailnet";
      await addHostConnection(displayName, tailnetAddress.trim(), tenantId, token, kind);
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
    <p class="hint">Paste a tailnet address, or an iroh pairing code (no VPN needed). The token is stored in the device keychain.</p>
  </header>

  <div class="fields">
    <label><span>Name</span><input bind:value={displayName} placeholder="mac-peer" /></label>
    <label>
      <span>Host address</span>
      <input bind:value={tailnetAddress} oninput={onAddressInput}
             placeholder="paste pairing code, or host address"
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
