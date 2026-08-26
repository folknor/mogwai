# North Star

What mogwai is when it is finished.

This is not a description of what the tree currently does. Like the glossary,
it states the end state: where this document and the code disagree, the code
is behind, and correcting this document to match the present is the one edit
that is always wrong here. The open work lives in `notes/`; the vocabulary
lives in `reference/glossary.md`; this page is the reason both exist. Only
the owner changes what it aims at.

## The world this serves

One human runs a handful of orchestrator agents. Each orchestrator deals a
batch of deterministic strategy-design slates and launches tens of subagents,
one slate each - on the order of two hundred agents running concurrently
across the batches. Each subagent authors one Pine strategy, establishes its
edge on real historical data through backtest, optimization, Monte Carlo and
walkforward, and then forward-tests it against mogwai.

Mogwai is the only forward-test venue that world has. The claim pipeline -
collecting results, adjudicating replications, deciding what deploys - is the
orchestrator's job, human plus Claude, deliberately not software. Nothing in
mogwai grows toward owning it.

## What a forward run proves

Execution robustness, never edge. The edge was established upstream on real
history; what a bar-close backtest structurally cannot see is what mogwai
exists to exercise: resting-order and conditional-order timing, partial
fills, rejects, delays, duplicate fills, dropped updates, blackouts -
survival of the messy live path. Dollars earned on a synthetic tape are a
statement about the fitted distribution of worlds, never about next month's
market, and anything mogwai reports keeps those two claims distinct.

The distribution is the point. A fitted generative tape supplies unlimited
counterfactual months drawn from the same measured process: one seed is one path, a claim wants many seeds,
and every run is reproducible and bindable. That is why fire-and-forget
instances, seed reproducibility and cheap tape identity outrank any single
run.

## An exchange first

Strip every havoc knob off and what remains is a venue. That is the product,
not a reduction: the havoc is an adversarial dial on top of an exchange, and
it can only sit on the parts of an exchange that exist. So the surface is
complete, not curated - every order type, time-in-force and instrument class
that goes with what an exchange lists, sized against no particular consumer's
catalog. A shape mogwai refuses is a strategy family with no forward test
anywhere.

Accounts are real accounts: client-named, policy-enforced, prop-firm
faithful. A risk rule is what it measures, on what basis, and what it does on
breach - and the venue enforces it, because a strategy that would have been
liquidated must actually be liquidated or the forward claim is worth nothing.

## Serve anything, gate nothing

A symbol is a request parameter, not an identity the venue owns. Any string
arrives and is served: a preset supplies the shape when the name matches one,
the default shape wears the label when none does, and resolution is total at
every layer - symbol, account, policy. A preset is nothing but a named bundle
of knobs a user could set by hand, carrying no authority.

The generator's end state is pure instrument-agnostic method. Everything is
per-instrument in principle, so onboarding instrument N is the intake
sequence terminating in a TOML file, zero method edits. Each knob is a
landing site where a measurement lands, and evidence is bought only when a
knob exists to receive it. Each carries fitted, derived or declared
provenance, so a preset is a claim ledger: which parts of its tape rest on
measurement, and which on assertion. The intake sequence makes a river
better; it never decides whether a symbol may be served.

## Rivers, passengers, and two modes

The water is exogenous. Order flow never feeds back into the tape, which is
what gives passengers non-interference by construction: no queue competition,
no market impact, fifty agents submitting the same buy at the same instant
all get the same fill. Modelling impact would let passengers reach each
other through the water and nothing else in the design would save it.
Passengers are also owed
invisibility - no account can observe that another exists.

The venue runs in the two modes the glossary names. Server mode is the
day-to-day shape: one exchange per orchestrator batch, tens of accounts
connecting with their own ledgers and asking for whatever water their
strategies need, tape generation amortized across the batch. Transient mode
is the fallback and the dev path: a consumer given no address spawns its own
ephemeral venue and the kernel reaps it. The semantics are identical in both;
only the number of processes differs.

## Tapes across session classes

Session structure is the one thing bars do not normalize away, so a
session-bound thesis forward-tested against the wrong session class tests a
different claim. The preset set spans three classes, on the order of five
presets: 24/7 crypto, CME futures with genuine closure, cash-equity hours.
Tapes are composable session footprints - an endless Asia, a looping week -
built by resampling real segments, and the standing gate on any tape change
is a rendered chart under the owner's eye, the cheapest gate available and
the only one that catches what statistics cannot see.


## The settled premises

Forward tests always run accelerated, never at real time. A run is fire and
forget: no restart, no resume - reproducing a path means a fresh instance
with the same seed and config, or the same named window on a shared
exchange. Strategies are single-instrument, so independent per-symbol tapes
carrying no cross-instrument correlation are correct. There is one venue
across asset classes and across a batch's agents. Warmup is declared config.
Resource cost shapes no decision; it may motivate a mode, never bend the
model.

## Who decides

The owner, on every product and architecture question. There is one user;
the operator is an agent acting for them; consumers are consumers, and where
a consumer's preference conflicts with what the venue should be, the
preference loses. Every demand for measurement names the decision the result
would change. The budget that matters is minutes of owner attention at real
forks.

The vocabulary is decided the same way and more narrowly. Only the owner
admits a glossary entry, so an agent meeting a load-bearing word with no
definition escalates and never writes one - a definition invented to unblock
a task becomes the target the code is then built toward, which is the whole
failure the glossary exists to prevent. Vocabularies that want writing down
and are not that word belong in `reference/`.

## What this is not

Not a claim pipeline. Not a market-impact model. Not an authentication
system - an account id is a bearer token on a loopback venue, written down
as exactly that. Not an options venue until someone who understands options
argues it in. And never a venue that refuses a symbol, an order type or an
account shape because nobody has needed it yet.
