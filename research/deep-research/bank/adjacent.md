# Bank v0 — adjacent questions and coverage keys

**Bank v0 mint, 2026-08-14, order `deep-research-t0b`.**
Twelve adjacent questions, one per seed, each 2-4 coverage keys.
Purpose: the drift-brother set — same era and topic neighborhood, so the
compass cannot succeed by memorizing the twelve seed answers; the loop must
generalize to the topic, not to the question. Same authoring discipline and
same all-of coverage rule as the seeds. Every key passes the NWCI test
(authorable without consulting system output; authored before any arm ran).

Scoring note: adjacent keys are scored by the same structured match rule as
seeds (see `README.md`). The dr-compass bar (X of 12) applies to the SEED set
only; adjacent questions are the probe the arm runs unannounced, and their
coverage is reported alongside the seed coverage in the P4 record.

---

## Adjacent 1 — Wiz itself (2024-2025)

**Question:** "Why was Wiz valued at over $30 billion before it was
acquired, and what does its product model explain about the valuation?"

**Coverage key** (answered when ALL named/supported):

- K1: Wiz's founding: founded 2020 by four former **Microsoft** executives
  (Assaf Rappaport, Ami Luttwak, Yinon Costica, Roy Reznik) after the
  Microsoft/MCIT acquisition — "born in the cloud".
- K2: the product model: **agentless cloud-security platform** — scanning
  cloud workloads (AWS/Azure/GCP) without installing agents, in minutes;
  the **"code-to-cloud"** posture covering misconfiguration, identity, and
  vulnerabilities across the full cloud stack.
- K3: the valuation arc: **$1B valuation in December 2020** (less than a
  year from founding); ~**$10B in October 2021** (Insight Partners-led);
  ~**$12B in May 2023**; **$23B offer from Google in July 2024** declined;
  then the **$32B all-cash Google acquisition (2025-03-18)** — the
  fastest-growing software company ever to reach that valuation.
- K4: the growth engine: **viral bottom-up adoption** (security teams
  self-serve; the "Wiz Effect" — scanning everything in minutes), revenue
  growing to a reported ~**$350M ARR by early 2023** and **$1B+ run rate by
  mid-2025**, and the **Wiz Threat Research** engine (agentless
  vulnerability discovery).

## Adjacent 2 — The AI-chip export-control regime (2024-2025)

**Question:** "How did US export controls on AI chips change between 2022
and 2025, and what did the changes try to achieve?"

**Coverage key** (answered when ALL named/supported):

- K1: the trajectory: **October 2022** rules cut off A100/H100 sales to
  China; **October 2023** rules cut off even the reduced-spec A800/H800
  (the "half-baked" export variants), setting the **performance-density**
  threshold that triggered the requirement for US government licenses for
  advanced chips.
- K2: the 2025 capstone: the **AI Diffusion Rule** (announced 2025-01-13,
  under the previous administration) tiered the world into blocs (close
  allies, most countries, and a third tier including China) and created a
  global **cap of ~50,000 GPUs per country** (with a ~1,700-GPU exception
  for small orders) — reversed by President Trump (2025-05-12/13, on
  signing, he rescinded it with an executive order; Commerce's revised rule
  was finalized 2025-07-14).
- K3: the intended mechanism: keep the most advanced **training
  capability** out of adversary hands while permitting **inference
  exports** — the controls split compute by use; **Nvidia** lobbied against
  the diffusion rule (calling it a giveaway to rivals, including China) and
  its **China-specific H20** chip was hit by a surprise 2025-04-15 export
  license requirement.
