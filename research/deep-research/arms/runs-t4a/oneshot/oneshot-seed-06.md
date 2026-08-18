On July 19, 2024 [ev-1], a global IT outage occurred because CrowdStrike pushed a faulty "Rapid Response Content" update to its Falcon Sensor [ev-1]. The root cause was identified as an out-of-bounds memory read in the sensor’s C++ code, specifically an untrusted pointer dereference triggered by a 21-byte input that passed a flawed content validator due to missing input validation and a memory-safety defect [ev-1].

The consequences of this single point of failure were severe:
*   **Scale:** Approximately 8.5 million Windows devices crashed into blue-screen loops [ev-1].
*   **Industry Disruption:** Airlines grounded operations (with Delta cancelling roughly 7,000 flights), while banks, hospitals, broadcasters, and 911 services suffered disruptions [ev-1].
*   **Financial Impact:** Delta incurred about $500M in costs and later sued CrowdStrike after hiring David Boies [ev-1].
*   **Market Reaction:** CrowdStrike shares fell by approximately 11% on Monday, July 22, 2024 [Source: ev-1] [ev-1].

In response to the incident, CEO George Kurtz testified before Congress on September 24, 2024, as the company apologized and promised deployment-process changes [ev-1]. The episode drove industry-wide discussions regarding chaos engineering and blast-radius limits for endpoint agents, leading CrowdStrike to adopt staged rollouts with canary testing and added sensor-level validation [ev-1].