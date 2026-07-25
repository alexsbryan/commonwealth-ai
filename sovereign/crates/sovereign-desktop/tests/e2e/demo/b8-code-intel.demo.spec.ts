// SPDX-License-Identifier: AGPL-3.0-or-later
// B8 — Ask your codebase a question you can't grep for.
//
// Thesis and answer-key: sovereign/docs/specs/CODE_INTEL_CHAT.md. The
// audience is the CTO who can smell BS: the buy-trigger is "turn the
// subsystem I'm afraid to touch into one I can change with confidence",
// and the moat is that the code never leaves the laptop.
//
// The "pop" test is the whole thesis and it is enforced HERE, at spec
// level, not left to good intentions: the question must name no symbol
// from the expected answer. "Callers of gate_answer" has zero pop — you
// already named it. If someone later "fixes" a flaky beat by naming the
// function, the assertion below fails before the footage is ever cut.
//
// The anti-hallucination gate is the file-exists check. A plausible
// `src/inference/router.rs` that isn't on disk is exactly the failure
// this audience is scanning for, and it is the one thing a viewer can
// check themselves.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { beatTest, expect, demoClick } from "./beat";
import { realBootToChat } from "./demo-base";
import { hostedCorpora } from "./preflight";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "../../../../../..");

// Code corpora that all mean "this codebase". Retrieval choosing among
// them is correct behaviour, not drift.
const CODE_CORPORA = ["commonwealth-ai", "sovereign", "corpus-engine", "commonwealth"];

/** The notebook this beat asks. Scoped deliberately, for two reasons.
 *
 *  1. CODE_INTEL_CHAT.md §5 Inc 2 gates the call-graph tool-loop on "when a
 *     code corpus is **scoped**" — scoping is the trigger, not decoration.
 *  2. Asked unscoped, retrieval spreads across the whole 33-notebook shelf.
 *     Observed 2026-07-24: it drew from `wikipedia`, `obsidian-vault` and
 *     `conversations-personal` alongside the code corpora, and the answer
 *     cited `remote.rs` — a file that exists nowhere in this repo's source.
 *     B1 asks unscoped on purpose because landing on `sep` IS its claim;
 *     here dilution is just dilution.
 *
 *  `commonwealth-ai` specifically because it is the superset repo AND the
 *  only code corpus with a `scip_graph.db` on this machine — no call
 *  graph, no traversal. */
const SCOPE_CORPUS = "commonwealth-ai";

/** The conceptual→symbol bridge (CODE_INTEL_CHAT.md §5 Inc 1): per-symbol
 *  intent summaries, embedded alongside the code chunks, which is what
 *  lets a plain-English question rank a *function* above a raw chunk.
 *  Without it this beat does not fail honestly — it fabricates a
 *  plausible path, which is the one outcome this audience is scanning
 *  for. So its absence is a precondition, not an assertion.
 *
 *  Built by `sovereign enrich code-intel <corpus>`; the pass drops a
 *  body-hash sidecar next to the index. */
const INDEX_ROOT = path.join(os.homedir(), ".sovereign", "indexes");
function codeIntelEnriched(corpusId: string): boolean {
  return fs.existsSync(path.join(INDEX_ROOT, corpusId, "code_intel_cache.json"));
}

/** Directories that hold Rust source, walked once and memoized. Excludes
 *  build output — `target/` alone is larger than the source tree by
 *  orders of magnitude and contains generated `.rs` that would make the
 *  existence check pass for the wrong reason. */
const SOURCE_ROOTS = [
  "sovereign",
  "corpus-engine",
  "corpus-engine-scip",
  "corpus-engine-atos",
  "corpus-engine-notes",
  "corpus-engine-watchers",
  "corpus-engine-yield",
  "corpus-engine-archaeology",
  "commonwealth",
  "oicp-client",
  "oicp-types",
];
const SKIP_DIRS = new Set(["target", "node_modules", ".git", "dist", "vendor"]);

let BASENAMES: Set<string> | null = null;
function sourceBasenames(): Set<string> {
  if (BASENAMES) return BASENAMES;
  const out = new Set<string>();
  const walk = (dir: string, depth: number): void => {
    if (depth > 12) return;
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      if (e.isDirectory()) {
        if (SKIP_DIRS.has(e.name) || e.name.startsWith(".")) continue;
        walk(path.join(dir, e.name), depth + 1);
      } else if (e.isFile() && e.name.endsWith(".rs")) {
        out.add(e.name);
      }
    }
  };
  for (const root of SOURCE_ROOTS) walk(path.join(REPO_ROOT, root), 0);
  BASENAMES = out;
  return out;
}

