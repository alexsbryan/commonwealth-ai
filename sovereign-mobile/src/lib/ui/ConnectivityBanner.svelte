<script lang="ts">
  import type { ConnState } from "../types";

  let { state, retryAfterSecs }: { state: ConnState; retryAfterSecs?: number | null } =
    $props();

  // Three distinct, user-actionable states — never one generic
  // "can't connect" (MOBILE.md §7). `reachable` renders nothing.
  const info = $derived.by(() => {
    switch (state) {
      case "off_tailnet":
        return { text: "Off the tailnet — connect to reach your host.", cls: "danger" };
      case "host_down":
        return { text: "Host offline — showing cached conversations.", cls: "warn" };
      case "host_busy":
        return {
          text: `Host busy${retryAfterSecs ? `, retrying in ${retryAfterSecs}s` : ""}…`,
          cls: "warn",
        };
      default:
        return null;
    }
  });
</script>

{#if info}
  <div class="banner {info.cls}" role="status">{info.text}</div>
{/if}

<style>
  .banner {
    padding: 0.5rem 0.9rem;
    font-size: 0.85rem;
    text-align: center;
  }
  .danger {
    background: #3a1d1d;
    color: var(--danger);
  }
  .warn {
    background: #3a331d;
    color: var(--warn);
  }
</style>
