<!--
  FirstCorpusFlow — post-setup onboarding surface.

  Mounted by App.svelte AFTER the SetupWizard completes, ONLY on the
  very first launch (marker file `~/.sovereign/first_run_complete`
  absent AND no enriched corpora yet). Runs the user through:

    1. Explainer — three-paragraph honest-expectations block plus a
       "better with a mesh" callout.
    2. Pick method — folder drop, Obsidian vault, or skip.
    3. Embedded FolderDropFlow handles the rest (pre-scan → ingest →
       atlas gate → enriching → celebration), and we route the
       starter-question click back up to App.svelte via
       `onOpenChatWithSeed`.

  Skipping goes straight to chat. Completing the atlas-complete screen
  in the child flow bubbles up a selected starter; we hand it to
  `onComplete(seed)` so App can transition + seed the chat.
-->
<script lang="ts">
  import { markFirstRunComplete } from "../../api";
  import type { StarterQuestion } from "../../types";
  import FolderDropFlow from "../local-knowledge/folder/FolderDropFlow.svelte";
  import HonestExpectations from "./HonestExpectations.svelte";
  import InkStamp from "./InkStamp.svelte";
  import MeshBoost from "./MeshBoost.svelte";

  interface Props {
    /// Called exactly once when the user is ready to land in chat.
    /// `seed` is the question to auto-submit, or null when the user
    /// skipped / closed out without picking a starter.
    onComplete: (seed: StarterQuestion | null) => void;
    /// Called when the user clicks "Start chatting — atlas keeps
    /// building" while the sample atlas is mid-flight. App.svelte
    /// marks onboarding complete and flips to the chat view; the
    /// atlas subprocess continues running. Toast fires when it
    /// finishes, chat empty-state chips populate.
    onDropToChat?: () => void;
  }

  let { onComplete, onDropToChat }: Props = $props();

  type Step =
    | { kind: "explainer" }
    | {
        kind: "picking_flow";
        sourceType: "folder" | "obsidian";
      };

  let step: Step = $state({ kind: "explainer" });

  async function finish(seed: StarterQuestion | null) {
    // Mark first-run complete regardless of outcome so subsequent
    // launches don't trap the user in onboarding. If write fails
    // (disk full, permission error), fall through anyway — the UX
    // is non-blocking; on next launch the corpus-exists branch of
    // the App routing also skips onboarding.
    try {
      await markFirstRunComplete();
    } catch (e) {
      console.warn("markFirstRunComplete failed — continuing:", e);
    }
    onComplete(seed);
  }

  function pickFolder() {
    step = { kind: "picking_flow", sourceType: "folder" };
  }
  function pickObsidian() {
    step = { kind: "picking_flow", sourceType: "obsidian" };
  }
  function skip() {
    void finish(null);
  }

  function handleChildExit() {
    // User clicked Done on atlas_complete / FolderCompletePanel /
    // ingest error. Either way, they're ready to drop to chat.
    void finish(null);
  }

  function handleChildStarterPick(question: StarterQuestion) {
    void finish(question);
  }

  async function handleChildDropToChat() {
    // Mark onboarding complete then hand off — App.svelte flips the
    // view to `chat` via the provided callback.
    try {
      await markFirstRunComplete();
    } catch (e) {
      console.warn("markFirstRunComplete failed — continuing:", e);
    }
    onDropToChat?.();
  }
</script>

