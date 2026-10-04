<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  // What the last build did with the author's declared ontology.
  //
  // The one question the whole ontology-v1 program exists to answer —
  // "did my nouns reach the atoms?" — put where the author already
  // is, immediately after Build & enrich. It reads a VERDICT the build
  // wrote (`schema_validation.json`), not a live measurement, so it
  // reports what the last build found and nothing more.
  //
  // It renders NOTHING in three distinct situations, and they are
  // distinguished in the payload rather than collapsed: no corpus, no
  // report (`reported: false` — the report step never ran), and a
  // corpus that declares nothing (`reported: true`, no `ontology`).
  // Only the third is a normal steady state, and none of them is an
  // error worth a message here. A FAILED CALL is a fourth thing and
  // does get a line — see `loadError`.
  //
  // NOT the same thing as `CoverageCard` / `corpusCoverageCard()`,
  // which is XBRL financial-statement coverage. Similar word, unrelated
  // concept.
  import Card from "./Card.svelte";
  import { atlasBuildReport } from "../../api";
  import type { AtlasBuildReport } from "../../types";

  let { corpusId }: { corpusId: string | null } = $props();

  let report = $state<AtlasBuildReport | null>(null);
  /** Why the report could not be fetched, when that is the reason it is
   *  absent. The three empty states above are ANSWERS and stay silent; a
   *  throw is not one of them, and hiding it would leave an author whose
   *  card never appears with nothing to look at (§18.3). */
  let loadError = $state<string | null>(null);

  $effect(() => {
    const id = corpusId;
    if (!id) {
      report = null;
      return;
    }
    void (async () => {
      try {
        report = await atlasBuildReport(id);
        loadError = null;
      } catch (e) {
        // NOT the "nothing to report" path: an absent report and a corpus
        // that declares nothing both come back as a successful payload
        // (`reported: false` / no `ontology`). Reaching here means the call
        // itself failed, and that is worth one line rather than a card the
        // author never sees and cannot ask about.
        report = null;
        loadError = typeof e === "string" ? e : String(e);
      }
    })();
  });

  let ontology = $derived(report?.ontology ?? null);

  /** Declared types that produced no atoms at all. The headline
   *  failure — it is what the as-built probe measured (1 of 1 type
   *  surviving as 0 atoms) — so it is stated first and by name, never
   *  averaged into a percentage. */
  let zeroTypes = $derived(
    (ontology?.by_type ?? []).filter((t) => t.count_with_subtypes === 0),
  );

  /** A declared attribute whose type's atoms have no attributes slot
   *  at all. That is a DECLARATION defect (a `role_of` type lands as a
   *  State, and States carry no attributes), fixed in the recipe —
   *  not a model failure, and reporting it as "never filled" would
   *  send the author to the prompt for the wrong bug. */
  let unlandable = $derived(
    (ontology?.attribute_fill ?? []).filter(
      (f) => f.atoms > 0 && f.with_slot === 0,
    ),
  );

  /** Declared and could have been filled, and never was. */
  let unfilled = $derived(
    (ontology?.attribute_fill ?? []).filter(
      (f) => f.with_slot > 0 && f.filled === 0,
    ),
  );

  function fillLabel(f: {
    filled: number;
    with_slot: number;
    atoms: number;
  }): string {
    if (f.with_slot === 0) return `no slot on ${f.atoms} atoms`;
    return `${f.filled}/${f.with_slot}`;
  }
</script>

