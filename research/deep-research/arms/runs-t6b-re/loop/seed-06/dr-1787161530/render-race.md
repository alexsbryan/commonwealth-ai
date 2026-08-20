# Why did a single CrowdStrike software update cause a global IT outage in July 2024, and what were the consequences?

_run: `dr-1787161530` — 0 findings established; 9 claims open. Citations are chunk-level (evidence id + source URL)._

## Findings


## Open questions

- **[open question]** The global IT outage in July 2024 was caused by a faulty CrowdStrike Falcon Sensor content update that contained an out-of-bounds memory read in the sensor's C++ code . — *single-origin support (corroboration floor)*
- **[open question]** Specifically, this defect involved an untrusted pointer dereference triggered by a 21-byte input that bypassed a flawed content validator due to missing input validation and a memory-safety defect . — *single-origin support (corroboration floor)*
- **[open question]** The scale of the disruption resulted from monoculture concentration; one security agent deployed at enterprise scale with fast-cadence updates became a single point of failure for approximately 8.5 million Windows devices worldwide . — *extracted specifics absent from the evidence*
- **[open question]** The consequences included widespread disruptions across multiple sectors, including airlines, banks, hospitals, broadcasters, and 911 services . — *single-origin support (corroboration floor)*
- **[open question]** Delta Air Lines cancelled roughly 7,000 flights and incurred about $500M in costs before suing CrowdStrike . — *single-origin support (corroboration floor)*
- **[open question]** Financially, CrowdStrike shares fell by about 11% on July 22, 2024 . — *single-origin support (corroboration floor)*
- **[open question]** In response to the incident, CEO George Kurtz testified before Congress on September 24, 2024; the company apologized and implemented staged rollouts with canary testing and sensor-level validation, while the episode drove industry-wide discussions regarding chaos engineering and blast-radius limits for endpoint agents . — *single-origin support (corroboration floor)*
- **[open question]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update caused a global IT outage because it contained an out-of-bounds memory read in the sensor's C++ code . — *single-origin support (corroboration floor)*
- **[open question]** Specifically, an untrusted pointer dereference was triggered by a 21-byte input that bypassed a flawed content validator due to missing input validation and a memory-safety defect . — *single-origin support (corroboration floor)*