// Phrased to LOCATE CODE, not to describe architecture — while still naming
// no symbol (see FORBIDDEN_IN_QUESTION).
//
// Intent routing is k-NN over exemplars and only reclassifies to `code_query`
// when a code exemplar is the nearest neighbour (CODE_INTEL_CHAT.md §5). The
// original phrasing ("how does the request actually get to another machine
// and come back?") reads as general architecture: on 2026-07-24 it routed to
// the knowledge path, which answered in confident prose citing a design doc
// and named zero source files. That is a worse failure than a wrong answer —
// it's the answer every other tool already gives you.
//
// "Where is … handed off … and what calls that path" mirrors the shape the
// spec validated ("Where is answer gating implemented, and what calls it?").
//
// 2026-07-25: that shape was still not enough. The trailing subordinate clause
// ("when the local model is too small") pulled the k-NN nearest neighbour back
// into the general-knowledge cluster: the turn routed to `KnowledgeQuery`
// (routing_log row 9627, coarse_intent LOOKUP), so `reweight_by_query_relevance`
// never fired, the 279 code-intel summaries lost to the raw chunks, and the
// grounding gate declined — "I couldn't confirm an answer to this against the
// 12 passages your sources turned up." A decline is the CORRECT behaviour for
// unsupported context; the defect was upstream, at the route.
//
// So the question now mirrors the spec's validated skeleton exactly —
// "Where is <plain-English noun phrase> implemented, and what calls it?" — and
// the route is ASSERTED below rather than inferred from the answer's shape.
// This is not tuning until green: the beat films a documented capability whose
// supported phrasing is code-structural, and it now fails at the routing step,
// with the reason, when that capability isn't reached.
const QUESTION =
  "In this codebase, where is the handoff of a chat request to another machine " +
  "implemented, and what calls it?";

// Symbols the answer is expected to reach. Naming any of them in the
// question would hand over the bridge the feature is supposed to build.
const FORBIDDEN_IN_QUESTION = [
  "model_slot",
  "ModelSlot",
  "placement",
  "rpc_worker",
  "oicp",
  "OICP",
  "distributed_inference",
  "mesh_get_placement",
  "InferenceRouter",
  "chat_completions",
];