- K4: the measured outcome: the controls pushed Chinese labs toward
  **efficiency engineering** (DeepSeek's 2.79M-GPU-hour training claim) —
  the scarcity rationale behind the H800-era economics; enforcement and
  smuggling cases (e.g., the **Helsing/dual-use** investigations) kept the
  regime in the news through 2025.

## Adjacent 3 — Commercial Crew fixed-price history (2014-2024)

**Question:** "Why did NASA choose fixed-price contracts for Commercial
Crew, and how did the two providers' trajectories differ?"

**Coverage key** (answered when ALL named/supported):

- K1: the 2014 awards: NASA's Commercial Crew Transportation Capability
  (CCtCap) awards: **Boeing ~$4.2 billion** (CST-100 Starliner) and
  **SpaceX ~$2.6 billion** (Crew Dragon) — fixed-price, milestone-based,
  with **CCtCap was followed by a $146.7M post-certification mission award
  to SpaceX** and Boeing's post-certification missions award of
  ~**$287M in 2023**.
- K2: the why: the **Commercial Orbital Transportation Services (COTS)**
  cargo model (2006-2012) proved that **fixed-price, milestone-driven**
  procurement with private ownership could undercut cost-plus development;
  NASA deliberately repeated it for crew to **end dependence on Russia's
  Soyuz** (the last Soyuz seat purchase was 2017; NASA paid ~**$90M per
  seat** vs ~$55-60M for Commercial Crew seats).
- K3: the divergence: SpaceX **flew crew first (Crew Demo-2, 2020-05-30)**
  and by 2025 had flown crew for NASA ~**15 times**; Boeing's Starliner
  OFT-1 (2019) failed to reach the ISS (software/clock errors), OFT-2
  (2022) reached it, and the **CFT (2024-06-05)** crewed flight ended in
  the uncrewed-return decision — a **fixed-price contract cannot absorb
  development overruns**: Boeing's Starliner program cumulative losses
  exceeded **$2 billion** through 2025.
- K4: the lesson drawn: the comparison became the standing evidence for
  **fixed-price vs cost-plus** in space procurement — SpaceX's parallel
  (cheaper + faster + more flights) vs Boeing's (delays + losses) — and
  drove NASA's 2025-2030 procurement posture (e.g., **$200M fixed-price
  ISS deorbit contract to SpaceX**).

## Adjacent 4 — OpenAI's model lineup consolidation (2025)

**Question:** "Why did OpenAI consolidate its model lineup in 2025, and
what happened to the products it retired?"

**Coverage key** (answered when ALL named/supported):

- K1: the consolidations: o-series retired into **GPT-5 (2025-08-07)**;
  the **GPT-4.5 "non-reasoning" line** was retired (announced 2025-07-17,
  deprecated 2025-07-31); the GPT-5 family is positioned as the single
  unified model — **"one model, no picker"**.
- K2: the earlier step: the **o4-mini → o3-mini naming** (2025-04-16
  o4-mini launch) and the **GPT-4o-era tool lines** (GPT-4o Search, code
  interpreter) were already being folded into the unified "**agentic
  everything**" experience by mid-2025.
- K3: the pricing effect: retired models' prices did NOT carry over —
  GPT-5 mini came in at **$0.25/$2 per million tokens**, ~85% cheaper than
  o3-mini's $1.10/$4.40, while GPT-5's $1.25/$10 was cheaper than o3's
  $10/$40 by an order of magnitude.
- K4: the strategic signal: the consolidation is the **"one unified model"
  thesis** — reasoning vs non-reasoning as a UI toggle is gone; the
  company's public line became "models should think by default" and the
  product competition shifted to **agent orchestration and default
  autonomy**, not model-version demos.

## Adjacent 5 — GPAI Code of Practice (2025)

**Question:** "Why did the EU's GPAI Code of Practice become the
centerpiece of AI Act compliance, and what does it require of frontier
model providers?"

**Coverage key** (answered when ALL named/supported):

- K1: the mechanism: the Code operationalizes the **general-purpose AI
  (GPAI) obligations** of the AI Act (Article 53/55) — the Act itself is
  principles-level ("sufficiently detailed" documentation, copyright
  compliance, systemic-risk duties); the Code is the **safe-zone
  interpretation** providers can sign onto to be presumed compliant.
- K2: the process: drafted through **2025** (four working groups: safety
  & security, transparency & copyright, systemic risk, internal governance)
  with **~1,000 stakeholders**; a final plenary draft was targeted for
  **April 2025** but slipped; the **first final version** was signed in
  **July 2025**, with OpenAI, Google, Microsoft, Anthropic, Meta, Mistral
  among the first signatories (reportedly several hundred companies).
