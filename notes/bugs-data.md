# Bug hunt: mogwai-data

CLOSED. Round 1 landed findings 3, 5, 6 and 7 (and forced the
`TAPE_PROTOCOL_VERSION` bump to 14); round 2 landed finding 4, finding 8 and
the Smaller items. Finding 1 was closed from the server side and struck during
`bugs-server-tape.md` round 2; finding 2 closed with the `BoundedSeek` repair
in the same arc; finding 9 was deleted as describing a tree that no longer
exists. Finding 8's deletion proposal was REJECTED - `MergeSource`'s merge,
fault latching, index tie-break and inclusive one-tick seek buffer are real
contracts and its documentation was what was false. The `SweepShape::new`
bullet was dead as written and its proposed denominator cache measured no
improvement, so nothing was kept from it.

The per-round classifications, machinery, no-bump verdicts and bite checks are
in the git history of the retired `notes/bug-loop-carry-forward.md`; the
loop's durable lessons live in `AGENTS.md`. No open finding remains.
