<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import type { PositionStyle } from "../types";

  interface Props {
    name: string;
    style: PositionStyle;
  }

  let { name, style }: Props = $props();

  let cssClass = $derived(
    typeof style === "string"
      ? style === "Compatibilism"
        ? "pos-compatibilism"
        : style === "HardIncompatibilism"
          ? "pos-hard-incompat"
          : style === "Libertarianism"
            ? "pos-libertarianism"
            : "pos-neutral"
      : "pos-custom",
  );

  let customStyle = $derived(
    typeof style === "object" && "Custom" in style
      ? `background:${style.Custom.bg};color:${style.Custom.text};border-color:${style.Custom.border}`
      : "",
  );
</script>

<span class="pos-badge {cssClass}" style={customStyle}>{name}</span>

<style>
  .pos-badge {
    display: inline-block;
    font-family: var(--font-sans);
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.04em;
    padding: 2px 8px;
    border-radius: 999px;
    border: 0.5px solid;
    margin-bottom: 4px;
  }

  .pos-compatibilism {
    background: var(--pos-compat-bg);
    color: var(--pos-compat-text);
    border-color: var(--pos-compat-border);
  }

  .pos-hard-incompat {
    background: var(--pos-incompat-bg);
    color: var(--pos-incompat-text);
    border-color: var(--pos-incompat-border);
  }

  .pos-libertarianism {
    background: var(--pos-libert-bg);
    color: var(--pos-libert-text);
    border-color: var(--pos-libert-border);
  }

  .pos-neutral {
    background: var(--bg-surface);
    color: var(--text-secondary);
    border-color: var(--border-mid);
  }

  .pos-custom {
    border-style: solid;
  }
</style>
