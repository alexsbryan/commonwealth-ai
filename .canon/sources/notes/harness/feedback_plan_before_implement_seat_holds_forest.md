# Sit with information and objective and plan; don't jump to implementing. The seat holds the forest — measuring then tuning is…

Operator direction, 2026-08-12: *"I want to maybe try to align on a workflow where we sit with information and objective and do more planning and less wild implementing. It's much harder to see the forest once you're in amongst the trees."*

And the sharper version, on a draft order: *"the one difficulty of studying the system, measuring then trying to tune is that you are fundamentally falling into an anchor and adjust fallacy."*

Why: measuring the incumbent and then asking "which levers reduce this" silently concedes every current component's right to exist and negotiates only its price. Ask "how do we make audit#2 cheaper" and you have already decided audit#2 exists. The method that works here is the one the NATIVE_GROUNDING spec used: name what each stage accomplishes (implementation-free), derive the cheapest mechanism for that function, ask whether the function survives once upstream stages are right, and *only then* look at what today spends beyond that minimum. Measured numbers are a bound on the incumbent, never the frame for the target.

How to apply:
- Before drafting an order, state the function; the cost analysis comes after. A plan whose phases are named after today's expensive components (`make X cheaper`, `speed up Y`) has reproduced the fallacy.
- Testable form: a reader should be able to finish the design half of a plan and not know what the current system costs.
- Deletion/LOC is a byproduct, never a goal or a bar. The real target is the user-visible outcome. Related: the failure mode is usually not "old code still on disk" but "old code still executing" — tombstone (stop executing, keep the code, ledger row with review-by) before deleting.
- The comaintainer seat's job is to hold the forest — objectives, orders, ledger, blast radii — and descend into files only to verify, never to implement. When the seat starts reading code to decide, it has already lost the altitude the operator is relying on it for.

RECURRED 2026-08-20, and the recurrence matters more than the original. The
seat ran an entire campaign rung (noun-convergence nc-2b) by hand — 44 call-site
rewrites, 72 impl blocks chased through the compiler, sed scripts — then started
a second rung the same way. Operator: *"This is all just implementation detail
of the campaign right? Not sure why the comaintainer is doing it."* The bullet
above was already written, already in the memory index, and still lost. So the
rule is not the problem; the tells are, because none of them look like a
decision at the time:

- You just received a worker's report. nc-5-corpus came back as a report the
  seat was overseeing. That IS the shape. Picking up the next rung by typing it
  is the drift, and it happens one tool call at a time.
- "Keep going" / "keep working autonomously" is not "keep implementing." For
  the seat it means keep the campaign moving: orders drafted, approvals sought,
  workers spawned, verdicts written.
- Delegation is standing-authorized here (`.claude/CLAUDE.md` §Delegation,
  cap 3 concurrent). So NOT delegating is an active choice that has to be
  justified, never a default that needs permission.
- Measurement is seat work; the edit is not. Pricing an order before
  approving it — "the order says publish `Atom`, but the verb already exists in
  the wrong crate, 44 types / 176 refs" — is exactly intake and is the seat's
  product. The `git mv` that follows is the worker's.

Structural check before touching any file: is there an order for this, is it
`approved:` (not `pending`), and am I the one who should be typing? Two noes and
a yes means draft the order and spawn the worker.

The cost is not just role purity: a seat in the trees is a seat that cannot see
the campaign, and it burns the context the operator needs it to keep.

Related: [[feedback-journey-first-design]], [[feedback-net-simplification-plans]], [[feedback-ground-in-reuse-before-building]], [[feedback-quantified-pitches]]
