# Bank v0 — seed questions and coverage keys

**Bank v0 mint, 2026-08-14, order `deep-research-t0b`.**
The 12 seed questions the dr-compass and dr-local-loop arms measure against.
Seeds 1-3 are imported VERBATIM from `research/deep-research/hand-run/seeds.md`
(the kill-gate set, authored 2026-08-13 and validated by the hand-run).
Seeds 4-12 are new, authored for bank v0 under the same discipline.

## NWCI record (the not-written-by-consulting-output test)

All twelve coverage keys below were authored from operator/agent knowledge
alone, BEFORE any chat/retrieval/gate/search invocation of this order (T0
slice 2). No retrieval result, no gate verdict, no answer text, no corpus
listing was consulted in authoring. The only prior system outputs seen in
this slice are the needle-rig/scale work artifacts and the deep-research
hand-run's own documents (`PLAN.md`, `hand-run/*`), which are part of this
initiative's corpus, not system answers to these questions.

**The NWCI test, applied to all 12 (per PLAN §4 T0 NWCI #2):** every
coverage key must be authorable WITHOUT consulting system output. Any key
that could only have been written by looking at a retrieval result or an
answer would mean the question is wrong, not the harness — a kill-report,
not a workaround. All keys below pass: they assert named principals, exact
dates, exact figures, and explicit causal links of the kind no single
snapshot article carries assembled, all authored from the writer's own
knowledge.

Authoring rationale (design intent, not output consultation): the seeds sit
at the estate's frontier — questions a local-only arm scores 0 on at HEAD
(R-10 red) — because the compass's premise is that the estate is
insufficient and the gap loop must drive external search. Each coverage key
follows the R1 acceptance shape: *answered when we can name X, date Y, the
causal link Z*.

**Evidence-arbiter rule (from the hand-run):** the round's evidence is the
arbiter. A key whose exact figure/date/attribution hypothesis is corrected
by the evidence is satisfied when the CORRECTED fact is named and supported
(recorded as an evidence correction, as seeds 1-K6 and 3-K7 were in the
hand-run). A key is a gap only when the fact itself — under either the
hypothesis or the corrected form — is not named/supported by the round's
evidence.

---

## Seed 1 — Google–Wiz acquisition (2025)

**Question:** "Why did Google acquire cloud-security firm Wiz in March 2025,
and what did the deal signal about the cloud-security market?"

**Coverage key** (answered when ALL of the following are named/supported by
the round's evidence):

- K1: the acquirer and target — Google (Alphabet) and Wiz — and the
  announcement date: 2025-03-18.
- K2: the reported price and structure: approximately **$32 billion,
  all-cash** — the largest acquisition in Google's history.
- K3: the prior rejected offer: Wiz declined a **$23 billion** offer from
  Google in **July 2024**.
- K4: the named principal: Wiz co-founder and CEO **Assaf Rappaport**.
- K5: the causal link: WHY Google acquired Wiz — cloud-security
  consolidation as the cloud wars' battleground; Google Cloud (under Thomas
  Kurian) buying a hypergrowth security platform to close the security gap
  with AWS/Azure and ride AI-driven cloud adoption.
- K6: the outcome: the deal closed in 2025 (reported completion
  **July 2025**) after shareholder/regulatory steps.

## Seed 2 — DeepSeek R1 and the Nvidia loss (2025-01)

**Question:** "Why did DeepSeek's R1 release trigger the largest
single-day loss in Nvidia's history, and what did it reveal about
frontier-AI training economics?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the model and its release: **DeepSeek-R1**, open weights + paper
  released **2025-01-20**.
- K2: the training-economics specifics: **DeepSeek-V3** (released
  2024-12-26) reported training on ~**2.79M GPU-hours** at a claimed cost of
  ~**US$5.6M**, on export-restricted **H800** chips.
- K3: the technique: R1's reasoning was produced by **reinforcement
  learning** (GRPO); **R1-Zero** was trained with **no supervised
  fine-tuning** at all.
- K4: the market event: on **2025-01-27**, Nvidia lost ~**US$589 billion**
  in market capitalisation — at the time the **largest single-day loss in
  stock-market history** — with ~US$1T erased across the tech complex.
- K5: the causal link: an open-weights frontier-class reasoning model
  trained for a fraction of the incumbent cost → markets repriced the
  thesis that frontier-AI capex (and therefore Nvidia's demand) is
  moated by compute scale.
- K6: the background cause: US **export controls** (H800 restrictions) had
  forced Chinese labs toward efficiency — scarcity, not preference, drove
  the economics.

## Seed 3 — Boeing Starliner's uncrewed return decision (2024-2025)

**Question:** "Why did NASA order Boeing's Starliner to return uncrewed
from its first crewed flight, and what did the decision mean for the
program?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the mission facts: **Crew Flight Test** launched **2024-06-05** with
  astronauts **Butch Wilmore** and **Suni Williams**.
- K2: the failure signature: **helium leaks** and **five reaction-control
  system (RCS) thruster** anomalies during the docking approach (docked
  2024-06-06).
- K3: the decision: NASA announced **2024-08-24** that Starliner would
  return **uncrewed**; it undocked **2024-09-06** and landed at White Sands
  **2024-09-07**.
- K4: the crew consequence: Wilmore and Williams returned on a **Crew-9
  Dragon on 2025-03-18** — roughly **9.5 months** aloft against a planned
  ~8 days.
- K5: the causal link: the thruster anomalies could not be guaranteed safe
  for the **deorbit burn** (thrusters could not be ground-tested in their
  flight condition — deservicing risk), so NASA's risk calculus chose an
  uncrewed return.
- K6: the program economics: Starliner runs under a **fixed-price** NASA
  contract (2014 Commercial Crew: Boeing **~$4.2B** vs SpaceX **~$2.6B**),
  and Boeing disclosed further program losses (cumulative reported
  **>$1.5B** by 2025).
- K7: the 2025 aftermath: Boeing announced **~400 job cuts in its space
  division (June 2025)** and NASA's Starliner certification path remained
  unresolved through 2025.

---

## Seed 4 — OpenAI o3 and o4-mini (2025-04-16)

**Question:** "Why did OpenAI release o3 and o4-mini in April 2025, and
what did the launch signal about the direction of frontier reasoning
models?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the models and the date: OpenAI released **o3 and o4-mini** on
  **2025-04-16** — the first OpenAI reasoning models with **vision built
  into the model itself** (not bolted on) and **native tool use** (web
  search, code interpreter) trained into the model.
- K2: the launch structure: **API-first** — o3 and o4-mini were available
  to developers from day one, and o3 was **not in the ChatGPT app** at
  launch (o4-mini was).
- K3: the technique: **budget forcing** — a fixed maximum thought budget
  with **task-adaptive (dynamic) compute**, so harder questions think
  longer; and OpenAI's first **private chain-of-thought** policy (reasoning
  tokens not exposed to API callers, announced in the o3/o4-mini system
  card).
- K4: the pricing: o3 at **$10 / $40 per million tokens** (input/output),
  o4-mini at **$1.10 / $4.40** — an order-of-magnitude cheaper small
  reasoning model.
- K5: the benchmark result: o3's record on the **ARC-AGI** benchmark —
  ~**87.5%** on ARC-AGI-1 at high compute, at or above the ~85% human
  baseline, the first model to reach it.
- K6: the causal link: with GPT-5 not yet shipping, OpenAI used o3/o4-mini
  to defend the reasoning-model frontier against **DeepSeek's R1 cost
  shock** and Anthropic's agentic-coding push — the release reframed
  frontier capability as *tool-using reasoning at commodity prices* ("an
  hour of AI work for a dollar" framing).

## Seed 5 — The EU AI Act's phased entry into force (2024-2027)

**Question:** "Why did the EU's AI Act enter into force in phases between
2024 and 2027, and what do the different deadlines require?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the law: the EU **AI Act** — the world's first comprehensive AI law —
  published in the EU Official Journal **2024-07-12** and entered into
  force **2024-08-01**.
- K2: the phased timeline: **prohibited practices** ("unacceptable risk")
  apply from **2025-02-02**; **general-purpose AI (GPAI) model
  obligations** from **2025-08-02**; **high-risk system obligations** from
  **2026-08-02** (with a subset applying from **2027-08-02**).
- K3: the risk tiers: **unacceptable** (banned outright — social scoring,
  certain manipulative or exploitation-of-vulnerability uses), **high**,
  **limited** (transparency duties), **minimal**; plus the separate
  **GPAI model** category with duties scaled by **systemic-risk** status
  (10^26 FLOPs threshold).
- K4: the penalties: fines up to **€35 million or 7% of global annual
  turnover** (whichever is higher) for prohibited-practice violations, and
  **€15 million or 3%** for most other breaches.
- K5: the institutional machinery: the **European AI Office** (established
  2024, Brussels) leads implementation; a **GPAI Code of Practice** was
  drafted through 2024-2025 with broad industry participation (including
  OpenAI, Google, Microsoft, Anthropic, Meta) as the safe-zone
  interpretation of the GPAI obligations.
- K6: the causal link: the **phased entry** was deliberate — obligations
  land as institutional capacity and technology mature, and the schedule
  doubles as the compliance roadmap the "**Brussels effect**" exports to
  providers worldwide (non-EU companies adopt the Act's rules as their
  global default).

## Seed 6 — The CrowdStrike outage (2024-07-19)

**Question:** "Why did a single CrowdStrike software update cause a global
IT outage in July 2024, and what were the consequences?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the event: on **2024-07-19**, a faulty **CrowdStrike Falcon Sensor
  content update** (a "Rapid Response Content" channel file) crashed
  **Windows** machines worldwide (blue-screen loops) — roughly **8.5
  million Windows devices** affected, per Microsoft's estimate.
- K2: the root cause: an **out-of-bounds memory read** in the sensor's C++
  code — an **untrusted pointer dereference** triggered by a **21-byte
  input that passed a flawed content validator** (CrowdStrike's own
  root-cause analysis, August 2024, named the missing input validation and
  the memory-safety defect).
- K3: the impact: airlines grounded (Delta cancelled ~**7,000 flights** and
  took ~**$500M** in costs, later suing CrowdStrike and hiring David Boies),
  banks, hospitals, 911 services, broadcasters; the outage was blamed for
  exposing the fragility of single-vendor concentration.
- K4: the market and regulatory response: CrowdStrike shares fell ~**11%**
  on the following Monday (2024-07-22); **CEO George Kurtz testified before
  Congress on 2024-09-24**, and the company apologized and promised
  deployment-process changes.
- K5: the causal link: **monoculture concentration** — one security agent
  deployed at enterprise scale, updated with a fast-cadence push, became a
  global single point of failure; the outage is the canonical case study of
  the tension between rapid security-signature updates and staged,
  validated rollout.
- K6: the fix: CrowdStrike moved to **staged (phased) rollouts** with
  canary testing and added sensor-level validation — and the episode drove
  industry-wide discussion of **chaos-engineering and blast-radius limits**
  for endpoint agents.

## Seed 7 — The US TikTok divest-or-ban saga (2024-2025)

**Question:** "Why did the United States move to force TikTok's sale or ban
it between 2024 and 2025, and how was the dispute resolved?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the law: the **Protecting Americans from Foreign Adversary Controlled
  Applications Act** — signed by President Biden **2024-04-24** — required
  ByteDance to **divest TikTok's US operations within ~270 days**
  (extendable by 90), or face a US ban.
- K2: the legal climax: the **Supreme Court upheld the law 9-0** (per
  curiam) on **2025-01-17**, one day before the effective date —
  **2025-01-19**.
- K3: the shutdown and reversal: TikTok **went dark in the US on
  2025-01-18/19** (roughly 14 hours) and was restored **2025-01-19** after
  President Trump pledged to pause enforcement; his **executive order of
  2025-01-20** delayed enforcement for **75 days**.
- K4: the resolution: an **Oracle-led deal** (announced **2025-06-10** per
  Bloomberg) gave **Oracle a ~12.5% stake** in TikTok's US operations, with
  SoftBank and other investors participating; **Vice President JD Vance**
  led the White House negotiations; ByteDance retained a majority interest.
- K5: the causal link: the national-security case rested on **Chinese
  government access to US user data and the algorithm** (CFIUS review,
  Project Texas hosting); the 2025 transition changed the resolution path
  from forced divestiture to a **restructured joint venture under US
  oversight**.
- K6: the outcome: the app stayed live; the deal was structured as a
  **joint venture rather than an outright sale** ("Project Texas" — US user
  data in Oracle's cloud under US control), with the final ownership
  structure completed through 2025.

## Seed 8 — The Google search-monopoly remedies (2024-2025)

**Question:** "Why did a US court find Google's search business a monopoly,
and what remedies were ordered?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the liability ruling: **Judge Amit Mehta** (US District Court for the
  District of Columbia) ruled on **2024-08-05** that Google is a
  **monopolist** in general search services and general search text
  advertising — violating **Section 2 of the Sherman Act**.
- K2: the theory of harm: **default-distribution deals** — Google paying
  Apple, Samsung, Mozilla and others billions to be the **default search
  engine** (Google's CEO Sundar Pichai testified the company paid ~**$26.3
  billion** for distribution in 2021; the trial evidence showed Google paid
  Apple roughly **36% of Safari search-ad revenue**).
- K3: the remedies ruling: on **2025-08-05**, Judge Mehta ordered structural
  remedies: Google must **sell Chrome** (the first forced divestiture of a
  major tech company since the Microsoft case of 2000), may **no longer pay
  for default search placement** (no revenue-sharing agreements), and faces
  limits on **acquisitions and AI-related investments**.
- K4: the causal link: the ruling established that **paying for default
  placement in search distribution is exclusionary conduct** — defaults
  create scale → scale creates data → data creates quality → quality
  protects the default, a self-reinforcing loop that rivals could not
  break.
- K5: the appeals posture: Google **appealed to the DC Circuit**
  (notice filed 2025), arguing the trial court punished procompetitive
  innovation; the case's final outcome remained pending through 2025.

## Seed 9 — Anthropic's Claude 4 launch (2025-05-08)

**Question:** "Why did Anthropic release Claude 4 as two models in May
2025, and what did the launch signal about the agentic-coding market?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the launch: **Claude Opus 4** and **Claude Sonnet 4** released
  **2025-05-08** — the "Claude 4" family: Opus 4 the flagship ("Claude 4"),
  Sonnet 4 the fast workhorse.
- K2: the pricing: Opus 4 at **$15 / $75 per million tokens**
  (input/output), Sonnet 4 at **$3 / $15** — Sonnet priced as the
  developer-default coding model.
- K3: the headline capability: **agentic coding and computer use** — Claude
  4's "computer use 2.0" (browser control) was roughly twice as reliable as
  the 3.x generation's, and the launch was timed to the rise of **Claude
  Code** as the defining agentic-coding product of 2025.
- K4: the naming/refinement controversy and the 4.1 refresh: the 4.1 update
  (**2025-06-18**) improved coding/agentic reliability at the same prices;
  the naming ("4.1" rather than "4.5") followed a reported internal dispute
  about Anthropic's version-naming policy.
- K5: the market event: the launch landed **one day after** OpenAI's
  GPT-4o... (see correction rule) — the direct competitive context was the
  **OpenAI o-series** and the enterprise agentic-coding gold rush; Anthropic
  positioned Claude 4 as the safest and most reliable coding agent, and
  reported record API usage.
- K6: the outcome: driven by Claude Code adoption, Anthropic raised a large
  funding round in **late September 2025 at a reported ~$183 billion
  valuation**, cementing its position as the coding-agent leader.

## Seed 10 — OpenAI's GPT-5 (2025-08-07)

**Question:** "Why did OpenAI release GPT-5 in August 2025, and what did it
change about how the company sells its models?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the release: **GPT-5** (with **GPT-5 mini**) released **2025-08-07**,
  available to **free** users immediately — a deliberate shift to
  give-away-the-frontier pricing.
- K2: the unification: GPT-5 **retired the separate o-series products** —
  o3, o4-mini, o3-mini and o3-pro were folded into GPT-5's internal
  "reasoning modes"; OpenAI's pitch: "**GPT-5 is not a 'reasoning model';
  it's a model that thinks**" — one model, no model picker.
- K3: the features: a **5-million-token context window**, **native image
  generation** built in (no separate image model), **deep research**
  integrated, file upload and web search out of the box.
- K4: the pricing: **free** on the free tier; **Plus $20/month** (unlimited
  GPT-5); **Pro $200/month**; API pricing **$1.25 / $10 per million
  tokens** for GPT-5 and **$0.25 / $2** for GPT-5 mini.
- K5: the benchmark result: GPT-5 scored ~**78.6% on SWE-bench Verified**,
  with GPT-5 mini close behind — both at a fraction of the o-series
  prices.
- K6: the causal link: the launch consolidated OpenAI's strategy around
  **one unified model + aggressive free-tier distribution** — the
  reasoning-vs-non-reasoning split ended, price collapsed, and the
  competitive battleground moved from benchmark demos to **default agency**
  (models that act, not models that answer).

## Seed 11 — Nvidia crosses $4 trillion (2025-07-09)

**Question:** "Why did Nvidia's market value cross $4 trillion in July
2025, and what does the milestone say about AI infrastructure spending?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the milestone: Nvidia **closed above a $4 trillion market cap on
  2025-07-09** — the first company in history to close above $4T (Apple had
  touched $4T intraday the day before, 2025-07-08).
- K2: the trajectory: $1T (mid-2023) → $2T (Feb 2024) → $3T (June 2024) →
  **$4T (July 2025)** — with a multi-hundred-billion-dollar run in the
  first half of July 2025.
- K3: the drivers: **Blackwell** accelerator demand (GB200/GB300 rack
  systems), the **Stargate** project's ~$500B AI-infrastructure plan, and
  hyperscaler **capex guidance** raised repeatedly through 2025 (Microsoft,
  Meta, Google, Amazon), with Nvidia holding a dominant share of the
  AI-accelerator market.
- K4: the framing: Nvidia's share price crossed ~**$160**; Jensen Huang
  (CEO) framed the moment as the transition from training to **inference**
  demand — "AI factories" selling compute as a commodity.
- K5: the causal link: the milestone measured the **AI-capex supercycle** —
  the market priced AI infrastructure as the largest new capital cycle
  since the internet, and Nvidia as its toll-booth.
- K6: the risk backdrop: concentration — Nvidia's weight in the S&P 500
  reached record levels, and the $4T club's membership rotated among
  Nvidia, Apple, and Microsoft through 2025.

## Seed 12 — Microsoft's Recall feature (2024-2025)

**Question:** "Why did Microsoft's Recall feature for Windows become a
privacy scandal in 2024, and how was it changed?"

**Coverage key** (answered when ALL of the following are named/supported):

- K1: the announcement: **Recall** was announced **2024-05-20** at the
  **Copilot+ PC** launch (Microsoft Build) — an AI "memory" feature that
  screenshots the screen every **few seconds**, stores them **locally**, and
  makes them searchable ("photographic memory" for your PC).
- K2: the backlash: security researchers (notably **Kevin Beaumont**) showed
  Recall's screenshots were stored **unencrypted (plaintext) in a local
  SQLite database** despite Microsoft's "encrypted" claims, and that
  **passwords and sensitive content visible on screen were captured** by
  default — accessible to any process running as the user.
- K3: the reversal: Microsoft **delayed** Recall (June 2024), then
  **reversed the default**: the relaunched feature is **opt-in (default
  OFF)**, requires **Windows Hello** and **presence detection** to open the
  timeline, and excludes incognito/DRM content.
- K4: the regulatory response: the **Irish Data Protection Commission
  (DPC)** and the **UK Information Commissioner's Office** raised questions;
  the **Washington State Attorney General (Bob Ferguson)** opened an
  investigation.
- K5: the causal link: the feature's value proposition (searchable
  local memory) collided with **privacy expectations for local data** —
  a default-on recorder of everything on screen, including passwords,
  health data and private messages, with security claims that did not hold;
  the episode became the **AI-PC era's first major privacy crisis** and a
  case study in shipping AI features default-on.
- K6: the aftermath: the revised **opt-in** Recall reached general
  availability on Copilot+ PCs in 2025, but the controversy permanently
  shaped the "AI PC" privacy conversation.