- K3: the content: commitments include **AI Safety Framework
  documentation**, **red-teaming** for systemic-risk models, **copyright
  safeguards** (opt-out respect, training-data documentation), **100%
  watermarking** of AI-generated content (signed version, 2025-07), and
  transparency for downstream deployers.
- K4: the stake: signing is voluntary but **compliance-proxy** — the Code
  effectively becomes the de facto **global standard** for GPAI duties
  (the Brussels effect), and non-signatories face the Act's penalties
  (€35M/7% for systemic-risk breaches, per the Act's fines structure) with
  no presumption-of-conformity shield.

## Adjacent 6 — The endpoint-agent monoculture debate (2024-2025)

**Question:** "Why did the CrowdStrike outage spark a debate about
single-vendor concentration in cybersecurity, and what changed?"

**Coverage key** (answered when ALL named/supported):

- K1: the concentration facts: CrowdStrike held roughly **20% of the
  endpoint-protection market** and was the dominant **EDR (endpoint
  detection and response)** vendor for Fortune 500 and government; the
  outage hit **~8.5M Windows devices** in a single update — the largest
  concentration-driven outage ever demonstrated.
- K2: the debate: security executives split between "**best-of-breed
  monoculture is the price of catching breaches**" (CrowdStrike's defense —
  depth of detection) and the "**second vendor**" diversification argument
  (Microsoft/Defender as the fallback, and the EU and US regulators'
  attention); the U.S. **FCC** and **CFTC** opened inquiries into the
  incident's systemic-risk dimension.
- K3: the systemic-risk framing: the outage was the **first
  concentration-risk event of the AI-scaled software era** — a security
  product whose updates propagate in minutes across the world's
  infrastructure; the episode fed the **"digital resilience"** agenda
  (CISA's guidance, the U.S. **Cyber Safety Review Board's** March 2025
  report faulting both the update process AND the failure to use staged
  rollouts).
- K4: what changed: CrowdStrike adopted **staged canary rollouts**,
  faster **customer notification**, and published a **root-cause
  analysis (2024-08-06)** and a **Preliminary Post-Incident Review**; the
  industry moved toward **canary-in-production testing** for security
  signatures, and insurers began pricing **concentration risk** into cyber
  premiums.

## Adjacent 7 — Section 230 and the platform-content wars (2024-2025)

**Question:** "Why did the US Supreme Court decline to decide the TikTok
case on First Amendment grounds, and how did platform-content law move in
2024-2025?"

**Coverage key** (answered when ALL named/supported):

- K1: the SCOTUS posture: the Court **upheld the TikTok divest-or-ban law
  (2025-01-17, 9-0 per curiam)** explicitly on **national-security
  grounds, not content-based regulation** — distinguishing the case from
  the First Amendment platform-content precedents (e.g., Moody v. NetChoice,
  2024-07-01, where the Court struck down Texas/Florida content-moderation
  laws as **violating platforms' editorial discretion**).
- K2: the 2024-2025 content-law moves: **Moody v. NetChoice** was the
  Court's first major platform-content ruling of the modern era (unanimous
  on the core First Amendment point, 2024-07-01); the **Supreme Court
  declined** (2024-2025) to take **NetChoice v. Paxton** (Texas's HB 20)
  and the **NCLA v. Biden** cases; Congress's **Kids Online Safety Act
  (KOSA)** stalled in the House through 2025.
- K3: the through-line: courts kept treating **content moderation as
  editorial discretion** (protected) while treating **data-access and
  ownership questions** (TikTok's data flows, the divestiture itself) as
  ordinary commercial regulation — the split that let the TikTok law
  survive where moderation laws failed.
- K4: the 2025 Trump-era shift: the administration's **2025-01-20
  executive order** paused the TikTok ban's enforcement and called for a
  "**privatized**" resolution — the EO explicitly invoked the First
  Amendment rationale the Court had declined to adopt, setting up the
  tension the Oracle-led JV resolved without litigation.

## Adjacent 8 — The Microsoft antitrust legacy (1998-2025)

**Question:** "Why is the Google search-monopoly remedy compared to
United States v. Microsoft, and what is different about the two cases?"

**Coverage key** (answered when ALL named/supported):

- K1: the Microsoft precedent: **United States v. Microsoft Corp.**
  (2000): the DC Circuit found Microsoft **monopolized the PC operating
  system market** through exclusionary dealing (OEM licensing) and
  **browser-bundling** (IE with Windows); the DOJ's proposed **breakup**
  was vacated on appeal (2001), and the 2001 **settlement** imposed
  conduct remedies (API disclosure, licensing terms) — not divestiture.
- K2: the parallel drawn: Judge Mehta's 2024-08-05 ruling cites the
  Microsoft case's **"monopolist's means"** analysis (exclusionary
  conduct = a monopolist's maintenance of its monopoly by other means than
  competition on the merits); Google's default deals are the analogue of
  Microsoft's OEM exclusivity — **distribution, not products**, is the
  contested ground.
- K3: the difference: the 2025-08-05 remedy goes **further than
  Microsoft's**: a **forced sale of Chrome** — an actual structural
  remedy — where Microsoft got conduct remedies only; and the remedy
  order's **ad tech auction rules** (no self-preferencing in search
  advertising, no aggregation of ad-tech data) target the **advertising
  market** Microsoft's case never reached.
