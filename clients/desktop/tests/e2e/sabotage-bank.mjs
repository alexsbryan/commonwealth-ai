// SPDX-License-Identifier: AGPL-3.0-or-later
//
// THE MUTANT BANK — declared regressions the suite must catch.
//
// Each entry is a real, compiling change to real source that reproduces a
// regression a user could actually hit, together with the specs that claim to
// defend against it. `sabotage.mjs` applies each one, runs those specs, and
// requires them to go red.
//
// # Writing a mutant
//
//   target      path relative to the crate root. Production or test-harness
//               source — see the two layers below.
//   find        a substring that occurs EXACTLY ONCE in the target. The runner
//               reports STALE if that stops being true, which is the whole
//               anti-rot mechanism: a mutant that silently stops applying is
//               worse than no mutant, because the bank keeps reporting CAUGHT.
//   replace     must still COMPILE and still pass `npm run check`. A mutant
//               that breaks the build proves nothing about the tests — every
//               spec fails, for the wrong reason.
//   mustFail    the specs that must go red. Not "the suite" — the spec that
//               *claims this coverage*. A mutant caught by some unrelated spec
//               is weak evidence; a mutant caught by its own spec is proof.
//   breaks      the invariant, in the words its owner would use.
//   userImpact  what a person using the app would see. If this is hard to
//               write, the mutation is probably too synthetic to be worth
//               keeping.
//
// # The two layers
//
// LAYER 1 — mutate `tests/e2e/real/invariants.ts` (the assertion pack), caught
// by `specs/negative-controls.spec.ts`. This asks: if someone weakened a
// real-mode assertion, would anything notice? It matters because the real-mode
// suite runs NOWHERE in CI (ci.yml says so), so its assertions could be
// hollowed out by a passing PR. These mutants make CI the guard for a suite it
// never runs.
//
// LAYER 2 — mutate `src/` (the shipped frontend), caught by the synthetic
// specs. This asks the direct question: if the product regressed, would the
// gate hold? These are the ones whose SURVIVED verdict is a bug report.
//
// Layer 1 is meta and cheap; layer 2 is the point. Both are required — layer 2
// alone would leave the assertion pack unguarded, and layer 1 alone would only
// prove the tests test the tests.

