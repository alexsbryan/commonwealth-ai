<script lang="ts">
  // Host-UI affordance to install + open the first-party SF land-value-tax
  // mesh app, replacing the devtools-console invoke. Records the granted
  // permission subset (the consent), then opens the sandboxed window.
  import { onMount } from "svelte";
  import {
    listMeshApps,
    recordMeshAppInstall,
    openMeshApp,
    uninstallMeshApp,
    type MeshAppInstall,
  } from "../api";

  const LVT_APP_ID = "lvt";
  const LVT_NAME = "SF Land-Value Tax";

  let installed = $state<MeshAppInstall | null>(null);
  let busy = $state(false);
  let error = $state("");

  async function refresh() {
    try {
      const apps = await listMeshApps();
      installed = apps.find((a) => a.app_id === LVT_APP_ID) ?? null;
    } catch (e) {
      error = String(e);
    }
  }
  onMount(refresh);

  async function installAndOpen() {
    busy = true;
    error = "";
    try {
      await recordMeshAppInstall(LVT_APP_ID, LVT_NAME, {
        mesh_store_read: true,
        mesh_store_write: false,
        inference_access: false,
        knowledge_access: false,
      });
      await refresh();
      await openMeshApp(LVT_APP_ID);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function open() {
    busy = true;
    error = "";
    try {
      await openMeshApp(LVT_APP_ID);
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function uninstall() {
    busy = true;
    error = "";
    try {
      await uninstallMeshApp(LVT_APP_ID);
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }
</script>

<section class="mesh-apps">
  <h3 class="mesh-apps-h">Mesh apps</h3>
  <div class="app-card">
    <div class="app-name">{LVT_NAME}</div>
    <div class="app-sub">
      A sandboxed explorer over the SF assessor roll. Every figure is computed
      by the host and cited — no model originates a number.
    </div>
    {#if installed}
      <div class="app-perms">Granted: read corpus atoms</div>
      <div class="app-actions">
        <button onclick={open} disabled={busy}>Open</button>
        <button class="ghost" onclick={uninstall} disabled={busy}>Uninstall</button>
      </div>
    {:else}
      <div class="app-perms">
        Requests: read corpus atoms (<code>mesh_store_read</code>) · needs the
        <code>sf-assessor-roll</code> corpus
      </div>
      <div class="app-actions">
        <button onclick={installAndOpen} disabled={busy}>Install &amp; Open</button>
      </div>
    {/if}
    {#if error}<div class="app-error">{error}</div>{/if}
  </div>
</section>

<style>
  .mesh-apps { margin-top: 22px; }
  .mesh-apps-h { font-size: 14px; margin: 0 0 10px; }
  .app-card {
    border: 1px solid var(--border, #2a2f3a);
    border-radius: 10px;
    padding: 14px 16px;
  }
  .app-name { font-weight: 600; }
  .app-sub, .app-perms {
    color: var(--text-dim, #9aa3b2);
    font-size: 12px;
    margin-top: 4px;
  }
  .app-actions { display: flex; gap: 8px; margin-top: 12px; }
  .app-actions button {
    padding: 7px 14px; border-radius: 6px; border: 0;
    font: inherit; font-weight: 600; cursor: pointer;
  }
  .app-actions .ghost {
    background: transparent;
    border: 1px solid var(--border, #2a2f3a);
    color: var(--text-dim, #9aa3b2);
  }
  .app-error { color: #ff8d8d; font-size: 12px; margin-top: 8px; }
</style>