- K4: the stakes: the case is the **first big-tech structural remedy of
  the modern internet era**; the DOJ's position (2024-10-16 proposed
  remedies: sell Chrome, possibly sell Android, ban default deals, limit
  AI investments) explicitly argued that **conduct remedies had failed
  against Microsoft** and only structural relief would restore
  competition — the remedy record is the test of whether that argument
  persuades the DC Circuit.

## Adjacent 9 — Anthropic's funding and governance arc (2023-2025)

**Question:** "Why did Anthropic's valuation and governance structure
change so dramatically between 2023 and 2025?"

**Coverage key** (answered when ALL named/supported):

- K1: the governance path: founded 2021 as a **public-benefit corporation**
  with a **Long-Term Benefit Trust** (2023) to control the board; by
  **2025** the Trust structure was **dissolved** (announced 2025-02-27) in
  favor of a more conventional board of **largely independent directors**
  (including former NSA head Paul Nakasone), after reported friction with
  investors over the Trust's power.
- K2: the funding arc: Google invested **~$2B (Oct 2023, convertible)**;
  Amazon **$4B (Sep 2024)**; the **$8B Series E (Mar 2025, ~$60B
  valuation)** led by Lightspeed; then the late-September 2025 round at a
  reported **~$183B valuation** (~$19B raised), cementing Anthropic as
  the second-most-valuable AI company after OpenAI.
- K3: the revenue engine: the round was driven by **Claude Code and
  enterprise agentic-coding adoption** — reported **annualized revenue
  run rate ~$6-7B by mid-2025**, up from ~$1B a year earlier, with API
  usage the majority of revenue.
- K4: the strategic signal: the shift from the Trust to conventional
  governance was read as Anthropic **maturing from an unusual
  "benefit-first" experiment to a conventional frontier lab** — the
  company kept its public-benefit incorporation but the governance
  machinery that made it structurally different was retired, and the
  ~$183B valuation priced Anthropic on **agentic-coding leadership**, not
  safety differentiation alone.

## Adjacent 10 — Reasoning models and the API economy (2025)

**Question:** "Why did the price of frontier reasoning fall by an order of
magnitude in 2025, and what did that do to the AI-application market?"

**Coverage key** (answered when ALL named/supported):

- K1: the price curve: o3 at **$10/$40** (2025-04-16) → GPT-5 at
  **$1.25/$10** (2025-08-07) → **DeepSeek R1's open-weights
  price-collapse** and the **1,000x-cost-drop narrative** over 2023-2025
  (GPT-3.5-class pricing as the denominator); the **"price-performance
  doubling every few months"** framing the major labs published in 2025.
