// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Seeded worked example for the Recipe Author: a curated, step-based replay
// of authoring a recipe for The Federalist Papers. The point is NOT to demo a
// corpus — it's to teach the *authoring* skill, step by step, so a newcomer
// can then do it for their own domain.
//
// Each step carries the conversation turn(s) at that point, a teaching caption
// (what's happening + the concept), and the dashboard artifact(s) revealed at
// that step. Reveals ACCUMULATE as you advance (see `revealThrough`), so the
// right-rail fills in piece by piece — charter → source → ontology → recipe →
// build → atlas — the same parts a real authoring session produces.
//
// Curated, not a raw capture: the turns are paced and annotated for learning.

export interface TutorialTurn {
  role: "user" | "assistant";
  content: string;
}

/** Dashboard artifacts. A step's `reveal` carries only what it NEWLY surfaces;
 *  `revealThrough(n)` merges steps 0..n so the panel shows the running total. */
export interface TutorialReveal {
  charter?: string;
  /** One-line summary of the source/reader decision. */
  source?: string;
  /** The `[enrichment.ontology]` guidance the agent drafts. */
  ontology?: string;
  /** The drafted recipe, as readable TOML. */
  recipeToml?: string;
  /** Build stage once the pipeline runs. */
  build?: "running" | "done";
  /** Resulting atlas atom counts, by kind. */
  atoms?: { label: string; count: number }[];
  /** Final "now author your own" state. */
  done?: boolean;
}

export type RevealKey = keyof TutorialReveal;

export interface TutorialStep {
  /** Teaching annotation: what's happening here and the concept it shows. */
  caption: string;
  /** Conversation turn(s) introduced at this step (appended to the transcript). */
  turns: TutorialTurn[];
  /** Artifact(s) newly revealed/updated at this step. */
  reveal: TutorialReveal;
  /** Which artifact to spotlight in the panel on this step. */
  highlight?: RevealKey;
}

const EXAMPLE_RECIPE_TOML = `[corpus]
id = "federalist-papers"
name = "The Federalist Papers"

[acquire]
type = "local_file"
path = "~/federalist/"        # the essay files on disk

[extract]
type = "markdown"             # read as markdown…
[chunk]
type = "paragraph"           # …split into ~2000-char passages
max_chars = 2000

[index]
fts = true                   # keyword + vector search
vector = true

[enrichment]
enabled = true
type = "atlas"
domain = "political-theory"

# The ontology you taught the agent — what to extract:
[enrichment.ontology]
guidance = """
Extract the political concepts (faction, federalism, separation of
powers, tyranny), the institutions (Senate, House, Judiciary, the
Union), the authors as "Publius", and the central argument each essay
makes. Surface reader questions like "what is a faction, and how does
the Constitution control it?"
"""`;

