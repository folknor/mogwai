# Bug hunt: mogwai-engine

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Round 1 landed findings 1 through 6, each with a regression test verified to
fail against the reverted production change. Finding 6 was closed by making
`held_for` and `locked_balances` derive from one `Engine::order_reservation`,
which consults the instrument class before the margin map, rather than by
resting on the server-config guard that rejects a margin table on a spot
instrument (that guard is real, but `Engine::set_margin_policy` is public and
bypasses it). Two further defects found while reviewing that work - a venue
liquidation paying and expiring a client-armed `FeeSurcharge`, and an amend
whose funded-account check omitted commission - were fixed in the same pass.
Round 2 landed finding 7 and every bullet of finding 9. Finding 7 forced a
`TAPE_PROTOCOL_VERSION` bump from 12 to 13, so the protocol-12b mechanism
landing now takes 14. Reviewing that pass turned up three more defects, fixed
with it: a zero-quantity sweep pulled its scan frontier forward to the pass
time and could retire a span a truncated drain budget never walked; the
`DropNextAccountUpdate` doc comment and `docs/havoc.md` both defined the arm as
fill-only when the engine had already been spending it on any order transition
that moved the ledger; and the version bump left five durable statements of the
old number and the old 13 reservation standing.

A close pass over the whole three-commit arc re-reviewed the halves no cold
review had seen and fixed two corner defects: a zero initial-margin policy
could panic the debug reservation reconciliation on key existence alone, and
a zero-quantity partial spent `DropNextAccountUpdate` on an order that merely
came to rest. Both are pinned by bite-checked tests.

There are no open findings.
