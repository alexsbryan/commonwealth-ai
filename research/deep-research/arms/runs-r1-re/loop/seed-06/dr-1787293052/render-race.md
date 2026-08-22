# Why did a single CrowdStrike software update cause a global IT outage in July 2024, and what were the consequences?

_run: `dr-1787293052` — 0 findings established; 11 claims open. Citations are chunk-level (evidence id + source URL)._

## Findings


## Open questions

- **[open question]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update caused a global IT outage because it contained an out-of-bounds memory read in the sensor's C++ code . — *single-origin support (corroboration floor)*
- **[open question]** Specifically, an untrusted pointer dereference was triggered by a 21-byte input that passed through a flawed content validator due to missing input validation and a memory-safety defect . — *single-origin support (corroboration floor)*
- **[open question]** The root cause of the widespread impact was monoculture concentration, where one security agent deployed at enterprise scale with fast-cadence updates became a global single point of failure . — *extracted specifics absent from the evidence*
- **[open question]** The consequences were severe and far-reaching. — *single-origin support (corroboration floor)*
- **[open question]** Approximately 8.5 million Windows devices were affected according to Microsoft’s estimate . — *extracted specifics absent from the evidence*
- **[open question]** Airlines grounded flights, with Delta cancelling roughly 7,000 flights and incurring about $500M in costs before suing CrowdStrike . — *single-origin support (corroboration floor)*
- **[open question]** Additionally, banks, hospitals, 911 services, and broadcasters experienced disruptions . — *single-origin support (corroboration floor)*
- **[open question]** Financially, CrowdStrike shares fell by approximately 11% on July 22, 2024 . — *single-origin support (corroboration floor)*
- **[open question]** In response to the incident, CEO George Kurtz testified before Congress on September 24, 2024; the company apologized and promised deployment-process changes, including staged rollouts with canary testing and added sensor-level validation . — *single-origin support (corroboration floor)*
- **[open question]** The episode also drove industry-wide discussion regarding chaos-engineering and blast-radius limits for endpoint agents . — *single-origin support (corroboration floor)*
- **[open question]** The consequences were severe and far-reaching.  — *single-origin support (corroboration floor)*

