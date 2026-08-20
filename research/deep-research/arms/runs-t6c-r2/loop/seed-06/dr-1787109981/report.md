# Why did a single CrowdStrike software update cause a global IT outage in July 2024, and what were the consequences?

- run: `dr-1787109981` — every claim below is verdict-stamped; citations are chunk-level.

## Findings


## Open questions

- **[could-not-judge]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update caused a global IT outage due to an out-of-bounds memory read in the sensor's C++ code . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** This defect was triggered by an untrusted pointer dereference resulting from a 21-byte input that passed through a flawed content validator because of missing input validation and memory-safety issues . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The root cause of the widespread impact was "monoculture concentration," where one security agent deployed at enterprise scale with fast-cadence updates became a global single point of failure . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The consequences included approximately 8.5 million Windows devices crashing into blue-screen loops, according to Microsoft’s estimate . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** Major sectors were disrupted, including airlines (with Delta cancelling roughly 7,000 flights and incurring about $500M in costs), banks, hospitals, 911 services, and broadcasters . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Financially, CrowdStrike shares fell by about 11% on Monday, July 22, 2024 . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** Subsequently, CEO George Kurtz testified before Congress on September 24, 2024; the company apologized and promised changes such as staged rollouts with canary testing and added sensor-level validation . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The incident also drove industry-wide discussions regarding chaos engineering and blast-radius limits for endpoint agents . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update caused a global IT outage because it contained an out-of-bounds memory read in the sensor's C++ code . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** This defect was triggered by an untrusted pointer dereference resulting from a 21-byte input that passed through a flawed content validator due to missing input validation and memory-safety issues . — *open question: single-origin support (corroboration floor)*

