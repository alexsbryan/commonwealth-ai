The global IT outage occurred on July 19, 2024 [Source: ev-1], when a faulty CrowdStrike Falcon Sensor content update caused Windows machines to crash in blue-screen loops [Source: ev-1]. The root cause was identified as an out-of-bounds memory read in the sensor's C++ code, specifically triggered by a 21-byte input that passed a flawed content validator due to missing input validation and a memory-safety defect [Source: ev-1]. This incident affected roughly 8.5 million Windows devices worldwide [Source: ev-1].

The consequences were severe and widespread across multiple sectors [Source: ev-1]:
*   **Aviation:** Airlines grounded flights; Delta cancelled approximately 7,000 flights and incurred about $500M in costs, leading them to sue CrowdStrike [Source: ev-1].
*   **Critical Services:** Banks, hospitals, broadcasters, and 911 services experienced disruptions [Source: ev-1].
*   **Financial Impact:** CrowdStrike shares fell by about 11% on Monday, July 22, 2024 following the incident [Source: ev-1].
*   **Corporate Response & Accountability:** CEO George Kurtz testified before Congress on September 24, 2024, after which the company apologized and promised changes to its deployment processes [Source: ev-1].

The primary causal factor was monoculture concentration, where a single security agent deployed at enterprise scale with fast-cadence updates became a global single point of failure [Source: ev-1]. In response, CrowdStrike moved toward staged rollouts with canary testing and added sensor-level validation [Source: ev-1].