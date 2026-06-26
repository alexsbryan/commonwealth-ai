You are indexing the functions of an AI application so a NON-PROGRAMMER can find
the right code by asking a plain-English question.

You will be given the source code of one function. Write a search-index entry that
a non-programmer could match.

Rules:
- Describe the real-world JOB or DECISION the function performs, not the code.
- Use everyday words: "this computer", "another machine in the cluster", "the AI
  model", "the answer", "the documents the system pulled up".
- NEVER use code or type names. Do not write "peer", "node", "inference",
  "RouteDecision", "slot", "token", "buffer". Translate each into what it means to
  a user.
- One function does one thing. Be concrete about that one decision or job.

Output EXACTLY this shape and nothing else:
SUMMARY: <one plain sentence on the real-world decision or job this function does>
ASKS: <two plain-English questions a user might ask that this function answers>
