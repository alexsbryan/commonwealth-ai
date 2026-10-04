# What Sovereign can do

The [README](../README.md) covers getting it running and talking to it. This goes a little deeper into what happens when you ask it something — how it decides how much work a question needs, the tools it can reach for, what it remembers, and how it shows where an answer came from. None of this needs configuring; it's written down so you know what's going on underneath a given reply.

## Choosing a model

Most questions don't need a large model. A small, fast one answers the simple ones in about a second, and anything harder goes to a larger model that loads when it's needed and unloads again after a minute of sitting idle. You don't pick between them — the request decides. Setup installs both, plus a small third model used for search.

## Multi-step work

For a request that takes more than one step — research that needs several searches, or a task with a few moving parts — Sovereign breaks the work down, does the independent parts at the same time, and puts the result together. If a tool call fails for a temporary reason it waits and tries again, and if a step doesn't pan out it can change the plan rather than carry on from a bad assumption. On the harder steps it can spend more effort: check its own output and have another go, or try more than one approach and keep the best.

## Tools

It can search the knowledge bases you've installed and, if you've set it up, the web; fetch and read a web page; run a shell command, which it asks you to approve first; and read a local document you point it at.

## Memory

Within a conversation it keeps track of what's been said without letting the context run out. When a conversation ends it holds onto what mattered and brings it back later when something relevant comes up, so you're not reintroducing yourself each time. All of it stays on your machine, and a skill can set how long its memories last or when to let them go.

## Where answers come from

Every reply carries a record of how it was made: how the request was read, which knowledge bases were searched and how many passages matched, which model wrote the answer, and how long it took. In the desktop app that's a strip under the message you can open. It's there so that when an answer is thin you can see why — a search that turned up nothing in a corpus tells you more than a confident-sounding guess would.

## Skills

A skill is a plain text file that shapes how Sovereign works for a kind of task: how it plans, what it searches first, how it writes, what it keeps in memory. A few come built in — research and analysis with citations, code review, a personal assistant, and a reflective one called inner work. You can change any of them or write your own, since it's a file rather than code. Some are marked local-only, which means their data never leaves your machine even if you've set up a remote model; inner work is one of those. When more than one model is available, a skill can also state what it needs from one, and Sovereign routes to the closest fit.

Writing a skill is covered in [DEVELOPMENT.md](DEVELOPMENT.md#adding-a-skill).