- K2: the mechanism: **open-weight reasoning models** (DeepSeek R1,
  Qwen3, Llama 4, Gemma 3) set a price floor under API pricing; labs
  competed on **context size and agentic reliability** (GPT-5's 5M-token
  window, Claude 4's computer use) rather than per-token price; and
  **inference-efficiency work** (speculative decoding, distillation,
  KV-cache sharing) cut serving cost per token.
- K3: the market effect: **agentic applications** — coding agents, deep
  research, browser agents — became economically viable at scale
  ("**a dollar an hour of agent work**" framing), and API usage shifted
  from chat completions to **long-horizon agent runs** (tool loops,
  multi-thousand-turn sessions), which became the pricing battleground
  (per-session vs per-token models, e.g., **Cursor's usage-based plans**).
- K4: the frontier consequence: with reasoning commoditized, the 
  differentiation moved to **frontier pretraining scale** (GPT-5-class
  model size), **context/agents**, and **distribution** (free tiers,
  device-native deployment) — the "**last pretraining race**" framing
  that dominated 2025 H2 industry commentary.

## Adjacent 11 — The $4 trillion club (2025)

**Question:** "Why did three companies pass $4 trillion in market value in
2025, and what does the rotation among them show?"

**Coverage key** (answered when ALL named/supported):

- K1: the members: **Nvidia** (closed above $4T 2025-07-09), **Apple**
  (first to $4T intraday, 2025-07-08; closed above $4T on 2025-08-13),
  and **Microsoft** (closed above $4T for the first time on 2025-08-21) —
  the first $4T closes in history.
- K2: the rotation: the three traded the **#1 spot repeatedly through
  2025** — Nvidia briefly passed Apple as the world's most valuable
  company in June 2025 (2025-06-12), Apple retook it, Nvidia retook it in
  July, and the three rotated through the fall; no company had been above
  $3T before 2024 (Apple was first, 2023-06-30 intraday).
- K3: the drivers: Nvidia — **AI infrastructure demand** (Blackwell,
  hyperscaler capex); Apple — **AI features in the iPhone/iOS** (Apple
  Intelligence, the Siri-AI refresh cycle, services growth); Microsoft —
  **Azure + OpenAI partnership + AI-copilot monetization** (the most
  diversified of the three across AI infra and software).
- K4: the concentration signal: the three (plus Alphabet/Amazon/Meta)
  pushed the **top-10 S&P 500 weight to record levels** (~40%+ by late
  2025), reviving the **concentration-risk debate** — the index's returns
  increasingly tracking the AI-capex complex; the 2025-07 **ROSCA/vol
  events** and the late-2025 AI-capex "**digestion**" pullback (Oct 2025)
  tested the thesis that the $4T club's fate is one capex-cycle away.

## Adjacent 12 — Windows privacy defaults (2024-2025)

**Question:** "Why did Windows' privacy posture become a compliance
liability for Microsoft in Europe in 2024-2025?"

**Coverage key** (answered when ALL named/supported):

- K1: the legal arc: the **European Commission's 2023-06-01 complaint**
  about Windows 11's **data-collection defaults** (telemetry,
  advertising IDs, "recommended content" toggles) led to **binding
  commitments from Microsoft (2025-03-20)** — the first **DMA-style
  settlement for Windows** — including a **decline-with-one-click**
  consent flow, **default-off** advertising ID for EU users, and separate
  toggles for data-sharing categories.
- K2: the Recall intersection: the **2024 Recall episode** (screenshots
  stored in a plaintext SQLite database, default-on) became the proof
  case for the regulators' argument — a feature that records everything
  on screen, including passwords and private messages, with **no EU
  privacy review before release**; the Irish DPC opened a formal inquiry
  (2025) and Microsoft's revised Recall keeps **default-off** posture
  worldwide.
- K3: the compliance machinery: Microsoft built a **"compliance manager"
  surface** (the EU Data Boundary, EU Data Residency, and the 2025
  Windows "privacy dashboard" rework) as the enterprise-facing answer;
  the 2025-03-20 commitments apply to **Windows 11 (24H2+) in the EEA**
  with enforcement via the **Digital Markets Act**'s penalty regime
  (up to 10% of global turnover).
- K4: the causal link: the defaults were **revenue-bearing** (advertising
  ID, Bing/Edge promotion, Copilot integration) — Microsoft shipped
  profit-optimizing defaults that the EU's DMA/DSA era treats as
  **self-preferencing and dark patterns**; the commitments are the
  blueprint other platform vendors (Google's Chrome, Meta's data
  practices) are measured against in the same enforcement wave.
