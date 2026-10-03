<script lang="ts">
  /**
   * The room: mint guest links, show their QR, see what is outstanding.
   *
   * A "room" is one member's machine serving guest pages to people who are not
   * members. This panel is the operator's side of that — `svrn mesh grant` in
   * the CLI is the other client of the same daemon routes.
   *
   * The LINK is composed by the daemon (page path + token + this node's iroh
   * dial string when it has one): this app does not link the mesh crates, so it
   * asks and displays. The QR uses the same `qrcode` dependency the mobile
   * pairing card does.
   *
   * What this panel does NOT do, deliberately: revoke a row from the list. The
   * daemon lists rows by an 8-hex token PREFIX (so a screen-shared settings
   * panel never spills whole bearers), and revoke takes the full token. The
   * token is in hand only for the link you just minted here, which is the one
   * this panel offers to kill; the list is informational.
   */
  import { onMount } from "svelte";
  import {
    createGuestGrant,
    listGuestGrants,
    revokeGuestGrant,
    type GuestGrant,
    type GuestGrantRow,
  } from "../api";

  /** Where a phone opens the page: the static origin, or this machine's room
   *  address. Defaulted to the shipped origin because the anywhere case is the
   *  one this panel exists for. */
  let baseUrl = $state("https://svrnme.sh");
  /** Empty means the whole wall; otherwise one rail namespace. */
  let scope = $state("");
  let ttlSecs = $state(2 * 3600);
  let label = $state("");

  let busy = $state(false);
  let error = $state<string | null>(null);
  let minted = $state<GuestGrant | null>(null);
  let qr = $state<string | null>(null);
  let rows = $state<GuestGrantRow[]>([]);

  const TTL_CHOICES: Array<[string, number]> = [
    ["30 minutes", 1800],
    ["2 hours", 2 * 3600],
    ["12 hours", 12 * 3600],
    ["1 day", 24 * 3600],
  ];

  function until(unixMs: number): string {
    const secs = Math.floor(unixMs / 1000) - Math.floor(Date.now() / 1000);
    if (secs <= 0) return "expired";
    if (secs < 90) return "seconds left";
    if (secs < 3600) return `${Math.floor(secs / 60)}m left`;
    if (secs < 86400) return `${Math.floor(secs / 3600)}h left`;
    return `${Math.floor(secs / 86400)}d left`;
  }

  async function refresh() {
    try {
      rows = await listGuestGrants();
      error = null;
    } catch (e) {
      // Absence reported, not swallowed: an unreachable daemon is a fact the
      // operator needs, and the empty list would read as "nothing outstanding".
      error = String(e);
      rows = [];
    }
  }

  onMount(refresh);

  async function mint() {
    busy = true;
    error = null;
    minted = null;
    qr = null;
    try {
      const grant = await createGuestGrant({
        scope,
        models: [],
        baseUrl,
        ttlSecs,
        label,
      });
      minted = grant;
      if (grant.link) {
        // Dynamic import, as SettingsPanel does for the pairing card: the QR
        // library is not on the first-paint path.
        const QRCode = await import("qrcode");
        qr = await QRCode.toDataURL(grant.link, { width: 260, margin: 1 });
      }
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function revoke() {
    if (!minted) return;
    busy = true;
    try {
      await revokeGuestGrant(minted.token);
      minted = null;
      qr = null;
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function copyLink() {
    if (minted?.link) await navigator.clipboard.writeText(minted.link);
  }
</script>

<div class="room">
  <p class="section-label">Room</p>
  <p class="muted">
    Mint a guest link: someone who is not a member scans it and their browser
    reaches this machine — no account, no app. With the shipped origin as the
    base, that works from any network; a guest shares no network with you.
  </p>

  <div class="form">
    <label>
      <span>Page base</span>
      <input bind:value={baseUrl} placeholder="https://svrnme.sh" />
    </label>
    <label>
      <span>Reaches</span>
      <input bind:value={scope} placeholder="the whole wall (or an app name)" />
    </label>
    <label>
      <span>Lifetime</span>
      <select bind:value={ttlSecs}>
        {#each TTL_CHOICES as [text, secs] (secs)}
          <option value={secs}>{text}</option>
        {/each}
      </select>
    </label>
    <label>
      <span>Note</span>
      <input bind:value={label} placeholder="optional, shown in the list" />
    </label>
    <button class="primary" onclick={mint} disabled={busy}
      >{busy ? "Minting…" : "Mint guest link"}</button
    >
  </div>

  {#if error}
    <p class="alert error">{error}</p>
  {/if}

  {#if minted}
    <div class="card minted">
      <p class="minted-line">
        Minted: {minted.summary}
        {#if minted.link}
          · {until(minted.expires_at_ms)}
        {/if}
      </p>
      {#if minted.link}
        {#if qr}
          <img class="qr" src={qr} alt="Guest link QR code" />
        {/if}
        <code class="link">{minted.link}</code>
        <div class="actions">
          <button onclick={copyLink}>Copy link</button>
          <button class="danger" onclick={revoke} disabled={busy}>Revoke this link</button>
        </div>
      {:else}
        <p class="muted">
          No link: give a page base above (the daemon composes the link from
          it, and only then).
        </p>
      {/if}
    </div>
  {/if}

  {#if rows.length > 0}
    <p class="section-label">Grants</p>
    <ul class="grants">
      {#each rows as r (r.token_prefix)}
        <li class="grant-row">
          <span class="dot" class:on={r.live}></span>
          <span class="summary">{r.summary}</span>
          {#if r.label}<span class="label">{r.label}</span>{/if}
          <span class="meta">
            {r.token_prefix} · {until(r.expires_at_ms)}
            {#if r.revoked}· revoked{/if}
          </span>
        </li>
      {/each}
    </ul>
    <p class="muted">
      Rows are identified by their token prefix; to kill one, revoke the link
      where you minted it or run <code>svrn mesh grant --revoke &lt;token&gt;</code>.
    </p>
  {/if}
</div>

<style>
  .room {
    margin-top: 1.25rem;
  }
  .section-label {
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .muted {
    color: var(--text-muted);
    font-size: 0.85rem;
    margin: 0.25rem 0 0.75rem;
  }
  .form {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 0.6rem;
    align-items: end;
  }
  .form label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.85rem;
    color: var(--text-secondary);
  }
  .form input,
  .form select {
    padding: 8px 10px;
    background: var(--bg-input);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    outline: none;
  }
  .form input:focus,
  .form select:focus {
    border-color: var(--accent);
  }
  button.primary {
    padding: 9px 18px;
    background: var(--accent);
    color: var(--text-on-accent);
    border-radius: var(--radius);
    font-weight: 500;
    transition: background 0.2s;
  }
  button.primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }
  button.primary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .card {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px 18px;
    background: var(--bg-secondary);
  }
  .minted {
    margin-top: 0.9rem;
  }
  .minted-line {
    margin: 0 0 0.6rem;
    font-weight: 600;
    color: var(--text-primary);
  }
  /* The QR plate is white on purpose: a scanner needs dark modules on a light
     field, and every surface token here is dark. */
  .qr {
    display: block;
    width: 260px;
    max-width: 100%;
    height: auto;
    background: #fff;
    padding: 6px;
    border-radius: var(--radius);
  }
  .link {
    display: block;
    margin: 0.5rem 0;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: var(--text-secondary);
    word-break: break-all;
  }
  .actions {
    display: flex;
    gap: 0.5rem;
  }
  .actions button {
    padding: 7px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
    transition:
      border-color 0.2s,
      color 0.2s;
  }
  .actions button:hover:not(:disabled) {
    border-color: var(--border-bright);
    color: var(--text-primary);
  }
  .actions button.danger:hover:not(:disabled) {
    color: var(--error);
    border-color: var(--error);
  }
  .alert.error {
    background: color-mix(in srgb, var(--error) 10%, transparent);
    color: var(--error);
    border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
    border-radius: var(--radius);
    padding: 8px 12px;
    font-size: 0.85rem;
  }
  .grants {
    list-style: none;
    margin: 0.25rem 0 0.5rem;
    padding: 0;
  }
  .grant-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.4rem 0.65rem;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
    flex: none;
  }
  .dot.on {
    background: var(--success);
  }
  .summary {
    font-weight: 600;
    color: var(--text-primary);
  }
  .label {
    color: var(--text-muted);
  }
  .meta {
    color: var(--text-muted);
    font-size: 0.85em;
    margin-left: auto;
  }
</style>
