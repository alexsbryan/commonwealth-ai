# Why did a single CrowdStrike software update cause a global IT outage in July 2024, and what were the consequences?

_run: `dr-1787178512` — 0 findings established; 7 claims open. Citations are chunk-level (evidence id + source URL)._

## Findings


## Open questions

- **[open question]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update crashed Windows machines worldwide in blue-screen loops . — *single-origin support (corroboration floor)*
- **[open question]** The root cause was an out-of-bounds memory read in the sensor's C++ code, specifically triggered by an untrusted pointer dereference from a 21-byte input that passed a flawed content validator due to missing input validation and a memory-safety defect . — *single-origin support (corroboration floor)*
- **[open question]** This incident affected roughly 8.5 million Windows devices globally because one security agent deployed at enterprise scale with fast-cadence updates became a global single point of failure . — *extracted specifics absent from the evidence*
- **[open question]** The consequences were severe across multiple sectors, including grounded airlines (with Delta cancelling approximately 7,000 flights), disruptions to banks, hospitals, 911 services, and broadcasters . — *single-origin support (corroboration floor)*
- **[open question]** Financially, Delta incurred about $500M in costs and later sued CrowdStrike while hiring David Boies . — *single-origin support (corroboration floor)*
- **[open question]** In the stock market, CrowdStrike shares fell by about 11% on Monday, July 22, 2024 . — *single-origin support (corroboration floor)*
- **[open question]** Additionally, CEO George Kurtz testified before Congress on September 24, 2024 after the company apologized and promised deployment-process changes . — *single-origin support (corroboration floor)*