{#if loadError}
  <p class="report-unavailable" data-testid="build-report-error">
    Couldn't read the build report: {loadError}
  </p>
{:else if ontology}
  <div data-testid="build-report-card">
    <Card title="What the build made of your ontology" counter={`v${ontology.ontology_version}`}>
      <div class="report">
        <table class="types">
          <thead>
            <tr>
              <th scope="col">type</th>
              <th scope="col">kind</th>
              <th scope="col" class="num">atoms</th>
              <th scope="col">identity</th>
            </tr>
          </thead>
          <tbody>
            {#each ontology.by_type as t (t.name)}
              {@const criterion =
                ontology.identity.find((i) => i.type_name === t.name)
                  ?.criterion ?? "default:canonical_name"}
              <tr class:zero={t.count_with_subtypes === 0} data-testid="build-report-type">
                <th scope="row" class="name">{t.name}</th>
                <td class="kind">{t.kind}</td>
                <td class="num">
                  {t.count_with_subtypes.toLocaleString()}
                  {#if t.count_with_subtypes !== t.count}
                    <!-- The family total and the type's own count are
                         different numbers and the author needs both:
                         `coin` is 15 with `sceatta` and 13 without. -->
                    <span class="own" title="of which carry this exact type">
                      ({t.count.toLocaleString()} own)
                    </span>
                  {/if}
                </td>
                <td class="criterion" title={criterion}>{criterion}</td>
              </tr>
            {/each}
          </tbody>
        </table>

        {#if zeroTypes.length > 0}
          <p class="finding zero-finding" data-testid="build-report-zero">
            Declared and never extracted:
            {zeroTypes.map((t) => t.name).join(", ")}. Nothing in the corpus
            came back as one of these — check the guidance, or the type's
            description in the recipe.
          </p>
        {/if}

        {#if unlandable.length > 0}
          <p class="finding" data-testid="build-report-unlandable">
            Nowhere to land:
            {unlandable.map((f) => `${f.type_name}.${f.attribute}`).join(", ")}
            — these types' atoms carry no attributes at all (a
            <code>role_of</code> type becomes a State). Fix the declaration,
            not the prompt.
          </p>
        {/if}

        {#if unfilled.length > 0}
          <p class="finding" data-testid="build-report-unfilled">
            Never filled:
            {unfilled.map((f) => `${f.type_name}.${f.attribute}`).join(", ")}.
          </p>
        {/if}

        {#if (ontology.attribute_fill ?? []).length > 0}
          <details class="fills">
            <summary>Attribute fill ({ontology.attribute_fill.length})</summary>
            <ul>
              {#each ontology.attribute_fill as f (`${f.type_name}.${f.attribute}`)}
                <li>
                  <span class="mono">{f.type_name}.{f.attribute}</span>
                  <span class="fill">{fillLabel(f)}</span>
                </li>
              {/each}
            </ul>
          </details>
        {/if}

        <div class="pills">
          <span class="pill" title="Reified merges — same_as claims in the atlas">
            {ontology.same_as_claims} same_as
          </span>
          <span
            class="pill"
            title={ontology.merges == null
              ? "svrn enrich reconcile has not run on this corpus — not the same as zero merges"
              : "clusters the reconciler collapsed"}
          >
            {ontology.merges == null ? "merges not run" : `${ontology.merges} merged`}
          </span>
          {#if ontology.claims_missing_subject > 0}
            <span
              class="pill warn"
              title="A declared claim type names a subject; these claims came back without one, so the is-about link the type exists for is missing"
            >
              {ontology.claims_missing_subject} without a subject
            </span>
          {/if}
          {#if report?.grounding_fresh}
            <span class="pill ok" data-testid="build-report-grounding">grounding ready</span>
          {:else if report?.grounding_present}
            <span class="pill warn" data-testid="build-report-grounding">
              grounding stale — <code>svrn atlas backfill-ann</code>
            </span>
          {:else}
            <span class="pill warn" data-testid="build-report-grounding">
              not grounded — <code>svrn atlas backfill-ann</code>
            </span>
          {/if}
        </div>
      </div>
    </Card>
  </div>
{/if}

<style>
  .report {
    display: flex;
    flex-direction: column;
    gap: 0.55rem;
  }
  .types {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.8rem;
  }
  .types th,
  .types td {
    text-align: left;
    padding: 2px 6px 2px 0;
    font-weight: 400;
  }
  .types thead th {
    color: var(--muted, #8a8c93);
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    border-bottom: 1px solid var(--border, #2a2c33);
  }
  .types .name {
    font-family: var(--font-mono, monospace);
    font-weight: 500;
  }
  .types .kind,
  .types .criterion {
    color: var(--muted, #8a8c93);
  }
  .types .criterion {
    font-family: var(--font-mono, monospace);
    font-size: 0.72rem;
    max-width: 22ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .types .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .types .own {
    color: var(--muted, #8a8c93);
    font-size: 0.72rem;
  }
  .types tr.zero .name,
  .types tr.zero .num {
    color: var(--warn, #d08b3f);
  }
  .finding {
    margin: 0;
    font-size: 0.78rem;
    line-height: 1.45;
    color: var(--muted-bright, #b8bac1);
  }
  .zero-finding {
    color: var(--warn, #d08b3f);
  }
  .fills summary {
    cursor: pointer;
    font-size: 0.75rem;
    color: var(--muted, #8a8c93);
  }
  .fills ul {
    list-style: none;
    margin: 0.3rem 0 0;
    padding: 0;
    display: grid;
    gap: 2px;
    font-size: 0.75rem;
  }
  .fills li {
    display: flex;
    justify-content: space-between;
    gap: 0.6rem;
  }
  .fill {
    color: var(--muted, #8a8c93);
    font-variant-numeric: tabular-nums;
  }
  .mono,
  code {
    font-family: var(--font-mono, monospace);
  }
  .pills {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
  }
  .pill {
    padding: 1px 8px;
    border: 1px solid var(--border, #2a2c33);
    border-radius: 999px;
    font-size: 0.72rem;
    color: var(--muted-bright, #b8bac1);
  }
  .pill.ok {
    color: var(--ok, #5fa564);
    border-color: currentColor;
  }
  .pill.warn {
    color: var(--warn, #d08b3f);
    border-color: currentColor;
  }
  .report-unavailable {
    margin: 0.5rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted, #888);
  }
</style>
