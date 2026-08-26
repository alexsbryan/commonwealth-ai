On July 19, 2024 [ev-1], a faulty CrowdStrike Falcon Sensor content update caused a global IT outage by triggering an out-of-bounds memory read in the sensor's C++ code [ev-1]. Specifically, a flawed content validator allowed a 21-byte input to pass through, leading to an untrusted pointer dereference and missing input validation that crashed Windows machines into blue-screen loops [ev-1]. The root cause was identified as monoculture concentration, where one security agent deployed at enterprise scale with fast-cadence updates became a single point of failure [ev-1].

The consequences were severe and widespread:
*   Approximately 8.5 million Windows devices were affected worldwide [ev-1].
*   Airlines grounded flights; Delta specifically cancelled roughly 7,000 flights and incurred about $500M in costs before suing CrowdStrike [ev-1].
*   Disruptions extended to banks, hospitals, broadcasters, and 911 services [ev-1].
*   CrowdStrike shares fell by about 11% on July 22, 2024, following the incident [ev-1].
*   CEO George Kurtz testified before Congress on September 24, 2024 as part of the aftermath [ev-1].

In response, CrowdStrike apologized, promised deployment-process changes including staged rollouts with canary testing, added sensor-level validation, and the episode drove industry-wide discussions regarding chaos-engineering and blast-radius limits for endpoint agents [ev-1].