<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!--
  ConflictsPanel — the governance surface for a rule-set notebook.

  A steward's instrument (see the governance plan): it reconciles the
  rules the documents already state; it never authors rules. Friction is
  proportional to authority — "Not a conflict" is one click (dismissing
  detector noise is the steward's call), while "Keep this rule" / "Both
  can stand" carry a rationale (the community's decision). Exports are
  first-class: the agenda and the current-rules sheet are how the ~19
  people who never open the app actually use it.

  Data model: `governance_get_view` returns the joined read-model
  (`GovernanceView`) plus section titles, resolved chunk ids, topic
  names, recipe vocabulary, and per-op decision metadata. Every mutation
  refetches the whole view (no optimistic state) and calls `onChanged`
  so the shelf badge stays in sync.
-->
<script lang="ts">
  import {
    governanceGetView,
    governanceResolve,
    governanceAccept,
    governanceDismiss,
    governanceUndoTension,
    governanceExportWrite,
    enrichBuildAsync,
  } from "../../api";
  import { enrichProgressStore } from "../../stores/enrichProgress.svelte";
  import { readingNavigation } from "../../stores/readingNavigation.svelte";
  import { save } from "@tauri-apps/plugin-dialog";
  import EnrichmentStage from "../EnrichmentStage.svelte";
  import type {
    GovernanceViewPayload,
    RuleView,
    TensionView,
  } from "../../types";

  let {
    corpusId,
    notebookName,
    onChanged,
  }: {
    corpusId: string;
    notebookName: string;
    /** Fired after any change, so the Library shelf's conflict badge
     *  refreshes. */
    onChanged?: () => void;
  } = $props();

  let payload = $state<GovernanceViewPayload | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  // Per-conflict in-flight + error state (keyed by tension id).
  let rowBusy = $state<Record<string, boolean>>({});
  let rowError = $state<Record<string, string>>({});

  // The inline decision form open on a card, if any.
  type PendingAction =
    | { tensionId: string; kind: "resolve"; keepRuleId: string; keepText: string; dropText: string }
    | { tensionId: string; kind: "accept" };
  let pending = $state<PendingAction | null>(null);
  let rationale = $state("");

  // Collapsible history groups.
  let settledOpen = $state(false);
  let dismissedOpen = $state(false);

  // Transient labels for the export affordances.
  let copyLabel = $state("Copy agenda");
  let exportLabel = $state("Export current rules");

  // "Update from documents" build, if running.
  let updating = $state(false);
  let updateJob = $derived(enrichProgressStore.byCorpus(corpusId)[0] ?? null);

  // ── Load ──────────────────────────────────────────────────────────
  async function refetch() {
    try {
      payload = await governanceGetView(corpusId);
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }
  $effect(() => {
    void refetch();
  });

  // ── Vocabulary (recipe labels, with generic fallbacks) ────────────
  let ruleWord = $derived(payload?.vocabulary?.position_term || "rule");
  let conflictWord = $derived(payload?.vocabulary?.tension_term || "conflict");
  function cap(s: string): string {
    return s ? s[0].toUpperCase() + s.slice(1) : s;
  }

  // ── Derived views ─────────────────────────────────────────────────
  let rulesById = $derived(
    new Map<string, RuleView>((payload?.view.rules ?? []).map((r) => [r.id, r])),
  );

  let openTensions = $derived(
    (payload?.view.tensions ?? []).filter(
      (t) => t.disposition.disposition === "open",
    ),
  );
  let settledTensions = $derived(
    (payload?.view.tensions ?? [])
      .filter((t) =>
        ["resolved", "accepted", "moot"].includes(t.disposition.disposition),
      )
      .sort((a, b) => decisionTs(b) - decisionTs(a)),
  );
  let dismissedTensions = $derived(
    (payload?.view.tensions ?? [])
      .filter((t) => t.disposition.disposition === "dismissed")
      .sort((a, b) => decisionTs(b) - decisionTs(a)),
  );

  // Only issues that are genuinely actionable drift (the backend already
  // filters normal weekly variance out).
  let issues = $derived(payload?.view.issues ?? []);

  function decisionTs(t: TensionView): number {
    const d = t.disposition;
    if (d.disposition === "open" || d.disposition === "moot") return 0;
    return payload?.decisions[d.by]?.ts_unix ?? 0;
  }
  function decisionRationale(t: TensionView): string {
    const d = t.disposition;
    if (d.disposition === "open" || d.disposition === "moot") return "";
    return payload?.decisions[d.by]?.rationale ?? "";
  }

  // ── Source labelling + passage viewing ────────────────────────────
  function sourceTitle(ruleId: string): string {
    const rule = rulesById.get(ruleId);
    const section = rule?.citation?.chunk_id;
    if (section && payload?.section_titles[section]) {
      return payload.section_titles[section];
    }
    return "Source";
  }
  function passagePreview(ruleId: string): string | undefined {
    return rulesById.get(ruleId)?.citation?.passage_preview;
  }
  function chunkFor(ruleId: string): number | undefined {
    const section = rulesById.get(ruleId)?.citation?.chunk_id;
    return section ? payload?.section_chunks[section] : undefined;
  }
  function viewPassage(ruleId: string) {
    const chunk = chunkFor(ruleId);
    if (chunk == null) return;
    readingNavigation.requestChunk(
      corpusId,
      chunk,
      `via ${notebookName} — ${cap(conflictWord)}`,
    );
  }

  // ── Mutations (all refetch + notify; no optimistic state) ─────────
  async function withRow(tensionId: string, fn: () => Promise<unknown>) {
    rowBusy = { ...rowBusy, [tensionId]: true };
    rowError = { ...rowError, [tensionId]: "" };
    try {
      await fn();
      pending = null;
      rationale = "";
      await refetch();
      onChanged?.();
    } catch (e) {
      rowError = {
        ...rowError,
        [tensionId]: e instanceof Error ? e.message : String(e),
      };
    } finally {
      rowBusy = { ...rowBusy, [tensionId]: false };
    }
  }

  function todayStamp(): string {
    return new Date().toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }

  function startResolve(t: TensionView, keepRuleId: string) {
    const keepText = keepRuleId === t.rule_a ? t.text_a : t.text_b;
    const dropText = keepRuleId === t.rule_a ? t.text_b : t.text_a;
    pending = { tensionId: t.id, kind: "resolve", keepRuleId, keepText, dropText };
    rationale = `Meeting — ${todayStamp()}`;
  }
  function startAccept(t: TensionView) {
    pending = { tensionId: t.id, kind: "accept" };
    rationale = "";
  }
  function cancelPending() {
    pending = null;
    rationale = "";
  }

  async function confirmResolve() {
    if (pending?.kind !== "resolve") return;
    const { tensionId, keepRuleId } = pending;
    await withRow(tensionId, () =>
      governanceResolve(corpusId, tensionId, keepRuleId, rationale.trim()),
    );
  }
  async function confirmAccept() {
    if (pending?.kind !== "accept") return;
    const { tensionId } = pending;
    await withRow(tensionId, () =>
      governanceAccept(corpusId, tensionId, rationale.trim()),
    );
  }
  async function dismiss(t: TensionView) {
    await withRow(t.id, () => governanceDismiss(corpusId, t.id));
  }
  async function undo(t: TensionView) {
    await withRow(t.id, () => governanceUndoTension(corpusId, t.id));
  }

  // ── "Update from documents" (weekly rebuild) ──────────────────────
  async function updateFromDocuments() {
    updating = true;
    try {
      const handle = await enrichBuildAsync(corpusId, null, null);
      await enrichProgressStore.track(handle);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      updating = false;
    }
  }
  function onUpdateTerminal(kind: string) {
    updating = false;
    if (kind === "complete") {
      void refetch();
      onChanged?.();
    }
  }

  // ── Exports (the multi-user interface) ────────────────────────────
  function dispositionLabel(t: TensionView): string {
    const d = t.disposition.disposition;
    if (d === "resolved") return "Resolved";
    if (d === "accepted") return "Both stand";
    if (d === "dismissed") return "Not a conflict";
    if (d === "moot") return "Superseded by a later decision";
    return "Open";
  }

  function agendaMarkdown(): string {
    const lines: string[] = [
      `# ${cap(conflictWord)}s to settle — ${notebookName} — ${todayStamp()}`,
      "",
    ];
    if (openTensions.length === 0) {
      lines.push(`No open ${conflictWord}s — current ${ruleWord}s are consistent.`);
    }
    openTensions.forEach((t, i) => {
      lines.push(`## ${i + 1}. ${t.why ?? "Which rule stands?"}`);
      lines.push(`- **${sourceTitle(t.rule_a)}:** "${t.text_a}"`);
      lines.push(`- **${sourceTitle(t.rule_b)}:** "${t.text_b}"`);
      if (t.why) lines.push(`- Question to settle: ${t.why}`);
      lines.push("");
    });
    return lines.join("\n");
  }

  function currentRulesMarkdown(): string {
    const active = (payload?.view.rules ?? []).filter(
      (r) => r.status.status === "active",
    );
    const groups = new Map<string, RuleView[]>();
    for (const r of active) {
      const topic =
        (r.scope && payload?.scope_names[r.scope]) || "General";
      const arr = groups.get(topic) ?? [];
      arr.push(r);
      groups.set(topic, arr);
    }
    const lines: string[] = [
      `# Current ${ruleWord}s — ${notebookName} — ${todayStamp()}`,
      "",
    ];
    for (const topic of [...groups.keys()].sort()) {
      lines.push(`## ${topic}`);
      for (const r of groups.get(topic)!.sort((a, b) => a.text.localeCompare(b.text))) {
        const deontic = r.deontic ? ` *(${r.deontic})*` : "";
        lines.push(`- ${r.text}${deontic} — ${sourceTitle(r.id)}`);
      }
      lines.push("");
    }
    return lines.join("\n");
  }

  async function copyAgenda() {
    try {
      await navigator.clipboard.writeText(agendaMarkdown());
      copyLabel = "Copied";
      setTimeout(() => (copyLabel = "Copy agenda"), 1500);
    } catch (e) {
      console.warn("copy agenda failed:", e);
    }
  }

  async function exportRules() {
    try {
      const path = await save({
        defaultPath: `current-rules-${new Date().toISOString().slice(0, 10)}.md`,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!path) return;
      await governanceExportWrite(path, currentRulesMarkdown());
      exportLabel = "Exported";
      setTimeout(() => (exportLabel = "Export current rules"), 1500);
    } catch (e) {
      console.warn("export current rules failed:", e);
    }
  }
</script>

<div class="conflicts">
  {#if loading}
    <p class="state">Loading…</p>
  {:else if error}
    <p class="state err">{error}</p>
  {:else if payload}
    <!-- Weekly cycle: documents changed → re-check. -->
    {#if updating && updateJob}
      <div class="update-stage">
        <p class="update-title">Re-reading the documents…</p>
        <EnrichmentStage job={updateJob} onTerminal={onUpdateTerminal} />
      </div>
    {:else if payload.docs_changed_since_build}
      <div class="banner">
        <div>
          <strong>Documents have changed since the last check.</strong>
          <span class="banner-sub">
            Update to pick up new decisions — anything already settled stays
            settled.
          </span>
        </div>
        <button class="btn primary" onclick={updateFromDocuments}>
          Update from documents
        </button>
      </div>
    {/if}

    <!-- Glass-box: decisions that no longer match the documents. -->
    {#if issues.length > 0}
      <div class="needs-attention">
        <strong>{issues.length} {issues.length === 1 ? "item needs" : "items need"} attention</strong>
        <p>
          Some past decisions no longer line up with the current documents —
          a {ruleWord}'s wording may have changed. Re-adjudicate them below.
        </p>
      </div>
    {/if}

    <!-- Header + exports (always available). -->
    <div class="head">
      <h2>
        {#if openTensions.length > 0}
          {openTensions.length} open {openTensions.length === 1 ? conflictWord : conflictWord + "s"}
        {:else}
          No open {conflictWord}s
        {/if}
      </h2>
      <div class="head-actions">
        <button class="btn quiet" onclick={copyAgenda} disabled={openTensions.length === 0}>
          {copyLabel}
        </button>
        <button class="btn quiet" onclick={exportRules}>{exportLabel}</button>
      </div>
    </div>

    <!-- Open conflicts — the ranked agenda. -->
    {#if openTensions.length === 0}
      <p class="allclear">Current {ruleWord}s are consistent.</p>
    {:else}
      <ul class="cards">
        {#each openTensions as t (t.id)}
          <li class="card" data-testid="conflict-card">
            {#if t.why}<p class="crux">{t.why}</p>{/if}
            <div class="sides">
              {#each [{ id: t.rule_a, text: t.text_a }, { id: t.rule_b, text: t.text_b }] as side (side.id)}
                <div class="side">
                  <div class="side-src">{sourceTitle(side.id)}</div>
                  <blockquote class="side-text">{side.text}</blockquote>
                  {#if passagePreview(side.id)}
                    <p class="preview">{passagePreview(side.id)}</p>
                  {/if}
                  {#if chunkFor(side.id) != null}
                    <button class="link" onclick={() => viewPassage(side.id)}>
                      View passage
                    </button>
                  {/if}
                  {#if pending?.tensionId !== t.id}
                    <button
                      class="btn keep"
                      onclick={() => startResolve(t, side.id)}
                      disabled={rowBusy[t.id]}
                    >
                      Keep this {ruleWord}
                    </button>
                  {/if}
                </div>
              {/each}
            </div>

            {#if pending?.tensionId === t.id && pending.kind === "resolve"}
              <div class="decide">
                <p class="decide-summary">
                  Keep <em>“{pending.keepText}”</em>; the other {ruleWord} is
                  superseded and drops out of current law.
                </p>
                <textarea
                  bind:value={rationale}
                  rows="2"
                  placeholder="How was this decided?"
                ></textarea>
                <div class="decide-actions">
                  <button class="btn quiet" onclick={cancelPending}>Cancel</button>
                  <button class="btn primary" onclick={confirmResolve} disabled={rowBusy[t.id]}>
                    Confirm
                  </button>
                </div>
              </div>
            {:else if pending?.tensionId === t.id && pending.kind === "accept"}
              <div class="decide">
                <p class="decide-summary">
                  Both {ruleWord}s remain in force — record why this
                  contradiction is tolerated.
                </p>
                <textarea
                  bind:value={rationale}
                  rows="2"
                  placeholder="Why do both stand? (required)"
                ></textarea>
                <div class="decide-actions">
                  <button class="btn quiet" onclick={cancelPending}>Cancel</button>
                  <button
                    class="btn primary"
                    onclick={confirmAccept}
                    disabled={rowBusy[t.id] || rationale.trim().length === 0}
                  >
                    Confirm
                  </button>
                </div>
              </div>
            {:else}
              <div class="card-actions">
                <button class="btn quiet" onclick={() => startAccept(t)} disabled={rowBusy[t.id]}>
                  Both can stand
                </button>
                <button
                  class="btn quiet"
                  data-testid="conflict-dismiss"
                  onclick={() => dismiss(t)}
                  disabled={rowBusy[t.id]}
                >
                  Not a {conflictWord}
                </button>
              </div>
            {/if}
            {#if rowError[t.id]}<p class="row-err">{rowError[t.id]}</p>{/if}
          </li>
        {/each}
      </ul>
    {/if}

    <!-- Settled — the living history. -->
    {#if settledTensions.length > 0}
      <div class="group">
        <button class="group-head" onclick={() => (settledOpen = !settledOpen)}>
          {settledOpen ? "▾" : "▸"} Settled ({settledTensions.length})
        </button>
        {#if settledOpen}
          <ul class="history">
            {#each settledTensions as t (t.id)}
              <li class="hrow">
                <div class="hrow-main">
                  <span class="hbadge">{dispositionLabel(t)}</span>
                  <span class="hcrux">{t.why ?? `${sourceTitle(t.rule_a)} vs ${sourceTitle(t.rule_b)}`}</span>
                </div>
                {#if decisionRationale(t)}<p class="hrationale">{decisionRationale(t)}</p>{/if}
                {#if t.disposition.disposition !== "moot"}
                  <button class="link" onclick={() => undo(t)} disabled={rowBusy[t.id]}>
                    Undo
                  </button>
                {/if}
                {#if rowError[t.id]}<p class="row-err">{rowError[t.id]}</p>{/if}
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    {/if}

    <!-- Dismissed — guilt-free, recoverable. -->
    {#if dismissedTensions.length > 0}
      <div class="group">
        <button class="group-head" onclick={() => (dismissedOpen = !dismissedOpen)}>
          {dismissedOpen ? "▾" : "▸"} Dismissed ({dismissedTensions.length})
        </button>
        {#if dismissedOpen}
          <ul class="history">
            {#each dismissedTensions as t (t.id)}
              <li class="hrow">
                <div class="hrow-main">
                  <span class="hcrux">{t.why ?? `${sourceTitle(t.rule_a)} vs ${sourceTitle(t.rule_b)}`}</span>
                </div>
                <button class="link" onclick={() => undo(t)} disabled={rowBusy[t.id]}>
                  Undo
                </button>
                {#if rowError[t.id]}<p class="row-err">{rowError[t.id]}</p>{/if}
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .conflicts {
    padding: 20px 24px;
    max-width: 860px;
    margin: 0 auto;
    overflow-y: auto;
  }
  .state { color: var(--text-secondary, #999); }
  .state.err { color: var(--error, #d27979); }

  .banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 12px 16px;
    margin-bottom: 16px;
    border: 1px solid color-mix(in oklch, var(--accent, #c4a46a) 40%, var(--border, #333));
    border-radius: var(--radius, 6px);
    background: color-mix(in oklch, var(--accent, #c4a46a) 8%, transparent);
  }
  .banner-sub { display: block; font-size: 0.82rem; color: var(--text-secondary, #999); margin-top: 2px; }
  .update-stage { margin-bottom: 16px; }
  .update-title { font-weight: 600; margin: 0 0 8px; }

  .needs-attention {
    padding: 12px 16px;
    margin-bottom: 16px;
    border: 1px solid color-mix(in oklch, var(--error, #d27979) 35%, var(--border, #333));
    border-radius: var(--radius, 6px);
    background: color-mix(in oklch, var(--error, #d27979) 8%, transparent);
  }
  .needs-attention p { margin: 4px 0 0; font-size: 0.85rem; color: var(--text-secondary, #999); }

  .head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }
  .head h2 { margin: 0; font-size: 1.1rem; color: var(--text-primary, #eee); }
  .head-actions { display: flex; gap: 8px; }

  .allclear { color: var(--text-secondary, #999); padding: 8px 0 16px; }

  .cards { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 14px; }
  .card {
    border: 1px solid var(--border, #333);
    border-radius: var(--radius, 6px);
    background: var(--bg-secondary, #1a1a1a);
    padding: 16px;
  }
  .crux { margin: 0 0 12px; font-weight: 600; color: var(--text-primary, #eee); }
  .sides { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
  @media (max-width: 620px) { .sides { grid-template-columns: 1fr; } }
  .side {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px;
    border: 1px solid var(--border, #333);
    border-radius: var(--radius, 6px);
    background: var(--bg-primary, #111);
  }
  .side-src {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: color-mix(in oklch, var(--accent, #c4a46a) 70%, var(--text-muted, #888));
  }
  .side-text {
    margin: 0;
    font-size: 1.05rem;
    line-height: 1.4;
    color: var(--text-primary, #eee);
    border-left: 2px solid var(--border, #333);
    padding-left: 10px;
  }
  .preview { margin: 0; font-size: 0.8rem; color: var(--text-secondary, #999); font-style: italic; }

  .decide {
    margin-top: 14px;
    padding: 12px;
    border: 1px dashed color-mix(in oklch, var(--accent, #c4a46a) 40%, var(--border, #333));
    border-radius: var(--radius, 6px);
  }
  .decide-summary { margin: 0 0 8px; font-size: 0.9rem; color: var(--text-primary, #eee); }
  .decide-summary em { color: var(--accent, #c4a46a); font-style: normal; }
  .decide textarea {
    width: 100%;
    box-sizing: border-box;
    resize: vertical;
    font: inherit;
    padding: 8px;
    border: 1px solid var(--border, #333);
    border-radius: var(--radius, 6px);
    background: var(--bg-primary, #111);
    color: var(--text-primary, #eee);
  }
  .decide-actions, .card-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    margin-top: 10px;
  }

  .btn {
    font: inherit;
    cursor: pointer;
    padding: 5px 12px;
    border-radius: var(--radius, 6px);
    border: 1px solid var(--border, #333);
    background: var(--bg-elevated, #222);
    color: var(--text-primary, #eee);
    font-size: 0.82rem;
    font-weight: 550;
  }
  .btn:hover:not(:disabled) { border-color: var(--accent, #c4a46a); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .btn.primary {
    background: color-mix(in oklch, var(--accent, #c4a46a) 18%, transparent);
    border-color: color-mix(in oklch, var(--accent, #c4a46a) 45%, var(--border, #333));
  }
  .btn.keep { align-self: flex-start; }
  .btn.quiet { background: transparent; }
  .link {
    align-self: flex-start;
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
    color: var(--accent, #c4a46a);
    font: inherit;
    font-size: 0.8rem;
    text-decoration: underline;
  }
  .link:disabled { opacity: 0.5; cursor: default; }
  .row-err { color: var(--error, #d27979); font-size: 0.8rem; margin: 8px 0 0; }

  .group { margin-top: 18px; border-top: 1px solid var(--border, #333); padding-top: 10px; }
  .group-head {
    background: none;
    border: none;
    cursor: pointer;
    color: var(--text-secondary, #999);
    font: inherit;
    font-weight: 600;
    padding: 4px 0;
  }
  .history { list-style: none; margin: 8px 0 0; padding: 0; display: flex; flex-direction: column; gap: 10px; }
  .hrow {
    padding: 10px 12px;
    border: 1px solid var(--border, #333);
    border-radius: var(--radius, 6px);
    background: var(--bg-secondary, #1a1a1a);
  }
  .hrow-main { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .hbadge {
    font-size: 0.72rem;
    padding: 1px 8px;
    border-radius: 999px;
    background: color-mix(in oklch, var(--success, #7fb86a) 18%, transparent);
    color: color-mix(in oklch, var(--success, #7fb86a) 85%, var(--text-primary, #eee));
    white-space: nowrap;
  }
  .hcrux { color: var(--text-primary, #eee); font-size: 0.9rem; }
  .hrationale { margin: 6px 0; font-size: 0.82rem; color: var(--text-secondary, #999); }
</style>
