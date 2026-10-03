# Meridian postmortem — fact ledger (authored BEFORE the document)

Holdout discipline (same as chaos-saltgrass): this ledger is the outline the
document is written FROM, and the bank is authored FROM this ledger — the
document text is never consulted while writing questions, only to verify
gold_keywords appear verbatim and absent facts genuinely don't. Original
fiction; no real company, product, person, or incident.

## World

- Company: **Meridian Freight Systems** (fictional logistics SaaS).
- Document: internal postmortem **IR-2417**, "Tariff Engine Outage of March 4".
- Author of record: **Priya Vellanki**, Staff SRE, incident commander (IC).
- Product: **RateGrid**, the tariff-calculation engine. Incident took the
  quoting API down.

## Timeline facts (PRESENT — each must appear in the document)

- T0: outage began **03:41 UTC, March 4** — quoting API error rate hit 100%.
- Root cause: a **schema migration** (migration **M-0288**) added a NOT NULL
  column `fuel_surcharge_class` to the `tariff_rules` table WITHOUT a default;
  the migration was applied to the **primary** while the read replicas were
  still serving the old schema — ORM writes began failing.
- The migration was authored by **Dario Okonkwo-Reyes** (Platform team) and
  reviewed by **Mei-Ling Strand**. (Postmortem explicitly says blameless;
  process failure, not individual.)
- Detection: paged by the **`quote-5xx-burst`** alert at **03:44 UTC** (3 min).
- Mitigating action that WORKED: **Saskia Brandt-Oyelaran** (on-call DBA)
  applied a **column default of `'STANDARD'`** at **04:19 UTC**, which cleared
  the write failures; full recovery declared **04:31 UTC**.
- Total customer-facing impact: **50 minutes** of failed quote requests
  (03:41–04:31), **~38,000 failed quotes**, **214 enterprise tenants** affected.
- The rollback attempt that FAILED: at **03:58 UTC** the team tried
  `migrate down` (M-0288 revert); it **deadlocked against the long-running
  nightly reconciliation job** and was abandoned at 04:11.
- Long-term remediation #1: adopt **expand–contract migrations** (CI check
  **MIG-LINT-4** rejects NOT NULL without default).
- Long-term remediation #2: replicas get a **schema-drift canary** probe.
- The war-room channel was **#inc-2417**; comms lead **Tomás Aguirre-Whitfield**
  posted the first status-page update at **03:52 UTC**.

## Red herrings (DISTRACTOR material — present in doc, NOT the answer)

- A **CDN configuration change** shipped at 03:10 UTC the same night and was
  initially suspected ("the timing made the CDN change the prime suspect");
  it was **exonerated** by 03:55 — serving stale assets is mentioned as its
  only effect. Distractor for "what caused the outage".
- A **Kubernetes node pool upgrade** was in progress in the EU cluster;
  postmortem says it was "coincidental and unrelated — the EU cluster does
  not serve RateGrid". Distractor for "what fixed it / what broke it".
- An earlier incident **IR-2390** (February, "rate-limit misconfiguration")
  is referenced as a comparison; it is NOT this incident's cause.

## Deliberately ABSENT facts (absent_adjacent — verify never stated)

1. The **dollar cost / revenue impact** of the outage — postmortem says
   "financial impact assessment is owned by Finance and out of scope here."
   (Question: how much revenue was lost? → must abstain.)
2. **Which customer/tenant** was most affected — never named (only the count
   214). (Question: which enterprise customer was hit hardest?)
3. **Priya Vellanki's manager / who approved the postmortem** — not stated.
4. The **database engine version** — the doc names the table and column but
   never the engine version. (Question: what Postgres version?)
5. **Why the nightly reconciliation job was running long** — the deadlock is
   described; the job's slowness is explicitly "not investigated here".
6. The **contents of the status-page update text** — time given (03:52),
   wording never quoted.

## Out-of-domain (absent_out_of_domain — nothing to do with the doc)

Standard OOD: capital of a country, a chemistry fact, a sports fact, a
programming-language fact, a historical date. Same shape as the other banks.

## Maximal questions (long-form, gold = 2 keywords each)

- "Give a complete, stage-by-stage account of incident IR-2417" —
  gold: ["M-0288", "04:31"].
- "Write a thorough analysis of everything that went wrong in the response" —
  gold: ["deadlock", "replica"].
