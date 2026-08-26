# Start here

A map of this folder, nothing more. It exists because the alternative keeps
happening: an agent reconstructs the shape of the work from `todo.md` plus
whichever notes it happens to open, gets it three-quarters right, and writes the
quarter that is wrong into three more documents.

**Keep it short.** The previous version of this file explained instead of
pointing, and by 2026-08-23 it described two dead tracks as live, omitted the two
most valuable documents in the folder entirely, and asserted a death condition as
pending that had already been met. If this file starts explaining things, it has
failed. If its file list stops matching `ls`, it is worse than absent.

Notes-class, like everything here: transient, no truth guarantee, nothing durable
cites it. Its own end state is deletion.

## The arc

`reference/north-star.md` is the durable statement of the end state and
everything here is a phase of it. One sentence, because the rest makes no sense
without it: make mogwai generate a realistic tape for whatever instrument gets
traded next, and serve it over the live path so a strategy can be forward-tested
against it.

The instrument set today - MNQ, MES, BTCUSDT - is where the work has reached, not
where it stops. Assuming the corpus is closed has made three separate assessments
wrong.

## Where the work stands, 2026-08-23

The measure-and-fit approach to tape generation was closed by the owner on
2026-08-23 after producing, over a hundred-odd commits, a tape indistinguishable
from the one it started with. Its documents were deleted the same day. What it
produced that survives is `tape-research-v1.md`; the data-format facts it bought
were moved to `reference/corpus-formats.md`, which is durable.

Tape research v2 has no premise yet. That is deliberate: the premise gets decided
after the cleanup, not before.

| Document | What it is |
|---|---|
| `todo.md` | Open work only, across the whole project. The catch-all beneath everything else. |
| `tape-research-v1.md` | What the closed measure-and-fit arc produced: outcomes, surviving findings about markets and about this generator, and why the loop produced nothing. The input to whatever v2 becomes. |
| `segment-sampler.md` | The session-segment sampler: the tape as composable real segments judged by the owner's eye. Standing direction since 2026-08-12, and the strongest candidate seed for v2. Its slice-1 gate failed 2026-08-18 and is still failing. |
| `testnet.md` | Misnamed: it is the product-type plan. Nautilus account-model constraints forcing a venue split by account type, the venue as sole money authority, multi-account topology, instrument declaration. Venue-side, untouched by any tape question. |

## Traps this repo has actually fallen into

Cheap to state, expensive to rediscover. The test-doctrine and format versions of
these live durably in `reference/test-doctrine.md` and
`reference/corpus-formats.md`; what stays here is the documentation failure mode.

- **Assuming the corpus is closed.** It made three separate script assessments
  wrong. Every new instrument re-runs the whole intake sequence.
- **Trusting an inventory instead of the tree.** Point-in-time lists drift, this
  file included. Verify against the tree before acting on a list.
- **Citing a frozen artifact by vibe.** `targets-frozen.json` was described as
  the retired Python window-selection implementation's gate in three documents;
  it was the BTCUSDT microstructure target set and that script never touched it.
- **Substituting a noun phrase for a filename with a script.** The 2026-08-26
  citation sweep replaced nine retired script names with descriptions by plain
  substring replacement, and corrupted a dozen durable sites: a name rewritten
  inside a longer path, a phrase carrying its own article dropped into a
  sentence that already had one, a filename that opened a sentence or wore a
  line-number suffix left ungrammatical. One landed in a preset's provenance
  table, which is a claim ledger. Every test stayed green, because nothing
  lints prose. `scripts/retire_note_citations.py` now gates the four mechanical
  shapes and refuses to write without `--write`, but the gate for the rest is a
  human reading the lines - which is the actual lesson.
- **Reading a summary and calling it the source.** The purchase report's summary
  of the sampling-frame verdict was wrong for days, and the deleted
  preregistration was wider than any summary of it carried. Read the deciding
  document, not the document that cites it.

## The end state of these documents

All of `notes/` resolves to deletion. The test for anything written here is
whether it dies cleanly. A note that has to survive belongs in `reference/` or in
a code comment, because a code comment outlives the note. This file dies with the
rest.
