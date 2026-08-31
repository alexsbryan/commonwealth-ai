# CLA signatures

This orphan branch is the signature store for `.github/workflows/cla.yml`
(`contributor-assistant/github-action`). It carries no project code and is
never merged.

`signatures/version1/cla.json` records one entry per contributor who has
signed [CLA.md](https://github.com/alexsbryan/commonwealth-ai/blob/main/CLA.md)
— GitHub id, login, PR number, timestamp. The action reads it on every PR and
appends to it when someone comments the sign phrase.

The branch must exist before the action can write: the GitHub Contents API
creates files, not branches, so a missing branch fails every PR check with
"Branch cla-signatures not found".

To revoke every signature (e.g. the CLA text changes materially), empty
`signedContributors` back to `[]`; everyone re-signs on their next PR.
Do not protect this branch — the action commits to it directly.
