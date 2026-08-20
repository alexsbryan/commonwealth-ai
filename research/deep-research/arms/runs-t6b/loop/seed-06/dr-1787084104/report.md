# Why did a single CrowdStrike software update cause a global IT outage in July 2024, and what were the consequences?

- run: `dr-1787084104` — every claim below is verdict-stamped; citations are chunk-level.

## Findings


## Open questions

- **[could-not-judge]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update caused a global IT outage by triggering an out-of-bounds memory read in the sensor's C++ code . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** This defect was triggered by a 21-byte input that passed a flawed content validator due to missing input validation and memory-safety issues . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The root cause of the widespread impact was "monoculture concentration," where one security agent deployed at enterprise scale with fast-cadence updates became a global single point of failure . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The consequences included approximately 8.5 million Windows devices crashing into blue-screen loops, according to Microsoft’s estimate . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** Significant disruptions occurred across multiple sectors, including airlines, banks, hospitals, 911 services, and broadcasters . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Specifically, Delta Air Lines cancelled roughly 7,000 flights and incurred about $500M in costs before suing CrowdStrike and hiring David Boies . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Financially, CrowdStrike shares fell by about 11% on July 22, 2024 . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** In response to the incident, CEO George Kurtz testified before Congress on September 24, 2024; the company apologized and promised deployment-process changes . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** Additionally, CrowdStrike moved to staged rollouts with canary testing and added sensor-level validation, while the episode drove industry-wide discussion of chaos-engineering and blast-radius limits for endpoint agents . — *open question: single-origin support (corroboration floor)*

