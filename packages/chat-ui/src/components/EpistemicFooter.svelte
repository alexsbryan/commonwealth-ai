<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  EpistemicFooter — renders the typed epistemic ledger
  (`metadata.epistemic_state`, EPISTEMIC_STATE.md) under an assistant
  message. This is the render half of the honesty program (initiative
  I2): the answer's basis, per claim, made visible.

  Pure props, no side channels (the SourceAttribution / RoutingMeta
  sharing rule — no `marked` / `xstate` imports). Three surfaces,
  selected by verdict:

  - The verdict receipt: a single quiet line derived from the ledger's
    verdict + holdings, replacing the grounding_gate.action string
    sniffing the desktop bubble did.
  - Provenance-grouped holdings: badges by basis (Corpus / Memory /
    General knowledge / Tool), expandable to the per-claim list.
    Memory renders DISTINCTLY — its honesty band + a "remembered, not
    verified" mark when the recall shipped unchecked (invariant I3,
    enforced at the type level: rendering matches on the provenance
    variant, so a memory can never render as document evidence).
  - The abstention panel (verdict `cannot_know_from_here`): the gap
    statements + catalog-grounded acquisition-route chips.
-->
<script lang="ts" module>
  import type {
    EpistemicState,
    Holding,
    Provenance,
    MemoryBand,
    Verification,
    TurnVerdict,
    AcquisitionRoute,
  } from "../types";

  /** The provenance basis of a holding, collapsed to its group key. */
  export type ProvKind = "corpus" | "memory" | "general_knowledge" | "tool_derived";

  export function provKind(p: Provenance): ProvKind {
    if (typeof p === "string") return "general_knowledge";
    if ("corpus" in p) return "corpus";
    if ("memory" in p) return "memory";
    return "tool_derived";
  }

  /** Human label for a memory's honesty band. */
  export function bandLabel(b: MemoryBand): string {
    switch (b) {
      case "told_directly":
        return "what you told me";
      case "inferred":
        return "inferred";
      default:
        return "tentative";
    }
  }

  /** A recalled memory that shipped without a completed check — the
   *  "remembered, not verified" mark. Only meaningful for Memory
   *  holdings; corpus/tool holdings carry their own verification. */
  export function isUnverifiedRecall(h: Holding): boolean {
    return (
      provKind(h.provenance) === "memory" &&
      (h.verification === "fail_open" || h.verification === "unverified")
    );
  }

  /** Human label for an acquisition route (extracted from the
   *  InformationRequestCard routes strip so both surfaces phrase them
   *  identically). Returns "" for routes the strip doesn't render. */
  export function routeLabel(route: AcquisitionRoute): string {
    if (route === "connect_folder") return "Connect a folder";
    if (route === "connect_vault") return "Connect an Obsidian vault";
    if (route === "import_conversations") return "Import conversations";
    if (typeof route === "object" && "install_recipe" in route)
      return `Install ${route.install_recipe.name}`;
    return "";
  }

  /** The verdict receipt line — derived purely from the ledger, the
   *  typed replacement for `grounding_gate.action` prefix sniffing.
   *  `null` for the abstention verdict (the panel owns that turn). */
  export interface VerdictReceipt {
    /** Growth / neutral / caution — drives the mark + colour. */
    tone: "grounded" | "neutral" | "caution";
    mark: string;
    text: string;
  }

  export function verdictReceipt(state: EpistemicState): VerdictReceipt | null {
    const verdict: TurnVerdict = state.verdict;
    const corpusVerified = state.holdings.filter(
      (h) => provKind(h.provenance) === "corpus" && h.verification === "verified",
    ).length;
    switch (verdict) {
      case "grounded":
        return {
          tone: "grounded",
          mark: "✓",
          text:
            corpusVerified > 0
              ? `Verified against your sources · ${corpusVerified} claim${corpusVerified === 1 ? "" : "s"} checked`
              : "Verified against your sources",
        };
      case "mixed":
        return {
          tone: "neutral",
          mark: "◑",
          text: "Partly verified — mixed sources",
        };
      case "memory_recall":
        return {
          tone: "neutral",
          mark: "◈",
          text: "Answered from what you've told me",
        };
      case "general_knowledge":
        return {
          tone: "caution",
          mark: "○",
          text: "From general knowledge — not your sources",
        };
      case "unverified":
        return {
          tone: "caution",
          mark: "○",
          text: "Used your sources — not independently verified",
        };
      case "cannot_know_from_here":
        return null; // the abstention panel owns this turn
      default:
        return null;
    }
  }
