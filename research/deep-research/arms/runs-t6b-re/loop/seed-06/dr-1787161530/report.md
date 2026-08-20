# Why did a single CrowdStrike software update cause a global IT outage in July 2024, and what were the consequences?

- run: `dr-1787161530` — every claim below is verdict-stamped; citations are chunk-level.

## Findings


## Open questions

- **[could-not-judge]** The global IT outage in July 2024 was caused by a faulty CrowdStrike Falcon Sensor content update that contained an out-of-bounds memory read in the sensor's C++ code . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Specifically, this defect involved an untrusted pointer dereference triggered by a 21-byte input that bypassed a flawed content validator due to missing input validation and a memory-safety defect . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The scale of the disruption resulted from monoculture concentration; one security agent deployed at enterprise scale with fast-cadence updates became a single point of failure for approximately 8.5 million Windows devices worldwide . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** The consequences included widespread disruptions across multiple sectors, including airlines, banks, hospitals, broadcasters, and 911 services . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Delta Air Lines cancelled roughly 7,000 flights and incurred about $500M in costs before suing CrowdStrike . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Financially, CrowdStrike shares fell by about 11% on July 22, 2024 . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** In response to the incident, CEO George Kurtz testified before Congress on September 24, 2024; the company apologized and implemented staged rollouts with canary testing and sensor-level validation, while the episode drove industry-wide discussions regarding chaos engineering and blast-radius limits for endpoint agents . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update caused a global IT outage because it contained an out-of-bounds memory read in the sensor's C++ code . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Specifically, an untrusted pointer dereference was triggered by a 21-byte input that bypassed a flawed content validator due to missing input validation and a memory-safety defect . — *open question: single-origin support (corroboration floor)*

