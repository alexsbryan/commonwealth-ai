# KnowledgeView — your terrain, not your transcript

How Sovereign builds a durable picture of what you work on, think
about, and return to — and why that picture is a map, not a record.

---

## The problem

AI assistants start every conversation from zero. You re-explain, you
re-orient. Nothing you settled in last month's sessions reaches this
one.

The obvious fix — "just remember everything" — is worse than it
sounds. An assistant that remembers every detail feels less like a
collaborator and more like a file being kept on you. Memory without
structure is surveillance.

KnowledgeView is a different answer. Sovereign keeps a compact,
structured *map* of what you return to, tensions that keep surfacing,
and questions you haven't resolved. The model reads it before
answering, so each session starts situated rather than over.

The map is not a transcript. It's a landscape.

---

## Three maps, each a distinct kind of knowledge

- **Personal knowledge** — memories Sovereign extracts from your
  conversations, enriched with patterns: recurring concerns, live
  tensions between positions you've held, open questions you keep
  returning to without resolution.
- **Conversation history** — what you've been working on across the
  last 180 days. Active domains, unresolved threads, questions that
  crossed multiple sessions.
- **Institutional knowledge** — if you write decision and invariant
  notes as you build software, the map surfaces architectural
  consensus: established positions, decisions in tension, open
  questions without resolving decisions.

The vocabulary differs per view because the kind of knowledge differs.
A thoughtful friend phrases things differently than a senior engineer
does.

---

## The fourth thing: connections across the maps

Human lives don't respect view boundaries. Someone whose personal
memories return to "what does meaningful work look like" and whose
conversation history circles the purpose of their research project is
probably asking one question, not two.

Sovereign notices these patterns. When the same theme appears in two
maps, it's surfaced as a fourth landscape section:

```
Cross-view connections:

  (possible connected inquiries — not assertions)
    — theme "What does meaningful work look like for me?" (personal)
      may resonate with theme "What's the purpose of this project?"
      (conversations)
```

*"may resonate with"*, not *"is about"*. Sovereign never asserts that
two themes are the same concern. It flags them as questions for you to
accept, reject, or ignore. Most sessions won't see any — the
similarity threshold is deliberately strict. When they appear, they
survived that filter.

---

## What a session looks like

Before answering your first message, Sovereign assembles roughly this
from your maps:

```
Personal knowledge (last enriched 2h ago):

  Settled concerns:
    — relationship to complexity and control (18 memories)
    — what meaningful work looks like (11 memories)

  Live tensions:
    — stated preference for simplicity vs. recurring attraction to
      complex systems

  Open questions:
    — "what kind of life do I actually want" — 12 appearances,
      no stable answer

Conversational knowledge (last 180 days):

  Active domains:
    — oil market analysis (14 conversations)
    — fullstack application development (31 conversations)

Institutional knowledge:

  Established:
    — corpus-engine stays database-free
  Live tensions:
    — decision-88 pulls rusqlite in; invariant-12 says tools stay light

Cross-view connections:
    — "meaningful work" (personal) may resonate with "purpose of this
      project" (conversations)
```

Under 700 tokens total. Your actual question, retrieved corpus
passages, and conversation history still own the majority of the
prompt. The model doesn't announce the map. It reads the shape and
responds to your question.

---

## What stays private, structurally

Three invariants, enforced in code, not by policy:

- **Nothing leaves your machine.** Each view is marked `scope =
  "local"` and `mesh_sharing = false`. These fields can't be
  overridden by config. If you run Sovereign as part of a mesh, your
  knowledge corpora are never advertised, queried, or replicated.
- **Private skills are walled off.** Conversations tagged with a
  `privacy = "local_only"` skill (e.g. `inner-work`) never enter the
  shared conversational map.
- **Private skills suppress the wider context.** When such a skill is
  active, the conversational, institutional, and cross-view digests
  are omitted entirely. You see only your personal map.

---

## Honest trade-offs

*What you're trading.* An assistant that starts from zero each time
has real benefits: clean slate, no accumulating misreadings, no
built-up take on you. KnowledgeView trades some of that for
situatedness.

*What could go wrong.* The model could read too much into a pattern —
tentative framing is there because these are guesses. Memory
extraction occasionally misfires. Cross-view connections can feel
intrusive — "your therapy work connects to your governance design"
may be accurate *and* unwelcome, which is why the system phrases it
as a question.

*When not to use it.* If you prefer an assistant that never builds a
picture of you, turn KnowledgeView off. The rest of the system works
without it.

---

## What you'll notice

The first message of a new session lands closer to where you actually
are. Topics you've settled don't need re-settling. Live tensions get
called out tentatively, not insisted on. Private sessions are
noticeably quieter — smaller context window, no cross-session
terrain.

The model never mentions the map unless you ask. It doesn't cite
specific memories back at you. It reads the shape and responds to
your actual question.

---

## Inspect / turn off / reset

- **Inspect** — each view's enriched state lives as
  `field_skeleton.json` under `~/.sovereign/indexes/<view>/`. Plain
  JSON, openable in any editor.
- **Reset one view** — delete its index directory; Sovereign
  re-ingests on the next session.
- **Turn off entirely** — three places, depending on how you run
  Sovereign:
  - *Desktop app*: Settings → Knowledge → "Enable KnowledgeView"
    (requires a restart).
  - *CLI*: pass `--no-knowledge-view`.
  - *Server*: set `[knowledge_view] enabled = false` in
    `sovereign-server.toml`.

  When off, Sovereign starts every session from zero, as it did
  before this feature existed.
- **Scope to a skill** — add `privacy = "local_only"` to the skill's
  `skill.toml`.

---

## In one sentence

An attempt to let an assistant know your terrain without pretending to
know you — by keeping structured maps that are tight, local, honest
about their uncertainty, and phrased as questions the model holds for
you rather than conclusions it has reached about you.

Whether that trade feels right is up to you.