</script>

<script lang="ts">
  // `EpistemicState`, `Holding`, `Provenance`, etc. are already imported
  // in the module script above (shared scope); only `Gap` is new here.
  import type { Gap } from "../types";

  /** Conv-tiered PPR provenance gate (mirrors SourceAttribution): a
   *  corpus holding whose matching retrieved chunk carries a
   *  `ppr_mass_norm > threshold` gets an entity-bridge subtitle. */
  const PPR_BADGE_THRESHOLD = 0.5;

  interface RetrievedChunk {
    title: string;
    corpus_id: string;
    url?: string;
    snippet: string;
    chunk_id?: number | null;
    source_doc_id?: string | null;
    metadata?: Record<string, string>;
  }

  interface Props {
    /** The typed ledger from `metadata.epistemic_state`. */
    ledger: EpistemicState;
    /** Retrieved-chunk payload, cross-referenced for PPR-bridge
     *  subtitles on corpus holdings (match by corpus_id + chunk_id). */
    retrievedChunks?: RetrievedChunk[];
    /** Navigate to the Library. Route chips on the abstention panel are
     *  NAVIGATIONS (they don't resolve anything). Unset = no chips
     *  (CLI/test hosts, or web-search/provide-document routes which the
     *  chat composer + paste box already cover). */
    onOpenLibrary?: () => void;
  }

  let { ledger, retrievedChunks, onOpenLibrary }: Props = $props();

  const GROUP_LABELS: Record<ProvKind, string> = {
    corpus: "Sources",
    memory: "Memory",
    general_knowledge: "General knowledge",
    tool_derived: "Computed",
  };
  const GROUP_ORDER: ProvKind[] = [
    "corpus",
    "memory",
    "general_knowledge",
    "tool_derived",
  ];

  interface HoldingGroup {
    kind: ProvKind;
    label: string;
    holdings: Holding[];
  }

  let receipt = $derived(verdictReceipt(ledger));

  let isAbstention = $derived(ledger.verdict === "cannot_know_from_here");

  let groups: HoldingGroup[] = $derived.by(() => {
    const byKind = new Map<ProvKind, Holding[]>();
    for (const h of ledger.holdings) {
      const k = provKind(h.provenance);
      const arr = byKind.get(k) ?? [];
      arr.push(h);
      byKind.set(k, arr);
    }
    return GROUP_ORDER.filter((k) => byKind.has(k)).map((k) => ({
      kind: k,
      label: GROUP_LABELS[k],
      holdings: byKind.get(k)!,
    }));
  });

  let expanded = $state(false);

  /** PPR-bridge seed for a corpus holding: match its (corpus_id,
   *  chunk_id) against the retrieved chunks; return the bridge seed
   *  when that chunk was boosted above threshold. */
  function pprBridgeFor(h: Holding): string | null {
    if (!retrievedChunks || retrievedChunks.length === 0) return null;
    const p = h.provenance;
    if (typeof p === "string" || !("corpus" in p)) return null;
    const { corpus_id, chunk_id } = p.corpus;
    if (chunk_id == null) return null;
    const match = retrievedChunks.find(
      (c) =>
        c.chunk_id === chunk_id &&
        (corpus_id == null || c.corpus_id === corpus_id),
    );
    if (!match || !match.metadata) return null;
    const seed = match.metadata.ppr_seed;
    const massRaw = match.metadata.ppr_mass_norm;
    if (!seed || !massRaw) return null;
    const mass = parseFloat(massRaw);
    if (!Number.isFinite(mass) || mass <= PPR_BADGE_THRESHOLD) return null;
    return seed;
  }

  /** The band label for a memory holding (empty for non-memory). */
  function memoryBand(h: Holding): string | null {
    const p = h.provenance;
    if (typeof p === "string" || !("memory" in p)) return null;
    return bandLabel(p.memory.band);
  }

  /** Acquisition-route chips for the abstention panel. `web_search`
   *  and `provide_document` are excluded — the composer + paste box
   *  already cover those; what remains are Library navigations. */
  let abstentionRoutes = $derived.by(() => {
    if (!isAbstention) return [] as AcquisitionRoute[];
    const routes: AcquisitionRoute[] = [];
    const seen = new Set<string>();
    for (const g of ledger.gaps) {
      for (const r of g.routes) {
        const isLibrary =
          r === "connect_folder" ||
          r === "connect_vault" ||
          r === "import_conversations" ||
          (typeof r === "object" && "install_recipe" in r);
        if (!isLibrary) continue;
        const key = typeof r === "string" ? r : JSON.stringify(r);
        if (seen.has(key)) continue;
        seen.add(key);
        routes.push(r);
      }
    }
    return routes;
  });

  /** Gap statements for the abstention panel (deduped, non-empty). */
  let gapStatements = $derived.by(() => {
    if (!isAbstention) return [] as Gap[];
    const seen = new Set<string>();
    return ledger.gaps.filter((g) => {
      const s = g.statement.trim();
      if (!s || seen.has(s)) return false;
      seen.add(s);
      return true;
    });
  });
