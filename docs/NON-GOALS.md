# What this isn't trying to be

Every project is shaped as much by what it turns down as by what it takes on.
This page is the honest list of things Commonwealth AI is *not* trying to do —
not because they're bad ideas, but because they'd pull against the one thing
this project is for: an assistant that runs on your own machine, answers from
sources you chose, and never phones home.

If you're about to ask for something, it's worth a skim. If your idea lands on
one of these lines, that's not a no forever — but it is a no here, and knowing
that up front saves us both a long thread. Where a line has some give, it says
so.

## It won't quietly send your data anywhere

No telemetry, no "anonymous usage stats," no crash reporting that ships off your
machine without you starting it. Nothing was built to phone home, and nothing
will be. If a feature can only work by sending your conversations, documents, or
usage somewhere, the answer is no — even if it'd make the product better on
paper. Web search and mesh sharing exist, but they're off until you turn them
on and labelled plainly when they run. That's the whole point of the thing.

## It isn't a hosted service

There's no cloud you log into, no account, no server we run on your behalf. The
model answering you lives on the machine in front of you (or the few you've
pooled). We're not building a SaaS, a hosted API you rent, or a "free tier that
becomes a subscription." If you want something managed for you, this is the
wrong tool, and that's fine.

## It isn't chasing the leaderboard

The goal is an assistant that's honest, grounded, and traceable on hardware you
already own — not the top score on a benchmark. Features that trade away
groundedness or the "every claim traces to a source" promise for a flashier
number aren't the direction, even when the number is real.

## It isn't a general plugin marketplace

Workflows and connected tools are real and encouraged — but there's no ambition
to be an everything-platform with an app store, a plugin economy, or a
third-party extension ecosystem to police. The surface stays small enough for a
small team to keep honest.

## It won't grow a config knob for every preference

Every setting is a thing that can break, a thing to document, and a thing to
keep working forever. Sensible defaults that suit most people beat a wall of
toggles. We'll add a setting when enough real use shows the default genuinely
doesn't fit — not preemptively for a case of one.

## Where the lines have give

Some of the above bends with a good enough reason:

- **A new data source or corpus** that keeps things local and cited — usually a
  yes.
- **A platform or hardware target** we don't cover yet — often a yes, if someone
  can help test it.
- **A setting** that a lot of people independently reach for — the default was
  probably wrong; let's talk.

The test is always the same: does it hold the line that your data stays yours,
your machine does the work, and every answer traces back to something real? If
yes, bring it. If it only works by breaking one of those, it's not this project
— and there's no hard feelings in hearing that.