<div class="onboarding">
  {#if step.kind === "explainer"}
    <div class="explainer">
      <header class="head">
        <div class="mark">
          <InkStamp size="lg" active={true} />
        </div>
        <h1 class="title">
          Your knowledge,<br />
          <span class="title-italic">on your machine.</span>
        </h1>
        <p class="lede">
          Sovereign is strongest when it's reading from content you chose.
          Connect a first source — then ask questions grounded in what's
          actually there.
        </p>
      </header>

      <HonestExpectations />

      <MeshBoost emphasis="active" />

      <section class="tiles" aria-label="Connect a source">
        <button type="button" class="tile" onclick={pickFolder}>
          <span class="tile-title">Drop a folder</span>
          <span class="tile-body">
            PDFs, text files, markdown. Good for research notes, papers,
            scanned archives.
          </span>
        </button>
        <button type="button" class="tile" onclick={pickObsidian}>
          <span class="tile-title">Connect an Obsidian vault</span>
          <span class="tile-body">
            Point at your vault. Sovereign indexes the notes (and, if you
            choose, writes back cluster tags value-perfectly).
          </span>
        </button>
        <button type="button" class="tile tile--quiet" onclick={skip}>
          <span class="tile-title">Skip for now</span>
          <span class="tile-body">
            Go straight to chat. You can connect a source later from
            Settings → Local Knowledge.
          </span>
        </button>
      </section>
    </div>
  {:else if step.kind === "picking_flow"}
    <div class="flow-wrap">
      <FolderDropFlow
        sourceType={step.sourceType}
        initialPath={null}
        onExit={handleChildExit}
        onOpenChatWithSeed={handleChildStarterPick}
        onDropToChat={handleChildDropToChat}
      />
    </div>
  {/if}
</div>

<style>
  /* App root pins `html,body,#app { overflow: hidden }` (see
     app.css) so the onboarding container is responsible for its
     own scrolling. `height: 100vh` + `overflow-y: auto` gives us a
     viewport-sized scrollable region; `min-height` would let the
     content grow past the viewport with no way to reach it. */
  .onboarding {
    height: 100vh;
    box-sizing: border-box;
    padding: 36px 40px 56px;
    max-width: 860px;
    margin: 0 auto;
    overflow-y: auto;
    color: var(--text-primary);
    animation: fade-in 320ms ease-out both;
  }
  .explainer { display: flex; flex-direction: column; gap: 24px; }

  .head {
    display: flex;
    flex-direction: column;
    gap: 12px;
    align-items: flex-start;
    padding-bottom: 4px;
  }
  .mark { display: inline-flex; }
  .title {
    margin: 0;
    font-size: 2.1rem;
    font-weight: 600;
    line-height: 1.06;
    letter-spacing: -0.02em;
    color: var(--text-primary);
  }
  /* Italic serif second line — the "editorial voice" beat. Georgia
     italic is the only point in the entire onboarding where we
     reach for a serif; it carries the reading-letter cadence the
     README's voice sets up. */
  .title-italic {
    font-family: var(--font-serif);
    font-style: italic;
    font-weight: 400;
    color: var(--accent-light);
  }
  .lede {
    margin: 0;
    font-size: 1rem;
    color: var(--text-secondary);
    line-height: 1.55;
    max-width: 60ch;
  }

  .tiles {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
    gap: 10px;
  }
  /* Tile is a <button>. Lock every color to a palette token so
     user-agent button defaults never surface on the plum
     background — this was the readability miss on the last pass. */
  .tile {
    display: flex;
    flex-direction: column;
    gap: 6px;
    text-align: left;
    background: var(--bg-surface);
    border: 1px solid var(--border-mid);
    border-radius: 10px;
    padding: 14px 16px;
    cursor: pointer;
    color: var(--text-primary);
    font-family: var(--font-sans);
    transition:
      border-color 160ms ease,
      background 160ms ease,
      transform 160ms ease,
      box-shadow 160ms ease;
  }
  .tile:hover {
    border-color: var(--accent);
    background: color-mix(in oklab, var(--accent) 8%, var(--bg-surface));
    box-shadow: 0 4px 18px var(--accent-glow);
    transform: translateY(-1px);
  }
  .tile:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }
  .tile--quiet {
    background: transparent;
    border-style: dashed;
    border-color: var(--border-bright);
  }
  .tile--quiet:hover {
    border-style: solid;
    border-color: var(--lavender);
    background: var(--lavender-dim);
    box-shadow: none;
  }
  .tile-title {
    font-size: 0.92rem;
    color: var(--text-primary);
    font-weight: 600;
    letter-spacing: -0.005em;
  }
  .tile-body {
    font-size: 0.8rem;
    color: var(--text-secondary);
    line-height: 1.45;
  }

  .flow-wrap {
    /* Give the embedded FolderDropFlow its own breathing room without
       stacking an extra wrapper style that would conflict with its
       internal animations. */
    animation: fade-in 260ms ease-out both;
  }

  @keyframes fade-in {
    from { opacity: 0; transform: translateY(4px); }
    to   { opacity: 1; transform: translateY(0);   }
  }
</style>
