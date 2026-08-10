# Your local journal: what we record, and how to read or stop it

Some features keep a short record of how they behaved on your machine —
not because we want your data, but because "is this thing actually any
good on real work?" is otherwise unanswerable, and the honest way to
answer it is to let you look at the evidence yourself and decide whether
to share it.

This page is the whole story: where it lives, what is in it, how to read
it, how to hand it back, and how to switch it off.

**The short version.** It is on your disk only. Nothing is ever sent
anywhere — there is no upload path in the code. It records *why* a
feature did what it did, never your code. One command reads it, one
command bundles it, one command stops it.

## Where it is

```
~/.svrnmesh/journal/<feature>-<date>.jsonl
```

One file per feature per day, plain JSON, one record per line. Fourteen
days of history, then the old days are deleted. Each day's file stops
growing at 8 MB, so a runaway feature cannot fill your disk.

One feature keeps a journal today:

| stream | what it records |
|---|---|
| `next-edit` | Whether the [next-edit lane](./NEXT_EDIT_IN_YOUR_EDITOR.md) fired or stayed silent and why, which model answered, how big the region was, how long it took — and what you did with the suggestion |

## Reading it

```bash
svrn journal
```

That is the whole read surface, and on a fresh machine it will honestly
tell you there is nothing yet. Once there is:

```bash
svrn journal                  # the summary for every feature
svrn journal next-edit        # just one
svrn journal show --last 50   # the raw records
```

The summary counts what happened *and* what it could not judge. For
next-edit, that means an acceptance rate computed only over suggestions
you actually accepted or dismissed, with the ones you typed past
(`diverged`), the ones a newer suggestion replaced (`superseded`), and
the ones that never resolved at all (`unknown`) reported separately
rather than quietly counted as rejections. If fewer than twenty were
judged, it says so instead of quoting you a percentage.

That is deliberate. A number that looks precise and is wrong is worse
than no number.

## What is NOT in it

Not your code. Specifically: not the document, not the region a model was
shown, not the file path, not the text a rule matched, not the rewrite
anything proposed. The record has no field those could travel through —
it is a fixed list of names, numbers and short labels the daemon chose
from its own vocabulary. `path_ext` is the file extension on its own
(`rs`, `tsx`, `go`), and `region_bytes` is a length.

You should not take that on faith:

```bash
svrn journal bundle
```

This writes one file and then prints **every field name in it**, read
back out of the bytes it just wrote. If a field you are not comfortable
with is in there, you will see its name in that list. Read the file — it
is small, and it is plain JSON.

`bundle` writes a file and stops. Where it goes after that is your
decision, made having read it. There is no send, submit, or upload
command, and adding one would be a design change, not a feature.

## Stopping it

```bash
svrn journal off              # stop recording, everything
svrn journal next-edit off    # stop one feature only
svrn journal on               # resume
svrn journal clear            # delete every record
```

`off` takes effect on the next record — no daemon restart, no config
edit. It leaves what is already recorded alone; `clear` is what deletes.
For scripting or CI there are env vars too: `SOVEREIGN_JOURNAL=off` for
everything, `SOVEREIGN_NEXT_EDIT_JOURNAL=off` for one feature.

## If you are handing evidence back

If you are trying one of these features for us, this is the loop we are
asking for:

1. Use it normally for a few days.
2. `svrn journal` — see for yourself whether the numbers match how it
   felt.
3. `svrn journal bundle` — read what it printed, read the file.
4. Send it, or don't. Both are useful answers; the second one is a
   finding about this page.

## For developers adding a journal

The machinery is feature-agnostic
(`sovereign/crates/sovereign-contracts/src/types/journal.rs`): declare a
`const JournalStream`, define serde types for your lines, and add a row
to `journal_cmd::VIEWS`. You inherit the file layout, rotation, the caps,
retention, and all four off-switches, and every command above starts
covering your stream.

Three rules are not yours to vary. No code in a line — enforce it in the
shape of your record type, not by remembering. No network path. And `off`
must mean off, which means going through `JournalStream::enabled` rather
than checking a flag yourself. Also add a row to the table above: a
stream nobody documented is a stream the user cannot consent to.
