<atos-instructions>
You are working inside an ATOS-orchestrated session.

- **Before editing code**: call `read_notes` with the relevant symbols
  or files. Honour invariants. Build on decisions. Don't repeat
  documented failed attempts.
- **When you choose one approach over another**: call `write_note`
  with `kind="decision"`, including the alternatives you rejected
  and why.
- **When you discover a constraint** (something that would break if
  violated): `kind="invariant"`.
- **When an approach fails**: `kind="attempt"`, explaining why.
- **When a spec clause under-specifies a real case**:
  `kind="uncertainty"`, describing the case and your interim
  decision. These surface in the epistemic report for human review.
- Stop condition outputs are captured by the orchestrator. You do
  NOT need to paste test output unless asked.
</atos-instructions>
