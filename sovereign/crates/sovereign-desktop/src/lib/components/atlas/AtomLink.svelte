<script lang="ts" module>
  import type { AtomType, ReferencedAtom } from "../../types";

  /** Shape of the context AtomDetail provides so every body
   *  subcomponent can render atom_id refs as clickable chips. */
  export interface AtomLinkResolver {
    labelFor(atomId: string): ReferencedAtom | undefined;
    navigate(atomId: string): void;
  }

  export const ATOM_LINK_CONTEXT_KEY = Symbol("atom-link-resolver");

  const ATOM_TYPE_LABEL: Record<AtomType, string> = {
    Entity: "Entity",
    Event: "Event",
    State: "State",
    Relation: "Relation",
    Claim: "Claim",
    Question: "Question",
    Configuration: "Config",
    ArgumentReconstruction: "Argument",
  };

  export function atomTypeLabel(t: AtomType): string {
    return ATOM_TYPE_LABEL[t];
  }
</script>

<script lang="ts">
  // AtomLink — clickable chip for one atom_id reference inside a
  // body subcomponent. Resolves the label via the parent's
  // `atomLinkResolver` context, falls back to rendering the raw id
  // when the ref is dangling (atom not found in atoms.json).

  import { getContext } from "svelte";

  interface Props {
    atomId: string;
  }

  let { atomId }: Props = $props();

  const resolver = getContext<AtomLinkResolver | undefined>(
    ATOM_LINK_CONTEXT_KEY,
  );

  let info = $derived(resolver?.labelFor(atomId));
</script>

{#if info && resolver}
  <button
    type="button"
    class="atom-link"
    onclick={() => resolver.navigate(atomId)}
    title={`Open ${atomId}`}
  >
    <span class="atom-link-type">{atomTypeLabel(info.atom_type)}</span>
    <span class="atom-link-name">{info.display_name}</span>
  </button>
{:else}
  <span class="atom-link-fallback mono" title="Unresolved reference">{atomId}</span>
{/if}

<style>
  .atom-link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px 2px 4px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 10px;
    color: inherit;
    font: inherit;
    font-size: 0.8rem;
    cursor: pointer;
    transition:
      border-color 150ms ease,
      background 150ms ease,
      color 150ms ease;
  }

  .atom-link:hover {
    border-color: var(--accent);
    background: var(--bg-elevated, var(--bg-secondary));
    color: var(--accent);
  }

  .atom-link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .atom-link-type {
    padding: 1px 5px;
    background: var(--bg-primary);
    border-radius: 6px;
    font-size: 0.65rem;
    color: var(--text-muted);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .atom-link-name {
    font-weight: 500;
    max-width: 28ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .atom-link-fallback {
    color: var(--text-muted);
    font-size: 0.78rem;
    font-family: var(--font-mono, monospace);
  }
</style>