</script>

<div class="epistemic-footer" data-testid="epistemic-footer" data-verdict={ledger.verdict}>
  {#if isAbstention}
    <!-- Abstention panel: the honest "I can't answer from here" surface.
         Names the gap, then offers concrete, catalog-grounded places to
         get what would fill it. -->
    <div class="abstain" role="note">
      <div class="abstain-head">
        <span class="abstain-mark" aria-hidden="true">⟢</span>
        <span class="abstain-label">Not answerable from your current sources</span>
      </div>
      {#if gapStatements.length > 0}
        <ul class="gap-list">
          {#each gapStatements as gap}
            <li>{gap.statement}</li>
          {/each}
        </ul>
      {/if}
      {#if onOpenLibrary && abstentionRoutes.length > 0}
        <div class="routes" data-testid="abstention-routes">
          <span class="routes-label">Where you could get this</span>
          {#each abstentionRoutes as route}
            <button
              type="button"
              class="route-chip"
              onclick={() => onOpenLibrary?.()}
              title="Opens the Library so you can add the source"
            >
              {routeLabel(route)}
            </button>
          {/each}
        </div>
      {/if}
    </div>
  {:else}
    {#if receipt}
      <div
        class="receipt tone-{receipt.tone}"
        role="note"
        data-testid="epistemic-receipt"
      >
        <span class="receipt-mark" aria-hidden="true">{receipt.mark}</span>
        <span class="receipt-text">{receipt.text}</span>
      </div>
    {/if}

    {#if groups.length > 0}
      <div class="holdings">
        <div
          class="badges"
          role="button"
          tabindex="0"
          aria-expanded={expanded}
          onclick={() => (expanded = !expanded)}
          onkeydown={(e) =>
            (e.key === "Enter" || e.key === " ") &&
            (e.preventDefault(), (expanded = !expanded))}
        >
          {#each groups as group}
            <span class="badge badge-{group.kind}">
              {group.label} ({group.holdings.length})
            </span>
          {/each}
        </div>
        {#if expanded}
          <div class="holding-list">
            {#each groups as group}
              {#each group.holdings as holding}
                {@const band = memoryBand(holding)}
                {@const bridge = pprBridgeFor(holding)}
                <div class="holding-item holding-{group.kind}">
                  <div class="holding-claim">{holding.claim}</div>
                  <div class="holding-meta">
                    {#if band}
                      <span class="mem-band">{band}</span>
                      {#if isUnverifiedRecall(holding)}
                        <span
                          class="mem-unverified"
                          title="Recalled from memory; the grounding check did not run"
                        >
                          remembered, not verified
                        </span>
                      {/if}
                    {:else if group.kind === "tool_derived"}
                      <span class="tool-tag">computed by a tool</span>
                    {:else if holding.verification === "verified"}
                      <span class="verif ok">verified</span>
                    {:else if holding.verification === "failed_once"}
                      <span class="verif warn">revised</span>
                    {:else if holding.verification === "fail_open"}
                      <span class="verif warn">not verified</span>
                    {/if}
                    {#if bridge}
                      <span
                        class="ppr-bridge"
                        title="Conv-tiered entity-graph PPR boost (A3-lite)"
                      >
                        ↗ via entity bridge: <span class="bridge-seed">{bridge}</span>
                      </span>
                    {/if}
                  </div>
                </div>
              {/each}
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .epistemic-footer {
    margin-top: 8px;
  }

  /* ─── Verdict receipt ───────────────────────────────────────── */
  .receipt {
    display: inline-flex;
    align-items: baseline;
    gap: 0.3rem;
    font-size: 0.74rem;
    font-style: italic;
    color: var(--text-muted);
  }
  .receipt-mark {
    font-style: normal;
    font-weight: 600;
  }
  .tone-grounded .receipt-mark {
    color: var(--growth, #6a9c78);
  }
  .tone-neutral .receipt-mark {
    color: var(--accent, #c9a84c);
  }
  .tone-caution .receipt-mark {
    color: var(--text-muted);
  }

  /* ─── Provenance-grouped badges ─────────────────────────────── */
  .holdings {
    margin-top: 6px;
  }
  .badges {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    cursor: pointer;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    padding: 2px 10px;
    font-size: 0.75rem;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 12px;
    color: var(--text-muted);
    white-space: nowrap;
  }
  /* Memory reads distinctly — a lavender tint so "what you told me" is
     never mistaken for a document source at a glance. */
  .badge-memory {
    border-color: color-mix(in srgb, var(--lavender, #9a86c4) 45%, var(--border));
    color: var(--lavender-light, #b6a6dd);
  }
  .badge-general_knowledge {
    font-style: italic;
  }

  .holding-list {
    margin-top: 6px;
    padding: 8px 12px;
    background: var(--bg-surface);
    border-radius: var(--radius);
    border: 1px solid var(--border);
  }
  .holding-item {
    font-size: 0.8rem;
    color: var(--text-secondary);
    padding: 4px 0;
    line-height: 1.4;
  }
  .holding-item + .holding-item {
    border-top: 1px dashed var(--border);
  }
  /* Memory holdings carry a lavender left-rule — the type-level
     distinction (invariant I3) made visual. */
  .holding-memory {
    border-left: 2px solid color-mix(in srgb, var(--lavender, #9a86c4) 55%, transparent);
    padding-left: 8px;
    margin-left: -2px;
  }
  .holding-claim {
    color: var(--text-primary);
  }
  .holding-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 2px;
    font-size: 0.72rem;
  }
  .mem-band {
    color: var(--lavender-light, #b6a6dd);
    font-style: italic;
  }
  .mem-unverified {
    color: var(--warning, #c9a84c);
  }
  .tool-tag {
    color: var(--text-muted);
    font-style: italic;
  }
  .verif.ok {
    color: var(--growth, #6a9c78);
  }
  .verif.warn {
    color: var(--warning, #c9a84c);
  }
  .ppr-bridge {
    color: var(--text-muted);
    font-style: italic;
  }
  .bridge-seed {
    color: var(--lavender-light, #b6a6dd);
    font-style: normal;
    font-weight: 500;
  }

  /* ─── Abstention panel ──────────────────────────────────────── */
  .abstain {
    margin-top: 8px;
    padding: 10px 14px;
    background: color-mix(in srgb, var(--accent, #c9a84c) 4%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent, #c9a84c) 22%, var(--border));
    border-left: 3px solid var(--accent, #c9a84c);
    border-radius: var(--radius);
    font-size: 0.82rem;
    line-height: 1.5;
  }
  .abstain-head {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--accent, #c9a84c);
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  .abstain-mark {
    font-size: 0.95em;
  }
  .gap-list {
    margin: 6px 0 0;
    padding-left: 20px;
    color: var(--text-secondary);
  }
  .gap-list li {
    margin: 2px 0;
  }
  .routes {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 10px;
  }
  .routes-label {
    font-family: var(--font-sans);
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-muted);
    margin-right: 4px;
  }
  .route-chip {
    background: transparent;
    border: 1px solid color-mix(in srgb, var(--lavender, #9a86c4) 40%, transparent);
    color: var(--lavender-light, #b6a6dd);
    font-family: var(--font-sans);
    font-size: 0.8rem;
    padding: 5px 12px;
    border-radius: 999px;
    cursor: pointer;
    transition: background 160ms ease, border-color 160ms ease;
  }
  .route-chip:hover {
    background: color-mix(in srgb, var(--lavender, #9a86c4) 10%, transparent);
    border-color: var(--lavender, #9a86c4);
  }
</style>
