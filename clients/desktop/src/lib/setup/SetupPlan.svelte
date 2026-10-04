<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  SetupPlan — the consent-before-mutation screen's container. It runs BEFORE
  SetupFlow (the only step that downloads or creates anything) and fetches the
  plan data via READ-ONLY commands (hardware + the recommended models), then
  renders SetupPlanView. Nothing on the machine changes until the user hits
  "Set up svrnmesh" — this screen only reads.

  The view/container split mirrors SetupScreen/SetupFlow: SetupPlanView is a
  pure, backend-free view the dev screen gallery can drive with fixtures, so
  the consent screen's copy is auditable without a daemon.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import SetupPlanView from "./SetupPlanView.svelte";
  import {
    recommendedProfile,
    primaryCatalog,
    slotRecommendation,
    getConfig,
  } from "../api";
  import type { RecommendedProfile, PrimaryOption, SlotConfig } from "../types";
  import type { PrimarySource } from "./setupTypes";

  interface Props {
    onConfirm: (opts: {
      installStarterCorpus: boolean;
      primaryFile?: string;
      primarySource?: PrimarySource;
    }) => void;
    onBack: () => void;
  }
  let { onConfirm, onBack }: Props = $props();

  let loading = $state(true);
  let loadError = $state<string | null>(null);
  let profile = $state<RecommendedProfile | null>(null);
  let catalog = $state<PrimaryOption[]>([]);
  let fast = $state<SlotConfig | null>(null);
  let embed = $state<SlotConfig | null>(null);
  let modelsDir = $state("~/.svrnmesh/models");
  let dataDir = $state("~/.svrnmesh");

  onMount(async () => {
    try {
      const [prof, cat, f, e, cfg] = await Promise.all([
        recommendedProfile(),
        primaryCatalog(),
        slotRecommendation("fast"),
        slotRecommendation("embed"),
        getConfig().catch(() => null),
      ]);
      profile = prof;
      catalog = cat;
      fast = f;
      embed = e;
      if (cfg?.data_dir) {
        dataDir = cfg.data_dir;
        modelsDir = `${cfg.data_dir}/models`;
      }
    } catch (err) {
      loadError = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  });
</script>

<SetupPlanView
  {loading}
  {loadError}
  {profile}
  {catalog}
  {fast}
  {embed}
  {modelsDir}
  {dataDir}
  {onConfirm}
  {onBack}
/>
