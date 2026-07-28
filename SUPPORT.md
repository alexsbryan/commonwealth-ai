# Getting help

Start here, in this order — it's roughly fastest-answer-first.

## Something isn't working

1. **`svrn doctor`.** It checks most of the common failures itself — daemon
   down, models missing, index unbuilt, watcher dead — and each row prints the
   command that fixes it. `svrn doctor --fix` repairs what it safely can.
2. **[TROUBLESHOOTING](./sovereign/docs/TROUBLESHOOTING.md)** for symptom-to-fix
   pairs, and **[HAVING_TROUBLE](./sovereign/docs/HAVING_TROUBLE.md)** if you'd
   rather read prose than a table.
3. **[FAQ](./sovereign/docs/FAQ.md)** for the questions people ask first.
4. Still stuck: **open a bug report.** The template asks for your version,
   platform, and `svrn doctor` output up front, because those three answer most
   of the follow-up questions before they're asked.

## A question, not a bug

Use **[Discussions](https://github.com/alexsbryan/commonwealth-ai/discussions)**.
How-to questions, "is this supposed to work like that", hardware advice, and
what-are-you-building all belong there. Blank issues are turned off deliberately
— an issue is for something actionable, and a question that turns out to be a
bug is easy to promote.

## Connecting another tool to it

[INTEROP.md](./docs/INTEROP.md) has copy-paste recipes for pointing an existing
agent harness, editor, or chat UI at a local daemon, plus an honest list of what
isn't supported. If your tool isn't covered and you get it working, that's a
welcome pull request.

## Something you want to build or change

[CONTRIBUTING.md](./CONTRIBUTING.md) states exactly what can be merged today.
Recipes, documentation, and interop configs are open; core architecture isn't
yet.

## Security or privacy

**Do not open a public issue.** That includes any way data could leave a machine
unexpectedly. See [SECURITY.md](./SECURITY.md) — there's a private advisory form
and an email address.

## What to expect

One steward, so response times vary with the week. Security reports get an
acknowledgement first, usually within a few days. Bug reports with a clear
reproduction get looked at soonest, because they're the ones that can be acted
on without a round trip.
