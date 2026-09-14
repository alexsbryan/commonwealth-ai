# Every system we build must ship with its closure loop, not just its creation surface — unclosed systems degrade within days

Operator, 2026-08-12: "This is one of the major phenomena we have to fight in
our agent-based work: creation and addition is sooooo easy, but we often let
those systems we make degrade within days. We have to make sure each system
has a complete loop of creation and closure."

Why: agents add cheaply and never subtract. A store, registry, ledger or
heap ships with a rich CREATE path and no CLOSE path, so it accumulates
entries that were true when written and are false a week later — while still
presenting as authoritative. Measured instance the same morning: the backlog's
top three vetted items were all already fixed (two by one commit six days
earlier, then migrated INTO the backlog three days after the fix). The ruler
ranked them pullable because VETTED means "well-formed", never "still
reproduces at HEAD". Seat finding 14e2bcb3.

The resilience half (operator, same day, verbatim): "Every system we build
has to be resilient to a messy process that doesn't always orchaestrate fully.
Sometimes we won't do a nightly sweep because the session went long and a soak
runs until 2am and the operator yields to that. Let's make sure this isn't as
precious as things like the pre-push hook which gets so ugly after one missed
run of it that we just want to skip it forever." A closure loop whose catch-up
cost GROWS with the time it was skipped will eventually be skipped forever, so:
prefer level-triggered questions ("does this still hold at HEAD?" — answerable
from current state, one run recovers any gap) over edge-triggered queues
("which events since the mark?" — a backlog that grows while you sleep); never
build a cursor-plus-cap (the nightly sweep is the live cautionary case: capped
six nights running, structurally unable to catch up); scale work to what is
about to be TRUSTED, not to elapsed time (verify at the point of use); and
never hard-block the operator on a missed run — show the age, proceed, let them
decide.

How to apply: when designing or reviewing any system that accumulates
entries, name its closure trigger before it ships — what event retires an
entry, what observes that event, and what the surface shows for an entry whose
liveness was never re-checked. Prefer closure as a byproduct of work already
happening (the nightly commit sweep can ask "does this commit close an open
item?") over a separate discipline someone must remember — that is principle
10, structural not remembered. Where closure cannot be automated, the surface
must show the age of the last verification rather than presenting stale
entries as fresh, the way `sovereign posture` does for every other subsystem.
This generalizes past the backlog: claims have TTL, directives have resolve,
orders have close, notes have retire/supersede — a new system with no
equivalent is the smell.

Related: [[feedback-net-simplification-plans]],
[[feedback-defaults-ledger-no-withering]],
[[feedback-ground-in-reuse-before-building]].
