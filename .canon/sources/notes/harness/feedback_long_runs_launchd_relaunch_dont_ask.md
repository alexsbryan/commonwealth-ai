# Long daemon-bound runs go straight to launchd one-shot; an out-of-band kill on an idle machine means relaunch immediately and notify, never…

Long runs (> ~25 min) with the local daemon on BeefyMac: launch as a
launchd one-shot FROM THE START — never as a harness background task
(the documented reaper kills tracked tasks mid-flight; shared-notes
512fd04e has two confirmed strikes, plus the 2026-08-10 arbitration
kill at 1 min). Monitor with short (<25 min) disposable waiters as
re-invocation timers.

**ARM A `Monitor` ON THE LOG IN THE SAME TURN YOU LOAD THE PLIST.** A
launchd job is invisible to the session by construction — no task
attaches, `/tasks` shows nothing, and the seat looks idle while an hour
of inference burns. Operator, 2026-08-25, unprompted and naming it a
pattern: "I don't see anything attached to the session (a chronic issue
we seem to be having when working with canon)." The launchd half of
this memory was being followed and the visibility half did not exist.

The watcher is a poll loop over the job's log, `persistent: true`,
emitting only on CHANGE plus terminal state — and it must also break
when the pinned binary disappears without the log reaching its done
marker, or a crash reads exactly like "still running". Canon logs
progress with `\r`, so `tr '\r' '\n'` before `tail -1` or the stage
line never emits.

If a seat-launched run is killed out-of-band and the machine is idle:
RELAUNCH IMMEDIATELY via launchd and tell the operator afterwards —
do not present options and wait. Operator verbatim (2026-08-11, after
8 idle hours): "It's running local inference. The machine is idle.
What's the risk you're avoiding?"

Why: bench runs are idempotent (overwrite their own outputs, daemon
read-only, artifacts stream to disk); the worst case of a wrong
relaunch is one bootout command. The worst case of asking is a lost
night.

How to apply: asking-before-relaunch is reserved for evidence the
operator is actively using the machine, or the run itself misbehaving
(crash-looping, corrupting outputs). Absent that, relaunch is the
default. Related: [[dogfood-mechanical-model-work]]; run-channel
taxonomy in .claude/skills/comaintainer/SKILL.md.