export const BANK = [
  // ── Layer 1: the real-mode assertion pack ──
  {
    id: "pack-accepts-any-stream",
    suite: "synthetic",
    target: "tests/e2e/real/invariants.ts",
    breaks: "stream integrity — concat(message-chunk) === message-complete.full_text",
    userImpact:
      "the text that streamed in front of you is not the text that got saved; " +
      "re-opening the conversation shows something you never watched arrive",
    find: "  ).toBe(complete.full_text);",
    replace: "  ).toBeDefined();",
    mustFail: ["tests/e2e/specs/negative-controls.spec.ts"],
  },
  {
    id: "pack-ignores-sse-lag",
    suite: "synthetic",
    target: "tests/e2e/real/invariants.ts",
    breaks: "stream integrity — a lagged SSE consumer invalidates every stream assertion",
    userImpact:
      "nothing directly, but every stream assertion downstream silently becomes " +
      "an assertion about a turn with holes in it",
    find: 'expect(cap.lagged, "SSE consumer lagged — stream assertions invalid").toBe(false);',
    replace:
      'expect(cap.lagged, "SSE consumer lagged — stream assertions invalid").toBeDefined();',
    mustFail: ["tests/e2e/specs/negative-controls.spec.ts"],
  },
  {
    id: "pack-tolerates-duplicate-completes",
    suite: "synthetic",
    target: "tests/e2e/real/invariants.ts",
    breaks: "stream integrity — exactly one message-complete per message id",
    userImpact:
      "a turn finalises twice — the answer can render, then be replaced by a " +
      "second copy of itself",
    find: "  ).toBe(1);",
    replace: "  ).toBeGreaterThan(0);",
    mustFail: ["tests/e2e/specs/negative-controls.spec.ts"],
  },
  {
    id: "pack-accepts-missing-metadata",
    suite: "synthetic",
    target: "tests/e2e/real/invariants.ts",
    breaks: "glassbox — every turn carries metadata",
    userImpact:
      "the provenance surfaces (intent, sources, latency) go blank and the " +
      "answer becomes unauditable",
    find: 'expect(meta, "message-complete.metadata must be present").toBeTruthy();',
    replace: 'expect(meta, "message-complete.metadata must be present").toBeDefined();',
    mustFail: ["tests/e2e/specs/negative-controls.spec.ts"],
  },
  {
    id: "pack-accepts-dangling-citations",
    suite: "synthetic",
    target: "tests/e2e/real/invariants.ts",
    breaks: "citations — a local citation must dereference (the 79fdd04c partition-strand bug)",
    userImpact:
      "clicking a citation opens nothing; the answer cites a source the reading " +
      "desk cannot show, which is exactly how the stranded-corpus bug presented",
    find: "\n      ).toBeTruthy();",
    replace: "\n      ).toBeDefined();",
    mustFail: ["tests/e2e/specs/negative-controls.spec.ts"],
  },
  {
    id: "pack-lets-grounded-turns-cite-nothing",
    suite: "synthetic",
    target: "tests/e2e/real/invariants.ts",
    breaks: "citations — a knowledge-grounded turn must carry retrieved_chunks",
    userImpact:
      "an answer sourced from your library arrives with no sources attached — " +
      "indistinguishable, to the reader, from the model making it up",
    find: '      "knowledge-grounded turn must carry retrieved_chunks",\n    ).toBeGreaterThan(0);',
    replace:
      '      "knowledge-grounded turn must carry retrieved_chunks",\n    ).toBeGreaterThan(-1);',
    mustFail: ["tests/e2e/specs/negative-controls.spec.ts"],
  },
  {
    id: "pack-stops-believing-the-numeric-audit",
    suite: "synthetic",
    target: "tests/e2e/real/invariants.ts",
    breaks: "numeric honesty — the runtime's own 'not traceable' verdict is fatal",
    userImpact:
      "the runtime flags a figure it cannot trace to a source and the app ships " +
      "the answer anyway, with the warning dropped on the floor",
    find: "\n    ).toBe(false);",
    replace: "\n    ).toBeDefined();",
    mustFail: ["tests/e2e/specs/negative-controls.spec.ts"],
  },

  // ── Layer 2: the shipped frontend ──
  //
  // Every one of these is a regression someone could ship on a Tuesday. The
  // question each asks is not "is this code correct" but "does the spec that
  // owns this behaviour actually watch it".
  {
    id: "cold-load-line-never-retires",
    suite: "synthetic",
    target: "src/lib/components/CounterCard.svelte",
    breaks: "the model-load sub-line retires once tokens start arriving",
    userImpact:
      '"Loading Qwen3.6-35B off disk" stays stranded under "writing… 142 tokens" ' +
      "for the rest of the turn, hiding the real drafting line — the app looks " +
      "stuck while it is in fact answering",
    find: "heartbeat ? null : modelLoad",
    replace: "modelLoad",
    mustFail: ["tests/e2e/specs/counter-card.spec.ts"],
  },
  {
    id: "claim-check-counts-every-claim-as-confirmed",
    suite: "synthetic",
    target: "src/lib/components/CounterCard.svelte",
    breaks: "the verification tally counts only claims with a 'supported' verdict",
    userImpact:
      'the check station reads "2 of 2 confirmed" when one claim was NOT ' +
      "supported — the app overstates how well grounded its own answer is, " +
      "which is the single worst class of bug this product can have",
    find: 'check ? check.claims.filter((c) => c.verdict === "supported").length : 0',
    replace: "check ? check.claims.length : 0",
    mustFail: ["tests/e2e/specs/counter-card.spec.ts"],
  },
  {
    id: "layer-corpora-leak-into-the-scope-strip",
    suite: "synthetic",
    target: "src/lib/components/CorpusFilterStrip.svelte",
    breaks: "the scope strip shows one chip per PARENT corpus; layers stay hidden",
    userImpact:
      "internal layer corpora appear as their own toggles, so the scope the user " +
      "thinks they picked is not the allow-list that gets persisted and sent",
    find: 'c.status === "installed" && !c.parent_corpus_id && !isPartition(c.id),',
    replace: 'c.status === "installed" && !isPartition(c.id),',
    mustFail: ["tests/e2e/specs/corpus-filter-strip.spec.ts"],
    bluntKill:
      "every test in the file starts from the rendered chip set, so a wrong " +
      "chip list moves all five — verified as five named assertion failures, " +
      "not a page crash",
  },
  {
    id: "epistemic-footer-never-renders",
    suite: "synthetic",
    target: "src/lib/components/AssistantMessage.svelte",
    breaks: "a ledger-bearing turn renders the verdict receipt and source badges",
    userImpact:
      "every grounded answer silently falls back to legacy prose-parsed " +
      "attribution: no verdict receipt, no source badges, no abstention route " +
      "chip — the whole provenance surface vanishes and the answer still looks fine",
    find: 'typeof (ledger as EpistemicState).verdict === "string"',
    replace: 'typeof (ledger as EpistemicState).verdict === "number"',
    mustFail: ["tests/e2e/specs/epistemic-footer.spec.ts"],
  },
  {
    id: "orphaned-turn-never-recovers",
    suite: "synthetic",
    target: "src/lib/stores/liveTurns.svelte.ts",
    breaks: "returning to a conversation mid-stream re-attaches the live turn",
    userImpact:
      "navigate away from a slow turn and back: the question sits alone with no " +
      "spinner and the answer never lands",
    find: "return _turns[conversationId];",
    replace: "return undefined;",
    mustFail: ["tests/e2e/specs/chat-orphaned-turn.spec.ts"],
    bluntKill:
      "the whole spec is about navigating away and back, and every path through " +
      "it reads the live-turn registry — verified as three named assertion " +
      "failures (loading affordance, completed answer, failed turn), not a crash",
  },
  {
    id: "placeholder-never-appears",
    suite: "synthetic",
    target: "src/lib/components/ChatView.svelte",
    breaks: "a silent turn shows the placeholder at ~400ms instead of bare dots",
    userImpact:
      "on a fast-path query with no narration the user stares at three bouncing " +
      'dots for the whole wait instead of "Working on it…"',
    find: "const PLACEHOLDER_DELAY_MS = 400;",
    replace: "const PLACEHOLDER_DELAY_MS = 40000;",
    mustFail: ["tests/e2e/specs/chat-placeholder.spec.ts"],
  },
  {
    id: "library-empty-state-never-shows",
    suite: "synthetic",
    target: "src/lib/components/library/LibraryView.svelte",
    breaks: "an empty Library offers the Add CTA",
    userImpact:
      "a brand-new user with no notebooks sees a blank shelf and no way forward " +
      "— a dead-end first run, on the first screen that matters",
    find: "{:else if notebooks.length === 0}",
    replace: "{:else if notebooks.length < 0}",
    mustFail: ["tests/e2e/specs/library.spec.ts"],
  },
  {
    id: "byproduct-corpus-leaks-into-layer-chips",
    suite: "synthetic",
    target: "src/lib/components/KnowledgeStatus.svelte",
    breaks: "the internal on-demand-fetch corpus is hidden from the layer chips",
    userImpact:
      "an internal byproduct corpus shows up as a clickable Add/Remove toggle a " +
      "user can break their own index with",
    find: 'c.id !== "wikipedia-fetched" &&',
    replace: 'c.id !== "wikipedia-fetched-byproduct" &&',
    mustFail: ["tests/e2e/specs/knowledge-layers.spec.ts"],
  },
  {
    id: "notebook-resumes-the-oldest-thread",
    suite: "synthetic",
    target: "src/lib/components/library/NotebookDetail.svelte",
    breaks: "re-opening a notebook resumes its MOST RECENT conversation",
    userImpact:
      "you land in your oldest thread instead of the one you were just in, and " +
      "the work you were doing appears to be gone",
    find: "askConversationId = notebookConvs[0].id;",
    replace: "askConversationId = notebookConvs[notebookConvs.length - 1].id;",
    mustFail: ["tests/e2e/specs/notebook-memory.spec.ts"],
  },

  // ── Layer 3: source-first mutants ──
  //
  // Found 2026-07-28 by a SOURCE-FIRST probe. The layer-2 mutants above were
  // picked spec-first (read the spec, find the source it asserts on), which can
  // only ever produce mutations inside covered code — 16/16 CAUGHT was close to
  // tautological. Re-run with six mutants chosen by reading only `src/`, biased
  // toward defensive and incidental code, and scored against the WHOLE suite,
  // four survived; one of those (a fabricated install ETA) turned out to be
  // caught by `corpusProgress.test.ts` at the vitest layer, leaving three real
  // holes, each verified to survive the ENTIRE desktop gate.
  //
  // All three were closed on 2026-07-28 by the specs named in `mustFail` and
  // promoted from `knownHole` entries into ordinary blocking gates. They keep
  // their original wording — the `breaks`/`userImpact` text is what made the
  // case for writing the spec, and it is the right thing to read when one of
  // them goes red again.
  //
  // Keep this section source-first. A bank grown only spec-first trends toward
  // 100% while covering less and less; these three are the standing evidence
  // that the other method finds what the first cannot.
  {
    id: "delete-confirm-inverted",
    suite: "synthetic",
    target: "src/lib/components/ConversationList.svelte",
    breaks: "the hover ✕ arms first and deletes only on a second, confirming click",
    userImpact:
      "one mis-click on a ✕ that appears on hover permanently deletes a " +
      "conversation — no confirm, no undo, no toast",
    find: "    if (pendingDeleteId === id) {",
    replace: "    if (pendingDeleteId !== id) {",
    // Surgical: the two arm/confirm cases fail, while the right-click case
    // — a deliberately single-action path — stays green.
    mustFail: ["tests/e2e/specs/conversation-delete-confirm.spec.ts"],
  },
  {
    id: "memory-budget-guard-band",
    suite: "synthetic",
    target: "src/lib/components/SettingsPanel.svelte",
    breaks: "the save block fires at the critical band (>=95%), not the warn band",
    userImpact:
      "a genuinely over-budget model combination saves happily and the daemon " +
      "OOMs on load, while a merely-warned 85% config cannot be saved at all",
    find: '{@const blockSave = activeTab === "models" && budgetState === "crit"}',
    replace: '{@const blockSave = activeTab === "models" && budgetState === "warn"}',
    // Surgical: the crit case stops blocking and the warn case starts, while
    // the ok case is unaffected and stays green.
    mustFail: ["tests/e2e/specs/settings-memory-budget.spec.ts"],
  },
  {
    id: "inner-work-draft-persistence",
    suite: "synthetic",
    target: "src/lib/stores/innerWorkSession.svelte.ts",
    breaks: "a non-empty draft is written to localStorage rather than removed",
    userImpact:
      "everything typed in Inner Work is dropped when the window closes — the " +
      "autosave silently deletes instead of saving, and you return to a blank page",
    find: "    if (text.length === 0) {",
    replace: "    if (text.length >= 0) {",
    // Declared blunt, and legitimately so: the file IS the persistence
    // contract, and every case in it has to write a draft before it can
    // assert anything. There is no case here that a save which always
    // deletes could leave standing. The crash risk the blunt-kill warning
    // guards against does not apply — `saveDraft` is a pure localStorage
    // write inside a try/catch, so the mutation cannot take the page down.
    bluntKill:
      "both cases persist a draft before they assert anything else, so a save " +
      "that always deletes fails the whole file",
    mustFail: ["tests/e2e/specs/inner-work-draft-persistence.spec.ts"],
  },

  // ── The runner's own negative control ──
  //
  // Everything above is only as trustworthy as this script's ability to report
  // SURVIVED. A bug that made it see every run as failing — a bad exit-code
  // read, a spec path that matches nothing, a stray non-zero from npx — would
  // print a perfect score forever, and the perfect score is exactly what
  // nobody would think to question.
  //
  // So: a mutation that CANNOT affect the spec it runs. The Library's empty
  // state has nothing to do with the chat placeholder, and
  // chat-placeholder.spec.ts never renders LibraryView. The declared verdict
  // is SURVIVED. If this ever reports CAUGHT, the instrument is broken and
  // every other verdict in the run is worthless — which is what the script
  // says, loudly, rather than quietly scoring 19/19.
  {
    id: "self-control-unrelated-mutation",
    suite: "synthetic",
    selfControl: true,
    expectVerdict: "SURVIVED",
    target: "src/lib/components/library/LibraryView.svelte",
    breaks: "NOTHING the declared spec observes — this is the control, not a mutant",
    userImpact:
      "n/a — this entry exists to prove the runner can distinguish a killed " +
      "mutant from a surviving one",
    find: "{:else if notebooks.length === 0}",
    replace: "{:else if notebooks.length < 0}",
    mustFail: ["tests/e2e/specs/chat-placeholder.spec.ts"],
  },
];
