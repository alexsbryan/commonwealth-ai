<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  Workshop — the maker surfaces under one roof (the "make" half of the
  use/make split). Five facets, each re-parenting a surface that already
  exists (re-parenting, not a rewrite):
    Build         — author a recipe by describing it (recipe-author workspace)
    Run           — point an automation at a folder and watch it
    Test          — validate / dry-run a recipe TOML
    Connect tools — plug in MCP servers
    Open to apps  — expose the OpenAI-compatible endpoint to external clients
  The last three moved out of Settings in Phase 3; the hidden
  `enable_recipe_authoring` flag died in Phase 0.
-->
<script lang="ts">
  import RecipeAuthorWorkspace from "../recipe_author/RecipeAuthorWorkspace.svelte";
  import WorkflowRunView from "../workflow_run/WorkflowRunView.svelte";
  import RecipeTestingPanel from "../RecipeTestingPanel.svelte";
  import McpServersSection from "../McpServersSection.svelte";
  import ConnectSection from "../ConnectSection.svelte";
  import type { StarterQuestion } from "../../types";

  type WorkshopTab = "build" | "run" | "test" | "connect" | "apps";

  let {
    tab,
    onTabChange,
    onExit,
    onUseInChat,
    onOpenChat,
    onRunWorkflow,
    runPreselect = null,
  }: {
    tab: WorkshopTab;
    onTabChange: (t: WorkshopTab) => void;
    onExit: () => void;
    onUseInChat?: (question: StarterQuestion) => void;
    onOpenChat?: () => void;
    onRunWorkflow?: (name: string) => void;
    runPreselect?: string | null;
  } = $props();

  const TABS: { id: WorkshopTab; label: string; testid: string }[] = [
    { id: "build", label: "Build", testid: "workshop-tab-build" },
    { id: "run", label: "Run", testid: "workshop-tab-run" },
    { id: "test", label: "Test", testid: "workshop-tab-test" },
    { id: "connect", label: "Connect tools", testid: "workshop-tab-connect" },
    { id: "apps", label: "Open to apps", testid: "workshop-tab-apps" },
  ];
</script>

<div class="workshop" data-testid="workshop-view">
  <nav class="subnav" aria-label="Workshop sections">
    {#each TABS as t}
      <button
        class:active={tab === t.id}
        data-testid={t.testid}
        onclick={() => onTabChange(t.id)}
      >
        {t.label}
      </button>
    {/each}
  </nav>

  <div class="workshop-body">
    {#if tab === "build"}
      <RecipeAuthorWorkspace {onExit} {onUseInChat} {onOpenChat} {onRunWorkflow} />
    {:else if tab === "run"}
      <WorkflowRunView {onOpenChat} preselectName={runPreselect} />
    {:else if tab === "test"}
      <div class="workshop-panel">
        <h2>Test a recipe</h2>
        <p class="lede">
          Validate and dry-run a recipe TOML before you ship it — catch a bad
          selector or a missing field on a small sample first.
        </p>
        <RecipeTestingPanel />
      </div>
    {:else if tab === "connect"}
      <div class="workshop-panel">
        <h2>Connect tools</h2>
        <p class="lede">
          Plug in <strong>Model Context Protocol</strong> servers (vision, web,
          filesystem…). Their tools appear in chat and your recipes — svrnmesh
          asks before the first use.
        </p>
        <McpServersSection />
      </div>
    {:else}
      <div class="workshop-panel">
        <h2>Open to apps</h2>
        <p class="lede">
          svrnmesh speaks the OpenAI API on your machine. Point Codex, Claude
          Code, or any OpenAI-compatible client at it — nothing leaves your
          machine.
        </p>
        <ConnectSection />
      </div>
    {/if}
  </div>
</div>

<style>
  .workshop {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }
  .subnav {
    display: flex;
    gap: 4px;
    padding: 10px 14px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-secondary);
    flex-shrink: 0;
    flex-wrap: wrap;
  }
  .subnav button {
    font: inherit;
    cursor: pointer;
    padding: 6px 14px;
    border-radius: var(--radius);
    border: 1px solid transparent;
    background: transparent;
    color: var(--text-secondary);
    font-weight: 500;
  }
  .subnav button:hover {
    color: var(--text-primary);
  }
  .subnav button.active {
    background: var(--bg-elevated);
    color: var(--text-primary);
    border-color: color-mix(in oklch, var(--accent) 35%, var(--border));
  }
  .workshop-body {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  /* The three re-parented Settings surfaces expect a padded, scrollable
     host (they sat inside a Settings doc-section). Build / Run own their
     own full-height layout and skip this wrapper. */
  .workshop-panel {
    height: 100%;
    overflow-y: auto;
    padding: 24px 28px;
    max-width: 820px;
  }
  .workshop-panel h2 {
    font-size: 1.05rem;
    font-weight: 600;
    color: var(--text-primary);
    margin: 0 0 8px;
  }
  .workshop-panel .lede {
    color: var(--text-secondary);
    font-size: 0.9rem;
    line-height: 1.55;
    margin: 0 0 18px;
    max-width: 64ch;
  }
</style>
