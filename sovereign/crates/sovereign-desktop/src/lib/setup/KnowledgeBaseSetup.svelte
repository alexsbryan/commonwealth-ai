<script lang="ts">
  interface Props {
    persona: "research" | "assistant" | "developer";
    onSelect: (tierId: string) => void;
    onSkip: () => void;
  }

  let { persona, onSelect, onSkip }: Props = $props();

  // Sizes reflect the registry_snapshot.toml `size_indexed_gb`
  // values. Wikipedia ships in **Core scope** (top 100K by pageview
  // rank ∪ Vital Articles Level 5, ~6 GB) plus a Simple English
  // satellite that installs alongside it as Layer 0 — the user only
  // sees one "Wikipedia" entry; the layered stack is a backend
  // detail. Expandable to full Wikipedia later from Settings →
  // Knowledge without re-indexing.
  const tiers = [
    {
      id: "essential",
      name: "Essential",
      size: "~7 GB",
      description:
        "Wikipedia (~150K most-looked-up + curated articles, plain-language fallback). Ready to chat in ~12 min. Expandable to full Wikipedia later.",
      corpora: ["Wikipedia"],
    },
    {
      id: "research",
      name: "Research",
      size: "~520 GB",
      description:
        "Essential plus scholarly papers, philosophy reference, and CRS policy analysis. For academic work.",
      corpora: ["Wikipedia", "OpenAlex", "SEP", "CRS Reports"],
    },
    {
      id: "technical",
      name: "Technical",
      size: "~130 GB",
      description:
        "Essential plus Stack Exchange expert Q&A. For programming and engineering.",
      corpora: ["Wikipedia", "Stack Exchange"],
    },
    {
      id: "full",
      name: "Full",
      size: "~660 GB",
      description: "All knowledge bases — maximum coverage.",
      corpora: [
        "Wikipedia",
        "OpenAlex",
        "SEP",
        "Stack Exchange",
        "Gutenberg",
        "CRS Reports",
      ],
    },
  ];

  const recommended: Record<string, string> = {
    research: "research",
    developer: "technical",
    assistant: "essential",
  };

  // Split recommendation from user choice: `selected` is what the user sees
  // highlighted, which defaults to the recommendation but flips to their pick
  // as soon as they click. This avoids capturing `persona` at init time.
  let userSelection: string | null = $state(null);
  let recommendedTier = $derived(recommended[persona] ?? "essential");
  let selected = $derived(userSelection ?? recommendedTier);
</script>

<div class="kb-setup">
  <h1>Knowledge Base</h1>
  <p class="subtitle">
    Choose which knowledge bases to install. These are indexed locally for
    private, offline search.
  </p>

  <div class="tier-cards">
    {#each tiers as tier}
      <button
        class="tier-card"
        class:selected={selected === tier.id}
        onclick={() => (userSelection = tier.id)}
      >
        <div class="tier-header">
          <h2>{tier.name}</h2>
          <span class="tier-size">{tier.size}</span>
        </div>
        <p class="tier-desc">{tier.description}</p>
        <div class="tier-corpora">
          {#each tier.corpora as corpus}
            <span class="corpus-tag">{corpus}</span>
          {/each}
        </div>
        {#if recommended[persona] === tier.id}
          <div class="recommended">Recommended</div>
        {/if}
      </button>
    {/each}
  </div>

  <div class="actions">
    <button class="continue-btn" onclick={() => onSelect(selected)}>
      Continue
    </button>
    <button class="skip-link" onclick={onSkip}>
      I'll set this up later
    </button>
  </div>
</div>

<style>
  .kb-setup {
    text-align: center;
    max-width: 900px;
    margin: 0 auto;
  }
  h1 {
    font-size: 1.8rem;
    font-weight: 300;
    margin-bottom: 0.5rem;
  }
  .subtitle {
    color: var(--text-secondary);
    margin-bottom: 2rem;
    font-size: 1rem;
  }
  .tier-cards {
    display: grid;
    grid-template-columns: repeat(2, 1fr);
    gap: 16px;
    margin-bottom: 2rem;
  }
  .tier-card {
    padding: 20px;
    background: var(--bg-secondary);
    border: 2px solid var(--border-mid);
    border-radius: var(--radius-lg);
    text-align: left;
    transition: border-color 0.2s, background 0.2s, box-shadow 0.2s;
    position: relative;
  }
  .tier-card:hover {
    border-color: color-mix(in srgb, var(--accent) 50%, transparent);
    background: var(--bg-surface);
    box-shadow: 0 2px 16px rgba(201, 168, 76, 0.08);
  }
  .tier-card.selected {
    border-color: var(--accent);
    background: var(--bg-elevated);
    box-shadow: 0 2px 20px rgba(201, 168, 76, 0.12);
  }
  .tier-header {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    margin-bottom: 8px;
  }
  .tier-header h2 {
    font-size: 1.1rem;
    font-weight: 600;
  }
  .tier-size {
    font-size: 0.85rem;
    color: var(--text-muted);
    font-weight: 500;
  }
  .tier-desc {
    font-size: 0.85rem;
    color: var(--text-secondary);
    line-height: 1.4;
    margin-bottom: 12px;
  }
  .tier-corpora {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .corpus-tag {
    font-size: 0.75rem;
    padding: 2px 8px;
    background: var(--bg-primary);
    border-radius: 12px;
    color: var(--text-muted);
  }
  .recommended {
    position: absolute;
    top: 8px;
    right: 12px;
    font-size: 0.7rem;
    color: var(--accent);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }
  .actions {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
  }
  .continue-btn {
    padding: 12px 40px;
    background: var(--accent);
    color: var(--text-on-accent);
    border-radius: var(--radius);
    font-weight: 500;
    font-size: 1rem;
  }
  .continue-btn:hover {
    background: var(--accent-hover);
  }
  .skip-link {
    font-size: 0.85rem;
    color: var(--text-muted);
    background: none;
    border: none;
    text-decoration: underline;
    cursor: pointer;
  }
  .skip-link:hover {
    color: var(--text-secondary);
  }
</style>
