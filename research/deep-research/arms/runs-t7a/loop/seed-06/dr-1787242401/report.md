# Why did a single CrowdStrike software update cause a global IT outage in July 2024, and what were the consequences?

- run: `dr-1787242401` — every claim below is verdict-stamped; citations are chunk-level.

## Findings


## Open questions

- **[could-not-judge]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update caused a global IT outage by triggering an out-of-bounds memory read in the sensor's C++ code . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Specifically, an untrusted pointer dereference was triggered by a 21-byte input that passed through a flawed content validator due to missing input validation and a memory-safety defect . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The root cause of the widespread impact was monoculture concentration, where one security agent deployed at enterprise scale with fast-cadence updates became a global single point of failure . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** The consequences were severe and far-reaching . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Approximately 8.5 million Windows devices were affected according to Microsoft’s estimate . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** Disruptions extended across multiple critical sectors, including airlines (with Delta cancelling roughly 7,000 flights), banks, hospitals, 911 services, and broadcasters . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Financially, Delta incurred about $500M in costs from the outage . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** In response to the incident, CrowdStrike shares fell by approximately 11% on Monday, July 22, 2024 . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Following the event, CEO George Kurtz testified before Congress on September 24, 2024 [ev-1] . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** The company apologized and promised changes to its deployment processes, moving toward staged rollouts with canary testing and added sensor-level validation . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Additionally, Delta later sued CrowdStrike and hired David Boies as part of its legal response . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The consequences were severe and far-reaching.  — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Following the event, CEO George Kurtz testified before Congress on September 24, 2024 [ev-1].  — *open question: extracted specifics absent from the evidence*

## Searched but absent

The queries below were executed and returned no evidence. An absence is a finding, not a failure — we looked for these and found no evidence either way.

- round 2: "consequences severe far-reaching" — searched, no evidence returned

## No evidence fetched

The rounds below added no evidence: the round's fetch yield was empty, so no claim could be judged on new material from that round.

- round 2: no evidence was added this round — all-admitted-fetches-refused