beatTest(
  {
    id: "b8-code-intel",
    title: "Plain English in, the actual code path out",
    claim:
      "Ask the question you'd ask a senior engineer, naming no symbol, and get the " +
      "real code path back — without the code leaving the laptop.",
    gifPadSec: 1.0,
    gifMark: "code-answer",
  },
  async ({ page, run }) => {
    // ── The pop constraint, enforced before anything runs. ──
    const q = QUESTION.toLowerCase();
    for (const sym of FORBIDDEN_IN_QUESTION) {
      expect(
        q.includes(sym.toLowerCase()),
        `the demo question must not name "${sym}" — naming the symbol is the thing ` +
          "this feature exists to avoid, and a question that names it proves nothing",
      ).toBe(false);
    }

    const hosted = await hostedCorpora();
    const available = CODE_CORPORA.filter((c) => hosted.has(c));
    run.requireOrSkip(
      available.length > 0,
      `no code corpus hosted (looked for ${CODE_CORPORA.join(", ")}) — index the repo ` +
        "before capturing B8",
    );
    run.note(`code corpora available: ${available.join(", ")}`);

    run.requireOrSkip(
      hosted.has(SCOPE_CORPUS),
      `\`${SCOPE_CORPUS}\` is not hosted — B8 scopes to it because it is the only ` +
        "code corpus with a call graph. Index it before capturing B8.",
    );
    run.requireOrSkip(
      codeIntelEnriched(SCOPE_CORPUS),
      `\`${SCOPE_CORPUS}\` has no code-intel summaries ` +
        `(${path.join(INDEX_ROOT, SCOPE_CORPUS, "code_intel_cache.json")} missing). ` +
        "Without the conceptual→symbol bridge the model answers from its memory of " +
        "Rust and invents plausible paths. Run: " +
        `sovereign enrich code-intel ${SCOPE_CORPUS}`,
    );

    await realBootToChat(page);
    await run.dwell(1200);
    run.mark("open");

    // ── Scope by opening the notebook, not by muting 32 chips. ──
    // The notebook's Ask tab mounts ChatView with `hideScope`, so the
    // conversation is bound to this one corpus by construction. It also
    // films better: "open your codebase, ask it something" is the gesture
    // a real user makes. The shell renders views in an if/else chain, so
    // the main ChatView is unmounted here and the composer selectors
    // inside `run.turn()` stay unambiguous.
    await run.caption("My own codebase. Indexed locally.", 2800);
    await demoClick(page, page.getByTestId("nav-library"), { settleMs: 700 });
    const card = page
      .getByTestId("notebook-card")
      .filter({ hasText: SCOPE_CORPUS })
      .first();
    await expect(
      card,
      `the \`${SCOPE_CORPUS}\` notebook must be on the shelf to scope to it`,
    ).toBeVisible({ timeout: 15_000 });
    await card.scrollIntoViewIfNeeded();
    await run.dwell(900);
    await demoClick(page, card.getByTestId("notebook-ask"), { settleMs: 900 });

    // ── Start on an EMPTY thread. Load-bearing twice over. ──
    //
    // `openAsk()` reopens the notebook's most recent thread when one exists
    // (NotebookDetail.svelte), and demo mode keeps the scratch profile between
    // runs. So the default is: the take opens on the previous run's question,
    // and — the part that actually breaks the beat — the router's pre-check -2
    // `inherits_prior_knowledge_intent` keys off the PRIOR assistant turn's
    // intent and returns before the embed router runs (routing_log shows
    // `KNOWLEDGE_THREAD_INHERIT`, latency 0). On an inherited thread
    // `Intent::CodeQuery` is unreachable by construction, whatever the question
    // says. Measured 2026-07-25: inherited DeepQuery, code-intel bridge never
    // engaged.
    //
    // The conversation menu renders lazily (`notebookConvs.length > 0` resolves
    // after mount), so a short probe silently no-ops — which is how this hid.
    // Wait properly, then ASSERT the thread is empty: a stale thread must fail
    // the beat, not quietly re-pin the route.
    const convMenu = page.getByTestId("notebook-conv-menu");
    if (await convMenu.isVisible({ timeout: 15_000 }).catch(() => false)) {
      await demoClick(page, convMenu, { settleMs: 300 });
      await demoClick(page, page.getByTestId("notebook-ask-new"), { settleMs: 900 });
      run.note("minted a fresh conversation (notebook had prior threads)");
    }
    await expect(
      page.locator(".sv-ai-msg"),
      "B8 must open on an empty thread — a prior assistant turn makes the router " +
        "inherit that turn's intent (pre-check -2) and CodeQuery becomes unreachable",
    ).toHaveCount(0);
    run.mark("scoped");
    await run.dwell(1200);

    await run.caption("Asked in English. Scoped to the code.", 3000);
    const facts = await run.turn(QUESTION, { requireCitations: true, charDelayMs: 26 });
    run.mark("code-answer");

    // ── The route, asserted before anything downstream of it. ──
    // `Intent::CodeQuery` is what narrows retrieval to corpora with a
    // `scip_graph.db` AND lifts the code-intel summaries over the raw chunks
    // (`reweight_by_query_relevance`, CODE_INTEL_CHAT.md). Miss the route and
    // every later assertion fails for a reason that isn't the real one: the
    // answer is prose, or the gate declines outright. Checking it here means a
    // router regression reads as a router regression.
    const routed =
      facts.complete.metadata?.intent ?? facts.complete.metadata?.provenance?.intent ?? null;
    if (routed === null) {
      run.note("route not reported by this build — falling through to the answer-shape gates");
    } else {
      run.note(`routed as: ${routed}`);
      expect(
        routed,
        `the turn must route to code_query, got \`${routed}\`. Intent is k-NN over ` +
          "exemplars and only reclassifies when a CODE-STRUCTURAL exemplar is nearest " +
          '("what calls X", "where is Y implemented"). On the knowledge route the ' +
          "code-intel summaries are never reweighted, so retrieval returns raw chunks " +
          "and the answer is architecture prose — or an honest decline. Rephrase " +
          "QUESTION toward the structural shape, or check the exemplar set.",
      ).toBe("code_query");
    }

    // ── Grounded in code, not in the model's memory of Rust. ──
    const cited = [...new Set(facts.citations.map((c) => c.corpus_id))];
    run.note(`retrieval drew from: ${cited.join(", ")}`);
    expect(
      cited.includes(SCOPE_CORPUS),
      `the answer must cite the scoped corpus \`${SCOPE_CORPUS}\`; cited ` +
        `${cited.join(", ") || "nothing"}`,
    ).toBe(true);
    // Scoping is supposed to be binding, so anything from outside the code
    // corpora is a leak worth seeing in the ledger even when the beat passes.
    const strayCorpora = cited.filter((c) => !CODE_CORPORA.includes(c));
    if (strayCorpora.length > 0) {
      run.note(`WARNING — cited outside the scoped code corpora: ${strayCorpora.join(", ")}`);
    }

    // ── The gate that matters to this audience: the paths are real. ──
    const answer = facts.complete.full_text;
    const paths = [...new Set(answer.match(/\b[\w.\-/]+\.rs\b/g) ?? [])];
    run.note(`answer names ${paths.length} .rs path(s): ${paths.slice(0, 8).join(", ")}`);
    // On a zero, the ledger has to say whether the model wrote prose or the
    // harness read the wrong text — those have opposite fixes, and "0 paths"
    // alone cannot tell them apart.
    if (paths.length === 0) {
      run.note(
        `answer text (${answer.length} chars) as the gate saw it: ` +
          `${JSON.stringify(answer.slice(0, 400))}`,
      );
    }
    expect(
      paths.length,
      "a code-level answer must name at least one source file — prose about " +
        `architecture is what every other tool already gives you. Answer was ` +
        `${answer.length} chars: ${JSON.stringify(answer.slice(0, 300))}`,
    ).toBeGreaterThan(0);

    // Resolve loosely: the model may quote a repo-relative path, a
    // crate-relative one, or just a basename. Any of those is honest as
    // long as SOME .rs file with that name exists in the repo's SOURCE
    // tree. Bounded on purpose — a naive recursive walk from the repo
    // root drags through multi-gigabyte target/ dirs and would make this
    // assertion slower than the inference it's checking.
    const resolved = paths.filter(
      (p) => fs.existsSync(path.resolve(REPO_ROOT, p)) || sourceBasenames().has(path.basename(p)),
    );
    run.note(`resolved on disk: ${resolved.length}/${paths.length} — ${resolved.join(", ")}`);
    expect(
      resolved.length,
      `none of the cited paths exist in the repo (${paths.join(", ")}). A plausible ` +
        "path that isn't on disk is the exact failure this audience is scanning for.",
    ).toBeGreaterThan(0);

    await run.park();
    await run.dwell(3800);

    // ── Click through to the source line. ──
    const lastMsg = page.locator(".sv-ai-msg").last();
    const citations = lastMsg.locator(".source-citation");
    if ((await citations.count()) > 0) {
      await run.caption("Straight to the line.", 2600);
      await demoClick(page, citations.first(), { settleMs: 600 });
      // The reading surface is rendered by App.svelte inside the CHAT view
      // branch only — the notebook's Ask tab has no reading surface to open
      // (verified 2026-07-24: `<ReadingSurface />` has exactly one render
      // site). Scoping the beat is worth more than this shot, so treat the
      // click-through as unavailable-here rather than failing: everything
      // load-bearing (scoped corpus, real path on disk) is already asserted
      // above. If the Library ever gains a scoped-main-chat entry point,
      // film it there and restore the hard assertion.
      const surface = page.locator(".reading-surface");
      const opened = await surface
        .waitFor({ state: "visible", timeout: 20_000 })
        .then(() => true)
        .catch(() => false);
      if (opened) {
        await expect(surface.locator(".content")).toContainText(/\S/);
        run.mark("source-open");
        await run.park();
        await run.dwell(3400);
        run.note("reading surface opened on the cited source");
      } else {
        run.note(
          "click-through not filmed — the notebook's Ask tab hosts no reading " +
            "surface (it renders only in the main chat view). Scoping is what " +
            "makes this beat true, so the scoped surface wins.",
        );
      }
    } else {
      run.note(
        "model emitted no inline [Source:] marker — click-through not filmed. " +
          "Path-existence and citation resolution still asserted.",
      );
    }
  },
);
