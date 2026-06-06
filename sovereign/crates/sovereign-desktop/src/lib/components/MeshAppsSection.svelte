<script lang="ts">
  // Host-UI affordance to install + open the first-party mesh apps,
  // replacing the devtools-console invoke. Records the granted permission
  // subset (the consent), then opens the sandboxed window. Driven by a
  // small CATALOG so adding an app (e.g. Enron next) is one entry.
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import {
    listMeshApps,
    recordMeshAppInstall,
    openMeshApp,
    uninstallMeshApp,
    loadCatalog,
    listCorpora,
    installCorpus,
    stageCorpusRecipe,
    type MeshAppInstall,
    type MeshAppManifest,
    type MeshAppPermissions,
  } from "../api";
  import type { CorpusProgressPayload } from "../types";

  type CatalogApp = {
    id: string;
    name: string;
    blurb: string;
    corpus: string;
    grant: MeshAppPermissions;
    grantLabel: string;
    requestLabel: string;
    corpusData?: { size_indexed_gb?: number; recipe?: string };
  };

  // The catalog is discovered from each bundle's manifest (build-time
  // `meshapp/catalog.json`), not hard-coded — adding an app is dropping a
  // bundle + a meshapp.json, no edit here.
  let catalog = $state<CatalogApp[]>([]);

  const PERM_LABELS: Record<string, string> = {
    mesh_store_read: "read corpus atoms",
    mesh_store_write: "write to the mesh store",
    inference_access: "run inference",
    knowledge_access: "read your knowledge base",
  };

  function grantedKeys(g: MeshAppPermissions): string[] {
    return Object.entries(g).filter(([, v]) => v).map(([k]) => k);
  }

  /** Project a manifest into the rendered catalog shape, deriving the consent
   * labels from its granted permission set. */
  function toCatalogApp(m: MeshAppManifest): CatalogApp {
    const keys = grantedKeys(m.grants);
    const labels = keys.map((k) => PERM_LABELS[k] ?? k);
    const grantLabel = labels.join(", ") || "no permissions";
    const requestLabel = keys.length ? `${labels.join(", ")} (${keys.join(", ")})` : "no permissions";
    return {
      id: m.id, name: m.name, blurb: m.blurb, corpus: m.corpus,
      grant: m.grants, grantLabel, requestLabel, corpusData: m.corpus_data,
    };
  }

  let installs = $state<MeshAppInstall[]>([]);
  let busy = $state(""); // app id currently launching, or "" when idle
  let acquiring = $state(""); // app id whose corpus is downloading
  let error = $state("");
  let installedCorpora = $state<Set<string>>(new Set());
  let corpusProgress = $state<Record<string, CorpusProgressPayload>>({});

  function installOf(id: string): MeshAppInstall | null {
    return installs.find((a) => a.app_id === id) ?? null;
  }
  const corpusReady = (app: CatalogApp) => installedCorpora.has(app.corpus);
  const sizeLabel = (app: CatalogApp) => {
    const gb = app.corpusData?.size_indexed_gb;
    if (!gb) return "";
    return gb < 1 ? `${Math.round(gb * 1000)} MB` : `${gb.toFixed(1)} GB`;
  };

  async function refresh() {
    try { installs = await listMeshApps(); } catch (e) { error = String(e); }
  }
  async function refreshCatalog() {
    try { catalog = (await loadCatalog()).map(toCatalogApp); } catch (e) { error = String(e); }
  }
  async function refreshCorpora() {
    try {
      const corpora = await listCorpora();
      installedCorpora = new Set(corpora.filter((c) => c.status === "installed").map((c) => c.id));
    } catch {
      /* the data badge is best-effort */
    }
  }

  onMount(() => {
    refresh();
    refreshCatalog();
    refreshCorpora();
    let unlisten: (() => void) | undefined;
    listen<CorpusProgressPayload>("corpus-progress", (e) => {
      corpusProgress = { ...corpusProgress, [e.payload.corpus_id]: e.payload };
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  });

  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  /** Acquire a mesh app's corpus: stage its bundled recipe (so the daemon can
   * resolve it), kick the install, and poll until the prebuilt snapshot is
   * restored. The bar reads the `corpus-progress` events; completion is polled
   * from listCorpora (robust to a missed terminal event). */
  async function acquireCorpus(app: CatalogApp): Promise<void> {
    acquiring = app.id;
    try {
      const recipeFile = app.corpusData?.recipe || "recipe.toml";
      const res = await fetch(`/meshapp/${app.id}/${recipeFile}`);
      if (!res.ok) throw new Error(`fetch corpus recipe: ${res.status}`);
      await stageCorpusRecipe(app.corpus, await res.text());
      await installCorpus(app.corpus);
      const deadline = Date.now() + 15 * 60 * 1000;
      while (Date.now() < deadline) {
        await sleep(1500);
        if (corpusProgress[app.corpus]?.phase === "failed") {
          throw new Error(corpusProgress[app.corpus]?.message || "corpus install failed");
        }
        const corpora = await listCorpora();
        if (corpora.some((c) => c.id === app.corpus && c.status === "installed")) {
          await refreshCorpora();
          return;
        }
      }
      throw new Error("corpus install timed out");
    } finally {
      acquiring = "";
    }
  }

  /** The one button: record consent (if new), acquire the corpus (if missing),
   * then open the sandboxed window. */
  async function launch(app: CatalogApp) {
    busy = app.id;
    error = "";
    try {
      if (!installOf(app.id)) {
        await recordMeshAppInstall(app.id, app.name, app.grant);
        await refresh();
      }
      if (!corpusReady(app)) await acquireCorpus(app);
      await openMeshApp(app.id);
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
  {#each catalog as app (app.id)}
    {@const inst = installOf(app.id)}
    {@const ready = corpusReady(app)}
    {@const downloading = acquiring === app.id}
    {@const prog = corpusProgress[app.corpus]}
    <div class="app-card">
      <div class="app-name">{app.name}</div>
      <div class="app-sub">{app.blurb}</div>
      <div class="app-perms">
        {#if ready}
          Data: <span class="ok">✓ on this machine</span>
        {:else}
          Data: not downloaded{sizeLabel(app) ? ` · ${sizeLabel(app)}` : ""}
        {/if}
        · {inst ? `granted ${app.grantLabel}` : `requests ${app.requestLabel}`}
      </div>

      {#if downloading}
        <div class="app-progress">
          <div class="bar"><div class="fill" style="width: {prog?.percent ?? 0}%"></div></div>
          <div class="prog-label">{prog?.phase ?? "starting"}… {Math.round(prog?.percent ?? 0)}%</div>
        </div>
      {:else}
        <div class="app-actions">
          <button onclick={() => launch(app)} disabled={busy !== ""}>
            {#if !ready}Get data{sizeLabel(app) ? ` (${sizeLabel(app)})` : ""} &amp; Open
            {:else if inst}Open
            {:else}Install &amp; Open{/if}
          </button>
          {#if inst}
            <button class="ghost" onclick={() => uninstall(app.id)} disabled={busy !== ""}>
              Uninstall
            </button>
          {/if}
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
  .app-perms .ok { color: var(--good, #5bd6a0); }
  .app-progress { margin-top: 12px; }
  .app-progress .bar {
    height: 6px; background: #0c0e12;
    border: 1px solid var(--border, #2a2f3a); border-radius: 4px; overflow: hidden;
  }
  .app-progress .fill { height: 100%; background: var(--accent, #6ea8fe); transition: width 0.3s; }
  .app-progress .prog-label {
    color: var(--text-dim, #9aa3b2); font-size: 11px; margin-top: 5px;
    font-variant-numeric: tabular-nums;
  }
  .app-error { color: #ff8d8d; font-size: 12px; margin-top: 8px; }
</style>
