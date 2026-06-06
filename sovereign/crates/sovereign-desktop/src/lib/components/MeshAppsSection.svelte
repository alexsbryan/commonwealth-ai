<script lang="ts">
  // Host-UI affordance to install + open the first-party mesh apps,
  // replacing the devtools-console invoke. Records the granted permission
  // subset (the consent), then opens the sandboxed window. Driven by a
  // small CATALOG so adding an app (e.g. Enron next) is one entry.
  import { onMount } from "svelte";
  import {
    listMeshApps,
    recordMeshAppInstall,
    openMeshApp,
    uninstallMeshApp,
    type MeshAppInstall,
    type MeshAppPermissions,
  } from "../api";

  type CatalogApp = {
    id: string;
    name: string;
    blurb: string;
    corpus: string;
    grant: MeshAppPermissions;
    grantLabel: string;
    requestLabel: string;
  };

  const READ_ONLY: MeshAppPermissions = {
    mesh_store_read: true,
    mesh_store_write: false,
    inference_access: false,
    knowledge_access: false,
  };

  const CATALOG: CatalogApp[] = [
    {
      id: "lvt",
      name: "SF Land-Value Tax",
      blurb:
        "A sandboxed explorer over the SF assessor roll. Every figure is computed " +
        "by the host and cited — no model originates a number.",
      corpus: "sf-assessor-roll",
      grant: READ_ONLY,
      grantLabel: "read corpus atoms",
      requestLabel: "read corpus atoms (mesh_store_read)",
    },
    {
      id: "uap",
      name: "Project Blue Book",
      blurb:
        "A sandboxed explorer over the Air Force's UFO archive. The hotspot " +
        "ranking and every edge are a deterministic fold over the investigation " +
        "graph — each claim quotes its Form-10073 card.",
      corpus: "uap-blue-book",
      grant: READ_ONLY,
      grantLabel: "read corpus atoms",
      requestLabel: "read corpus atoms (mesh_store_read)",
    },
    {
      id: "enron",
      name: "Enron Task Force",
      blurb:
        "A sandboxed explorer over ~4,450 Enron emails, resolved into one " +
        "identity graph. Cross-inbox aliases collapse to canonical people and " +
        "companies; counterparty centrality is graph degree, and every " +
        "relationship cites its source email — no inference.",
      corpus: "enron-sample-multi-wide",
      grant: READ_ONLY,
      grantLabel: "read corpus atoms",
      requestLabel: "read corpus atoms (mesh_store_read)",
    },
  ];

  let installs = $state<MeshAppInstall[]>([]);
  let busy = $state(""); // app id currently busy, or "" when idle
  let error = $state("");

  function installOf(id: string): MeshAppInstall | null {
    return installs.find((a) => a.app_id === id) ?? null;
  }

  async function refresh() {
    try {
      installs = await listMeshApps();
    } catch (e) {
      error = String(e);
    }
  }
  onMount(refresh);

  async function installAndOpen(app: CatalogApp) {
    busy = app.id;
    error = "";
    try {
      await recordMeshAppInstall(app.id, app.name, app.grant);
      await refresh();
      await openMeshApp(app.id);
    } catch (e) {
      error = String(e);
    } finally {
      busy = "";
    }
  }

  async function open(id: string) {
    busy = id;
    error = "";
    try {
      await openMeshApp(id);
    } catch (e) {
      error = String(e);
    } finally {
      busy = "";
    }
  }

  async function uninstall(id: string) {
    busy = id;
    error = "";
    try {
      await uninstallMeshApp(id);
      await refresh();
    } catch (e) {
      error = String(e);
    } finally {
      busy = "";
    }
  }
</script>

<section class="mesh-apps">
  <h3 class="mesh-apps-h">Mesh apps</h3>
  {#each CATALOG as app (app.id)}
    {@const inst = installOf(app.id)}
    <div class="app-card">
      <div class="app-name">{app.name}</div>
      <div class="app-sub">{app.blurb}</div>
      {#if inst}
        <div class="app-perms">Granted: {app.grantLabel}</div>
        <div class="app-actions">
          <button onclick={() => open(app.id)} disabled={busy !== ""}>Open</button>
          <button class="ghost" onclick={() => uninstall(app.id)} disabled={busy !== ""}>
            Uninstall
          </button>
        </div>
      {:else}
        <div class="app-perms">
          Requests: {app.requestLabel} · needs the <code>{app.corpus}</code> corpus
        </div>
        <div class="app-actions">
          <button onclick={() => installAndOpen(app)} disabled={busy !== ""}>
            Install &amp; Open
          </button>
        </div>
      {/if}
    </div>
  {/each}
  {#if error}<div class="app-error">{error}</div>{/if}
</section>

<style>
  .mesh-apps { margin-top: 22px; }
  .mesh-apps-h { font-size: 14px; margin: 0 0 10px; }
  .app-card {
    border: 1px solid var(--border, #2a2f3a);
    border-radius: 10px;
    padding: 14px 16px;
    margin-bottom: 10px;
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