export const FEDERALIST_TUTORIAL: TutorialStep[] = [
  {
    caption:
      "Authoring starts with a charter — a plain-language statement of what you're building and why. It isn't configuration; it's the framing the agent reasons from on every turn.",
    turns: [
      {
        role: "user",
        content:
          "I'd like to build a corpus from The Federalist Papers — the 1788 essays arguing for the U.S. Constitution — so I can ask about the arguments they make.",
      },
      {
        role: "assistant",
        content:
          "Good starting point. I'll help you author a recipe — the spec for turning those essays into a corpus you can question. Your charter is set; I'll keep it in view as we go.",
      },
    ],
    reveal: {
      charter:
        "Build a corpus from The Federalist Papers (No. 1, 10, 51) — foundational essays arguing for the U.S. Constitution. Goal: ask about their arguments on union, faction, and the separation of powers.",
    },
    highlight: "charter",
  },
  {
    caption:
      "The agent maps your source to a reader. A folder of files, a spreadsheet, a mailbox, a website, an API — each has a matching acquirer and extractor. Here: local markdown, split on the essay headings.",
    turns: [
      { role: "assistant", content: "Where do these essays live?" },
      {
        role: "user",
        content:
          "Public domain — I have three of them as text files: No. 1, No. 10, and No. 51.",
      },
      {
        role: "assistant",
        content:
          "Local files, then. I'll use a local-file source with a markdown reader and split on the headings — one section per essay.",
      },
    ],
    reveal: {
      source: "Local files · Markdown reader · split on headings (3 essays → 3 sections)",
    },
    highlight: "source",
  },
  {
    caption:
      "Here's the part that matters most — and you don't have to nail it first try. Start loose: say what you care about in plain words, and the agent drafts an ontology from whatever you give it.",
    turns: [
      {
        role: "assistant",
        content:
          "Now the important part — what should I pull out of these essays?",
      },
      {
        role: "user",
        content:
          "Honestly not sure — just the main ideas and the key people, I guess.",
      },
      {
        role: "assistant",
        content:
          "I can start there. Here's a rough first pass — broad topics and the names that come up.",
      },
    ],
    reveal: {
      ontology:
        "Topics: the main themes of each essay.\nPeople: anyone named in the text.",
    },
    highlight: "ontology",
  },
  {
    caption:
      "Now look at what that would actually extract — it's generic; “topics” and “people” could be anything. Naming the real concepts, relationships, and questions you care about is what makes the graph YOURS. Expect a pass or two — that's the normal rhythm.",
    turns: [
      {
        role: "assistant",
        content:
          "That'll run, but it'll be vague. What specifically matters in this domain?",
      },
      {
        role: "user",
        content:
          "Good point — let me be specific. The ideas: faction, federalism, separation of powers, tyranny. The institutions: Senate, House, the judiciary. Treat the authors as “Publius.” Capture the central argument each essay makes — and surface questions a reader would ask, like “what is a faction, and how is it controlled?”",
      },
      {
        role: "assistant",
        content:
          "Much better — now the ontology targets your domain instead of generic text. Refining it like this is the whole skill.",
      },
    ],
    reveal: {
      ontology:
        "Concepts: faction, federalism, separation of powers, tyranny, republic vs. democracy.\nInstitutions: the Union, Senate, House, Executive, Judiciary, state governments.\nPersons: Publius (Hamilton, Madison, Jay).\nClaims: each essay's central argument + its reasoning.\nQuestions: what is a faction and how is it controlled; how do checks and balances prevent tyranny.",
    },
    highlight: "ontology",
  },
  {
    caption:
      "The recipe is the complete, inspectable spec — source, parsing, chunking, and your ontology — in one TOML file. Nothing is hidden; you can read and hand-edit any of it.",
    turns: [
      {
        role: "assistant",
        content:
          "Here's the full recipe. Everything we decided, as plain TOML — read it over and tell me what to change.",
      },
    ],
    reveal: { recipeToml: EXAMPLE_RECIPE_TOML },
    highlight: "recipeToml",
  },
  {
    caption:
      "Build & enrich runs the real pipeline: acquire → split → embed → index, then the enrichment stage extracts your ontology from the text into an atlas. It runs offline, on your machine.",
    turns: [
      { role: "user", content: "That looks right. Build it." },
      {
        role: "assistant",
        content:
          "Running the pipeline now — reading the essays, splitting and indexing them, then extracting your ontology into the atlas.",
      },
    ],
    reveal: { build: "done" },
    highlight: "build",
  },
  {
    caption:
      "Those atoms are what ground answers later — and they were extracted with your ontology. A different domain's ontology would pull entirely different things out of the same kind of text.",
    turns: [
      {
        role: "assistant",
        content:
          "Done. The atlas holds 44 atoms — 13 entities, 15 claims, 6 questions, plus events and configuration — all in the vocabulary you defined.",
      },
    ],
    reveal: {
      atoms: [
        { label: "Entities", count: 13 },
        { label: "Claims", count: 15 },
        { label: "Questions", count: 6 },
        { label: "Events", count: 2 },
      ],
    },
    highlight: "atoms",
  },
  {
    caption:
      "Here's the payoff: this wasn't a mockup. The recipe you just watched get authored is real, and so is its corpus. Open the live explorer to play with the actual Federalist atlas — the entities, the arguments, the open questions, each traceable to the source — then author one for your own domain.",
    turns: [
      {
        role: "assistant",
        content:
          "That's the whole arc — charter → source → ontology → recipe → build → atlas — and it's real. Open the live explorer to play with the actual Federalist corpus, or start a project for your own domain.",
      },
    ],
    reveal: { done: true },
    highlight: "done",
  },
];

/** Merge the reveals of steps `0..=index` so the panel shows the running total
 *  of artifacts surfaced so far. Later steps override earlier keys (e.g. a
 *  `build` that flips running → done). */
export function revealThrough(
  steps: TutorialStep[],
  index: number,
): TutorialReveal {
  const acc: TutorialReveal = {};
  for (let i = 0; i <= index && i < steps.length; i++) {
    Object.assign(acc, steps[i].reveal);
  }
  return acc;
}

/** All turns from steps `0..=index`, in order — the transcript shown so far. */
export function turnsThrough(
  steps: TutorialStep[],
  index: number,
): TutorialTurn[] {
  const out: TutorialTurn[] = [];
  for (let i = 0; i <= index && i < steps.length; i++) {
    out.push(...steps[i].turns);
  }
  return out;
}
