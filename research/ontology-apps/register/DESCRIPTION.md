# Rule ledgers for five software projects

I keep the rules that govern how five software projects are built: canon, zoracite, ersilia, eutropia and
commonwealth-ai. Each rule has an id like `can-0123456789ab`. The material is the commit messages that recorded
each change to a project's rule ledger: a rule proposed, approved, superseded by a newer rule, retracted or
reverted, adopted from another project. It comes as JSON lines, one message per line, with the fields `id`,
`thread` (the project), `created_at`, `author`, `title` and `body` (the message).

What I need to be able to ask:
1. Every rule the messages mention, by its id, with its project and what it says.
2. Each rule's standing now: in force, proposed and awaiting approval, superseded, or retracted.
3. When each rule was proposed, and when and by whom it was approved.
4. Which rule superseded a superseded rule.
5. When a retracted rule was retracted, and why.
