# Why did Microsoft's Recall feature for Windows become a privacy scandal in 2024, and how was it changed?

- run: `dr-1787085024` — every claim below is verdict-stamped; citations are chunk-level.

## Findings


## Open questions

- **[could-not-judge]** Microsoft's Recall feature became a privacy scandal in 2024 because, although it was announced on May 20, 2024  as an AI "memory" tool that screenshots the screen every few seconds to provide searchable local memory , security researchers demonstrated critical flaws in its implementation. — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Specifically, despite Microsoft’s claims of encryption, the feature stored these screenshots unencrypted (in plaintext) within a local SQLite database . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** This design allowed sensitive content visible on the user’s screen—including passwords and private messages—to be captured by default and made accessible to any process running with user privileges . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The collision between this default-on recording capability and privacy expectations triggered scrutiny from regulatory bodies, including the Irish Data Protection Commission, the UK Information Commissioner's Office, and an investigation opened by the Washington State Attorney General . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** In response to these security failures and public backlash, Microsoft delayed the launch in June 2024 and reversed its implementation strategy . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The relaunched feature is now opt-in (default OFF), requires Windows Hello authentication and presence detection to access the timeline, and explicitly excludes incognito browser windows and DRM content from capture . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** This revised version reached general availability on Copilot+ PCs in 2025 . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** Microsoft’s Recall feature became a privacy scandal in 2024 because, despite being marketed as an AI "memory" tool for Copilot+ PCs announced on May 20, 2024 , security researchers demonstrated that it stored screenshots of the screen unencrypted (in plaintext) within a local SQLite database, contradicting Microsoft's claims of encryption . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** This design flaw meant that sensitive content visible on the user's screen—including passwords and private messages—was captured by default and remained accessible to any process running with user privileges . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The collision between this default-on recording capability and privacy expectations led regulatory bodies such as the Irish Data Protection Commission, the UK Information Commissioner's Office, and the Washington State Attorney General to raise questions or open investigations . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** In response to these security failures and public backlash, Microsoft delayed the feature in June 2024 and subsequently reversed its implementation strategy . — *open question: single-origin support (corroboration floor)*
- **[could-not-judge]** The relaunched Recall is now opt-in (default OFF), requires Windows Hello authentication and presence detection to access the timeline, and explicitly excludes incognito browser windows and DRM content from capture . — *open question: single-origin support (corroboration floor)*

