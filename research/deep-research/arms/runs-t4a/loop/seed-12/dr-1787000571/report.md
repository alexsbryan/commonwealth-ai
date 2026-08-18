# Why did Microsoft's Recall feature for Windows become a privacy scandal in 2024, and how was it changed?

- run: `dr-1787000571` — every claim below is verdict-stamped; citations are chunk-level.

## Findings


## Open questions

- **[could-not-judge]** Microsoft's Recall feature, announced on May 20, 2024 , became a privacy scandal because security researchers demonstrated that the screenshots it captured were stored unencrypted in plaintext within a local SQLite database, contradicting Microsoft’s claims of encryption . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** This insecure implementation allowed any process running as the user to access sensitive content visible on screen by default, including passwords and private messages . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The combination of this security failure and the feature’s initial design to record everything on screen (including health data) created a significant collision with privacy expectations . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** In response to the controversy and inquiries from regulators such as the Irish Data Protection Commission and the UK Information Commissioner's Office, Microsoft delayed the release in June 2024 and subsequently changed the feature significantly . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Recall was relaunched as an opt-in feature that is off by default, rather than enabled automatically . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Additionally, accessing the timeline now requires Windows Hello authentication and presence detection, while incognito and DRM content are explicitly excluded from capture . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The revised version of the feature reached general availability on Copilot+ PCs in 2025 . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Microsoft's Recall feature became a privacy scandal in 2024 because, despite claims of encryption, security researchers demonstrated that the screenshots it captured every few seconds were stored unencrypted (in plaintext) within a local SQLite database . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** This storage method allowed any process running as the user to access sensitive content visible on screen, including passwords and private messages, by default . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The discrepancy between Microsoft’s "encrypted" claims and the actual insecure implementation, combined with the default-on nature of recording everything on screen—including health data—created a major collision with privacy expectations . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** In response to this controversy and inquiries from regulators such as the Irish Data Protection Commission and the UK Information Commissioner's Office, Microsoft changed the feature significantly . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Initially delayed in June 2024, Recall was relaunched as an opt-in feature (default OFF) rather than being enabled automatically . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Additionally, access to the timeline now requires Windows Hello authentication and presence detection, while incognito and DRM content are explicitly excluded from capture . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The revised version reached general availability on Copilot+ PCs in 2025 . — *open question: single-origin support (corroboration floor)*

