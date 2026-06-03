<script lang="ts">
  import { onMount } from "svelte";
  import { listHostConnections, getConnectivity } from "./lib/api";
  import { attachConnectivityListener } from "./lib/events";
  import { corporaStore } from "./lib/stores/corpora.svelte";
  import type { ConnState, HostConnection } from "./lib/types";
  import PairingScreen from "./lib/screens/PairingScreen.svelte";
  import ConversationListScreen from "./lib/screens/ConversationListScreen.svelte";
  import ChatScreen from "./lib/screens/ChatScreen.svelte";
  import ConnectivityBanner from "./lib/ui/ConnectivityBanner.svelte";

  let hosts = $state<HostConnection[]>([]);
  let loaded = $state(false);
  let activeConversationId = $state<string | null>(null);
  let connState = $state<ConnState>("off_tailnet");
  let retryAfter = $state<number | null>(null);

  async function refreshHosts() {
    hosts = await listHostConnections();
    if (hosts.length) {
      try {
        connState = (await getConnectivity()) as ConnState;
      } catch {
        /* leave default */
      }
      // Load CORPUS_REFs so cited sources can be privacy-badged.
      void corporaStore.refresh();
    }
    loaded = true;
  }

  onMount(() => {
    void refreshHosts();
    const offPromise = attachConnectivityListener((s, r) => {
      connState = s as ConnState;
      retryAfter = r ?? null;
    });
    return () => void offPromise.then((off) => off());
  });

  const paired = $derived(hosts.length > 0);
</script>

{#if !loaded}
  <p class="loading">Loading…</p>
{:else if !paired}
  <PairingScreen onpaired={refreshHosts} />
{:else}
  <ConnectivityBanner state={connState} retryAfterSecs={retryAfter} />
  {#if activeConversationId}
    <ChatScreen
      conversationId={activeConversationId}
      onback={() => (activeConversationId = null)}
    />
  {:else}
    <ConversationListScreen onopen={(id) => (activeConversationId = id)} />
  {/if}
{/if}

<style>
  .loading {
    padding: 1rem;
    color: var(--muted);
  }
</style>
