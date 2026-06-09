<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
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
    padding: 0.5rem 0.95rem;
    font-size: 0.8rem;
    font-weight: 500;
    text-align: center;
    letter-spacing: -0.005em;
    border-bottom: 1px solid var(--border);
  }
  .danger {
    background: color-mix(in srgb, var(--error) 14%, var(--bg-secondary));
    color: color-mix(in srgb, var(--error) 68%, var(--text-primary));
  }
  .warn {
    background: color-mix(in srgb, var(--warning) 11%, var(--bg-secondary));
    color: var(--accent-light);
  }
</style>
