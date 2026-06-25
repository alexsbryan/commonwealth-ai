<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // Right-rail dashboard for the selected recipe-author project.
  // Pure layout — composes the leaf cards in stable order over a
  // single coarse `RecipeAuthorDashboardState` object.
  import type { RecipeAuthorDashboardState, StarterQuestion } from "../../types";

  import CharterSummary from "./CharterSummary.svelte";
  import CorpusStateCard from "./CorpusStateCard.svelte";
  import RecipeValidationCard from "./RecipeValidationCard.svelte";
  import HarnessLadderCard from "./HarnessLadderCard.svelte";
  import BuildEnrichCard from "./BuildEnrichCard.svelte";
  import SampleProgressBar from "./SampleProgressBar.svelte";
  import IssueList from "./IssueList.svelte";
  import DecisionFeed from "./DecisionFeed.svelte";
  import CapabilityRequestsCard from "./CapabilityRequestsCard.svelte";
  import CheckpointsList from "./CheckpointsList.svelte";
  import ResearchLogPanel from "./ResearchLogPanel.svelte";
  import TechnicalDetailDrawer from "./TechnicalDetailDrawer.svelte";

  // `onUseInChat` / `onOpenChat` are the build-complete "land in use"
  // handoff, threaded down to BuildEnrichCard. Callbacks (not part of
  // the coarse dashboard state) — passed straight through.
  let {
    dashboard,
    onUseInChat,
    onOpenChat,
    onOpenExplorer,
  }: {
    dashboard: RecipeAuthorDashboardState;
    onUseInChat?: (question: StarterQuestion) => void;
    onOpenChat?: () => void;
    onOpenExplorer?: (corpusId: string) => void;
  } = $props();

  // recipe | workflow — drives the kind-aware card labels. Defaults to recipe
  // when the backend payload predates the tag.
  const artifactKind = $derived(dashboard.artifact_kind ?? "recipe");
</script>

<div class="dashboard" data-testid="recipe-author-dashboard">
  <CharterSummary
    title={dashboard.title}
    charterMd={dashboard.charter_md}
  />
  <!-- Corpus state, the sample-size ladder, build+enrich, and the sample
       progress bar are recipe-build concepts (ingest → test → enrich). A
       workflow has none of them, so they're recipe-only; the validation card,
       decision log, checkpoints, research, and the TOML drawer serve both. -->
  {#if artifactKind === "recipe"}
    <CorpusStateCard
      recipeId={dashboard.recipe_id ?? null}
      recipePath={dashboard.recipe_path ?? null}
      lastTestStatus={dashboard.last_test_status ?? null}
      lastTestAt={dashboard.last_test_at ?? null}
    />
  {/if}
  <RecipeValidationCard validation={dashboard.validation} {artifactKind} />
  {#if artifactKind === "recipe"}
    <HarnessLadderCard
      recipePath={dashboard.recipe_path ?? null}
      sampleSize={dashboard.current_sample_size ?? 15}
    />
    <BuildEnrichCard
      recipeId={dashboard.recipe_id ?? null}
      enrichmentReady={dashboard.validation.enrichment_ready}
      {onUseInChat}
      {onOpenChat}
      {onOpenExplorer}
    />
    <SampleProgressBar currentSampleSize={dashboard.current_sample_size ?? null} />
  {/if}
  <IssueList issues={dashboard.recipe_issues} />
  <DecisionFeed
    decisions={dashboard.decisions}
    deferredQuestions={dashboard.deferred_questions}
  />
  <CapabilityRequestsCard requests={dashboard.capability_requests} />
  <CheckpointsList
    featureId={dashboard.feature_id}
    checkpoints={dashboard.checkpoints}
  />
  <ResearchLogPanel findings={dashboard.research_findings} />
  <TechnicalDetailDrawer
    recipeToml={dashboard.recipe_toml ?? null}
    featureId={dashboard.feature_id}
    {artifactKind}
  />
</div>

<style>
  .dashboard {
    display: flex;
    flex-direction: column;
    gap: 0.7rem;
  }
</style>
