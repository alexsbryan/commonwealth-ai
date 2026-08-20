# Why did a single CrowdStrike software update cause a global IT outage in July 2024, and what were the consequences?

- run: `dr-1787178512` — every claim below is verdict-stamped; citations are chunk-level.

## Findings


## Open questions

- **[could-not-judge]** On July 19, 2024, a faulty CrowdStrike Falcon Sensor content update crashed Windows machines worldwide in blue-screen loops . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The root cause was an out-of-bounds memory read in the sensor's C++ code, specifically triggered by an untrusted pointer dereference from a 21-byte input that passed a flawed content validator due to missing input validation and a memory-safety defect . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** This incident affected roughly 8.5 million Windows devices globally because one security agent deployed at enterprise scale with fast-cadence updates became a global single point of failure . — *open question: extracted specifics absent from the evidence*
- **[could-not-judge]** The consequences were severe across multiple sectors, including grounded airlines (with Delta cancelling approximately 7,000 flights), disruptions to banks, hospitals, 911 services, and broadcasters . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Financially, Delta incurred about $500M in costs and later sued CrowdStrike while hiring David Boies . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** In the stock market, CrowdStrike shares fell by about 11% on Monday, July 22, 2024 . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Additionally, CEO George Kurtz testified before Congress on September 24, 2024 after the company apologized and promised deployment-process changes . — *open question: single-origin support (corroboration floor)*

