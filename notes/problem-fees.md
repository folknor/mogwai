# PROBLEM: trading is free here, so every claim made against this venue is optimistic

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry. Surfaced in
review of the other problem statements: the set claimed to be sufficient for the
end state and did not mention fees anywhere, and the end state is hundreds of
agents producing claims about whether strategies are profitable.

## What the user wants

Hundreds of agents forward-testing dealt strategies and reporting whether they
work. The user agreed on review that the set is not sufficient for that end
state without fees, alongside the venue's half of claim provenance.

## The observation

The engine books `commission: Decimal::ZERO` on every fill, unconditionally.
There is no fee policy, no fee schedule, no maker/taker distinction, and no
divergence path that can perturb one. This is recorded in the repository's own
hardcoded-value inventory with the note that it is "notable for a crate whose
stated purpose is injecting realistic execution divergences", and it appeared in
none of the seven problem statements until review.

A venue where trading is free flatters every strategy tested against it, and it
does so SYSTEMATICALLY rather than randomly - the bias always runs the same
direction, and it scales with turnover, so it hurts exactly the high-frequency
strategies whose edge is thinnest. At fleet scale that is a bias applied
uniformly to every claim the pipeline produces.

There is a second cost with the same character. Once resting orders are
consumed by arriving flow (`notes/problem-order-book.md`), a spread exists, and
crossing it is a real cost a strategy pays. Today fills print at the order's own
price, so there is no spread to cross and no adverse selection to suffer.

## Why this may matter more than the fidelity work

The cadence document argues the tape is orders of magnitude too slow, which
distorts how often a strategy trades. Fees distort what each of those trades is
worth, and the two compound: a strategy that looks profitable on a slow free
tape may be unprofitable on a fast charged one, and the fleet cannot currently
tell the difference. Of the two, the fee gap is far cheaper to close and is
arguably the larger correction to a claim.

## What must be decided

1. **Whether fees are modelled at all**, or whether the venue declares itself
   fee-free and pushes cost modelling onto the consumer. That is a legitimate
   answer if stated - nautilus can apply its own commission model - but it must
   be a decision rather than an omission.
2. **The fee model's shape.** Flat rate, maker/taker asymmetry, tiered by
   volume, per-instrument. Maker/taker matters more than it looks here: a
   resting order that gets consumed is a MAKER fill and a marketable order is a
   TAKER fill, so the distinction only becomes expressible once orders rest.
3. **Where it lives.** A per-instrument profile field, a venue-wide config knob,
   or part of the instrument definition on the wire. The consumer needs to know
   the schedule to reason about it, which argues for the wire.
4. **Which currency and how it books.** The ledger is per-currency with
   reservations derived from resting orders; a commission is a fourth kind of
   balance movement alongside the fill deltas and the seed funding.
5. **Whether fees are a havoc surface.** Every other venue behaviour here can be
   perturbed. A fee schedule that changes mid-run, or a venue that charges more
   than it advertised, is a real pathology a consumer should survive - and it is
   exactly the class of divergence this project exists to inject.
6. **Whether the spread cost is modelled separately** or falls out of the book
   work. If fills stop printing at the order's own price, part of this is
   answered by the book document rather than here.
7. **What the defaults are grounded in.** Binance publishes its spot schedule;
   CME publishes contract fees. Neither is in the corpus, so this is a declared
   value with a citable source rather than a fitted one - which is fine if
   labelled, per the provenance discipline the profile document sets out.

## What this document does not decide

The book (`notes/problem-order-book.md`), though the spread half of trading cost
depends on it. Nor the instrument model, though a futures contract's fees are
per-contract rather than a percentage of notional, so the two interact.

## Known cost, explicitly not a decision input

Per the user's standing instruction, resource cost does not shape this. A
commission field on a fill is trivial; what is not trivial is that every
existing test asserting balances after a fill would need to account for it, and
the golden fill distribution and the smoke's balance assertions both pin
post-fill state.
