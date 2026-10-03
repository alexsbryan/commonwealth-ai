<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // The author's declared attributes, as rows.
  //
  // Four of the eleven atom kinds carry an `attributes` map (Entity,
  // Event, Relation, Claim); the rest have no slot, which is why a
  // `role_of` type's attributes have nowhere to land. Rendered by the
  // four bodies that can have them, keyed by the author's own
  // attribute names.
  //
  // A `ref` attribute arrives as a bare string. It is an atom id when
  // the extraction resolved it (`"entity-0013"`) and free text when it
  // did not (`"unidentified continental mint"`) — the wire does not
  // say which, and the declaration that would is not on the atom. So
  // ASK the resolver: a value the focal atom's `referenced_atoms` map
  // knows is a link, everything else is text. That keeps a legitimate
  // free-text answer out of the "unresolved reference" styling, which
  // would read as a broken link rather than as what the source said.
  import { getContext } from "svelte";
  import AtomLink from "../AtomLink.svelte";
  import {
    ATOM_LINK_CONTEXT_KEY,
    type AtomLinkResolver,
  } from "../AtomLink.svelte";
  import type { AtomAttributes } from "../../../types";

  interface Props {
    attributes?: AtomAttributes;
    /** Heading for the group. The default suits every atom kind; a
     *  caller with a better word for it can say so. */
    title?: string;
  }

  let { attributes, title = "Attributes" }: Props = $props();

  const resolver = getContext<AtomLinkResolver | undefined>(
    ATOM_LINK_CONTEXT_KEY,
  );

  let rows = $derived(Object.entries(attributes ?? {}));

  function isLink(v: unknown): v is string {
    return typeof v === "string" && resolver?.labelFor(v) !== undefined;
  }

  /** Everything that is not a link, as text. `null` prints an em dash
   *  rather than the word "null": the wire says the attribute was
   *  present and empty, and that is what a reader needs to see. */
  function asText(v: string | number | boolean | null): string {
    if (v === null) return "—";
    if (typeof v === "boolean") return v ? "yes" : "no";
    return String(v);
  }
</script>

{#if rows.length > 0}
  <div class="attributes" data-testid="atom-attributes">
    <h3 class="attr-title">{title}</h3>
    <dl class="fields">
      {#each rows as [name, value] (name)}
        <dt data-testid="atom-attribute-name">{name}</dt>
        <dd data-testid="atom-attribute-value">
          {#if isLink(value)}
            <AtomLink atomId={value} />
          {:else}
            {asText(value)}
          {/if}
        </dd>
      {/each}
    </dl>
  </div>
{/if}

<style>
  .attributes {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .attr-title {
    margin: 0;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted);
    font-weight: 600;
  }
  .fields {
    display: grid;
    grid-template-columns: 140px 1fr;
    gap: 6px 14px;
    margin: 0;
    font-size: 0.85rem;
  }
  .fields dt {
    color: var(--text-muted);
    font-size: 0.78rem;
    letter-spacing: 0.02em;
    font-family: var(--font-mono, monospace);
  }
  .fields dd {
    margin: 0;
  }
</style>
