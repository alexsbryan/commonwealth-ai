# Having trouble?

This page is for people **using** svrnmesh. It doesn't ask you to open a
terminal, and nothing here can break your setup.

If you maintain the software, you want
[TROUBLESHOOTING.md](TROUBLESHOOTING.md) instead — it's the same
problems from the other side.

---

## Start here: the health check

Almost everything on this page is answered by one screen.

> **Settings → Diagnostics → Health check**

It runs seven checks and tells you, in plain language, what it found
and what to try. Most of the time you can fix it yourself from there
and never need to contact anyone. That's what it's for.

When something is wrong, the app will also offer you a **Check my
setup** button on the banner that appears — that's the same screen.

The checks, and what each one is really asking:

| Check | The question behind it |
|---|---|
| **Engine** | Is the part that does the thinking running at all? |
| **Model** | Is there a brain loaded for it to think with? |
| **Mesh** | Have you joined a group? |
| **Other people** | Can you see the others in it? |
| **Knowledge** | Did your documents and knowledge bases import cleanly? |
| **Disk space** | Is there room to work? |
| **Stability** | Has it crashed recently? |

Each check shows a short id (`engine`, `mesh_peers`, …). Those ids
never change wording, so if you mention one, whoever helps you knows
exactly which line you mean — including from a screenshot.

---

## Sending a report

Two kinds, depending on what went wrong.

**Something about the app is broken** — it's slow, it crashed, you
can't see anyone, an import stalled.

> Settings → Diagnostics → **Something's wrong — create a report**

**One specific answer was wrong** — it made something up, it said it
had no sources when you know it does, it answered from the wrong
document, it stopped mid-sentence.

> Underneath the answer, next to Copy and Export → **Report**

The second one is much more useful for that kind of problem, because
it captures how that particular answer was produced: which documents
were consulted, which machine answered, how long it took, and whether
the app's own checks flagged anything.

### What happens when you create a report

1. A file lands on your **Desktop**. Nothing is sent anywhere.
2. **Open it and read it.** It's plain text. Everything anyone would
   see is right there.
3. Send it to whoever set up your mesh — email, chat, however you
   normally talk to them.

The file itself begins by stating what it contains. Take that
seriously — it changes depending on which kind of report you made:

- A report from Settings contains your app version, your settings, and
  the health check. **Not** your documents, conversations, or answers.
- A report about one answer also contains **that** question and **that**
  answer — the one you chose — plus the *names* of the documents it
  used. Not their contents, and nothing from your other conversations.
- If you tick *"also include the text it read"*, it adds the passages
  themselves. That's a real choice, so it's off unless you tick it.
  Tick it when the problem is "it quoted the wrong thing" and you're
  comfortable sharing those passages.

### The reference code

A report about an answer gets a short code — something like
**`2AM-QSC`**. It's at the top of the file and in the filename.

Say it or type it when you describe the problem. It points at exactly
one answer, so nobody has to ask "which one?" — and it works even
before you've sent the file.

---

## Common situations

### "It says it can't find anything, but I know the document is there"

Check **Knowledge** in the health check first — an import that failed
or is still running looks exactly like this.

If Knowledge is fine, this is the case to use **Report** on the answer
itself. The report shows which knowledge bases were actually searched,
which is usually where the surprise is.

### "It's answering, but it's very slow"

Check **Model** and **Other people**.

A common cause on a mesh: the machine that normally answers has gone
offline, so your machine is doing all the work itself. **Other people**
will show that.

Report it from Settings → Diagnostics with *"It's too slow"*, and say
roughly how long you waited and what you asked.

### "The answer just stops in the middle of a sentence"

That's a length limit, not a crash. You can usually fix it yourself:
the answer will show a **Continue from here** button, and you can raise
the limit in **Settings → Models**.

If it happens constantly, report the answer — the report says outright
whether the reply hit the limit.

### "I can't see anybody else"

Check **Other people**. The usual causes, in order:

1. They aren't running the app right now.
2. You're on different networks — a VPN switching on does this, and so
   does moving to a different Wi-Fi than everyone else.
3. Guest or corporate Wi-Fi that blocks devices from finding each
   other.

The health check distinguishes "you haven't joined a mesh" from "you've
joined but nobody's visible", which are different problems with
different fixes.

### "It crashed" / "the window went blank"

Reopen it — the app restarts its engine by itself and usually comes
back within a few seconds.

If it happens twice, check **Stability** and send a report. Crash
reports already contain the technical detail needed; you don't have to
find or copy anything.

### "It won't start at all"

Quit it completely and reopen it once.

If it still won't start, you can't reach the health check from inside
the app — so tell whoever set it up what you see on screen, word for
word. That's enough to get started.

---

## What we will never ask you to do

Not because it's forbidden — because you shouldn't have to:

- Open a terminal or run a command.
- Find and copy a log file.
- Delete anything from a hidden folder.
- Reinstall to fix a problem.

If someone helping you asks for one of those, it means the app is
missing a way to answer the question. That's a bug in the app. Say so.
