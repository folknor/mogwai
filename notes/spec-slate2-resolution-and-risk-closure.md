# Implementation spec: risk enforcement closure and instrument resolution

Priority slate 2. Written against
`reference/technical-implementation-spec.md`, from the two work items it
completes: the instrument-definition entry under Venue and protocol and the
unvaluable-policed-account entry under Engine in `notes/todo.md`, both ruled
2026-08-26. `reference/north-star.md` binds the direction (a strategy that
would have been liquidated must actually be liquidated; serve anything, gate
nothing) and `reference/glossary.md` the vocabulary (boarding, passenger,
river, policed). This document is transient and is deleted in the commit that
lands its last brick.

## What this closes

Two defects with one shared shape. First: a policed account that becomes
unvaluable in its policy currency keeps trading with its rules switched off,
one sweep pass at a time, with a warn as the only trace
(`crates/mogwai-venue/src/sweeper.rs`, `enforce_policy`, the "cannot value
this account in its policy currency" arm). Second: the default instrument
shape every unmatched symbol resolves through is BTCUSDT spot settling in
USDT, coupled to a USDT-funded default account, where the ruling moves the
default to standard USD cash equity.

The owner ruling on the first, restated because it is the design: the venue
never invents a price. One-hop valuation is the venue declining to hold a
view it has not earned; a rate surface is a mechanism for fabricating prices
that would flow into liquidation decisions. So the fix is not to value the
unvaluable but to make the unvaluable state unreachable - and since the venue
cannot enumerate at process start what symbols it will be asked to serve,
**boarding is the boot moment**: at connect the venue knows the account, its
policy currency and the one symbol this passenger will trade, so the check is
decidable deterministically there, and a refused boarding is boot-time death
from the user's chair without taking down a venue serving fifty accounts.

## Survey of the ground

All paths below are current as of this writing; none are cited by line
number except where a symbol is not enough.

**Resolution.** The three-layer merge already exists in
`crates/mogwai-venue/src/config.rs`: a base bundle chosen by the three-step
precedence in `bundle_name` (operator `preset` key, then a preset whose name
matches the symbol, then `DEFAULT_PRESET`), then the `[instrument]` overlay,
then the matching `[symbols.*]` overlay, applied in `resolve_instrument_named`
/ `apply_overlay`. `configured_from_table` guards unknown keys
(`deny_unknown_fields` plus `refuse_unknown_subtable_keys` down to the
arrival-family level), and `profile_from_configured` runs the def validation,
the class/options cross-checks (`validate_instrument_options`), the scalar and
size-grid validations, and the session/calendar validations. Boot
(`build_instrument_profiles`) sweeps the boot shape, every configured symbol,
and the whole reachable preset set, refusing invalid configured shapes and
recording funding-barred settlement currencies. Consumer-driven resolution
(`InstrumentProfiles::resolve`) refuses with `ResolveRefusal`
(`IllegalSymbol`, `Invalid`, `FundingBarred`), surfaced as a 400 in the `/ws`
handler before `claim_account` - the pre-claim refusal ladder in
`crates/mogwai-venue/src/ws.rs` is deliberate and documented there: every
refusal is decided before anything is taken from an incumbent.

**The undefined-shape refusal door therefore already exists.** What the todo
entry calls "undefined-shape" is `ResolveRefusal::Invalid` at that door. The
buildable work on the resolution side is an audit that no unreconcilable
merge survives silently, not a new mechanism.

**Risk enforcement.** `open_account` in `crates/mogwai-venue/src/run.rs`
already refuses a policed account that opens holding anything other than its
policy currency (`AccountRefusal::ForeignOpeningBalance`), and order entry
already refuses, by name, an order on a shape that does not settle in the
policy currency: the guard in `crates/mogwai-venue/src/http.rs` (the
`settles_only_in` predicate, `def.class.settlement_currency() == currency`,
applied per submitted order against the resolved profile). Its comment states
the valuation model exactly: a future settling in the policy currency
qualifies, and so does a spot pair quoted in it, because the pair itself
prices the base one hop into the policy currency; what is refused is a shape
that would leave a holding nothing prices. Boarding, by contrast, checks
funding presence (`Run::funded_in`, asked of the prospective ledger with the
`resetting` claim semantics) but never the policy currency.

**How much of that the funding door already covers, established against the
code because the two reviews disagreed on it.** For a freshly opened policed
account the funding door shadows the proposed policy door completely, and the
spec's original example was wrong. `AccountPolicy::validate` refuses a policed
policy whose `opening_balances` leave the policy currency, and
`Run::open_account` refuses the same thing on the request's own `balances`
(`ForeignOpeningBalance`, filtered on `!is_unpoliced()`); `open_account` builds
the engine from those balances alone and never merges the venue's opening
terms. So a just-opened policed account's balance lines are a subset of
`{policy currency}`, `funded_in(settlement)` can only be true when `settlement`
is the policy currency, and that is exactly the predicate the new door would
test. A policed-USD account binding BTCUSDT is already refused today - as
unfunded, not as mispoliced. The originally proposed principal test could not
have bitten.

**The door is still reachable, by a two-step the funding door cannot see.**
Balance lines are not fixed at opening. `Engine`'s spot fill path
(`crates/mogwai-engine/src/account.rs`, the `Spot` arm) credits the base asset
as a balance line via `balances.entry(base).or_default()`, and
`is_funded_in` is `contains_key`. So: a policed-USD account opens in USD alone,
boards a USD-quoted spot pair (admitted by the funding door, and deliberately
admitted by the order-entry predicate under the one-hop argument), buys, and
now carries a base-asset balance line - `BTC`, say - that no policy rule can
value. A second boarding of any shape settling in that base asset (a
`BTC`-settled inverse, or a spot pair quoted in it) then passes `funded_in` and
fails `reaches_policy_currency`. That is the reachable unvaluable state, it is
reached entirely through public doors, and it is what Brick 2's door shuts.
Equity is not a route: an equity fill credits a position, never a balance
(`InstrumentClass::base_currency` answers `Some` for `Spot` alone), which is
why the class distinction that document already draws matters here.

**A stale doc found by this survey.** The `AccountPolicy::currency` field doc
in `crates/mogwai-protocol/src/risk.rs` still claims a policed account "today
means futures" and that `Engine::mark` refreshes only futures positions.
Both halves are superseded: `Engine::mark_over` records `last_marks` for
every class precisely so a spot base can be valued, and the http.rs guard
deliberately admits spot quoted in the policy currency. The paragraph is
corrected in Brick 2, which touches that contract.

**The sweeper warn.** `enforce_policy` declines to enforce when `equity_in`
answers `None`, warns, and moves on - per pass. With the open door and the
boarding door both shut, and order entry already shut, no path remains by
which a policed account *binds* a shape its policy cannot value:
venue-originated liquidation fills land on bound instruments, which the
boarding door has confirmed settle in the policy currency. The warn becomes a
backstop, not policy.

**But not an unreachable one, and the spec must not claim it is.** Two
residues survive the three doors, both worth naming because Brick 2 rewords
the warn's comment and an untrue guarantee in a comment is itself a defect:

- *Shape admitted, price absent.* `equity_in` needs every balance line priced,
  and `last_marks` is populated only on the mark path
  (`Engine::mark_over`); `apply_fill` never writes it. A USD-policed account
  that boards a USD-quoted spot, fills, and is swept before the first mark
  pass covering the base asset is unvaluable for that window. This is a
  timing residue, not a hole in a door.
- *The base-asset line itself.* Even with the boarding door shut, the spot
  base line above is a real balance in a currency the policy cannot value; the
  boarding door stops the account binding a *second* shape against it, but the
  line exists and `equity_in` must price it through the one hop. That is the
  one-hop model working as designed, not a defect - but it is why the warn is
  a live backstop rather than dead code.

**The default shape.** `DEFAULT_PRESET` is `"BTCUSDT"` in `config.rs`, with
the coupling documented at its declaration: the default preset's settlement
currency and the default account funding are a joint decision, because if
they disagree the wholly-unnamed request fails its own funding check.
`default_balances()` funds 1,000,000 USDT, mirrored by the committed
`mogwai.toml`. The BTCUSDT preset (`crates/mogwai-venue/presets/btcusdt.toml`)
carries no generator or session table: an absent generator defaults from the
committed fingerprint medians with `modal_tick`/`price_decimals` forced to
the def's grid and uncalibrated top sizes, which is exactly the mechanism the
new equity default preset rides too. The shipped default account policy is
unpoliced (`Run::minted_policy`); every shipped named policy is stated in
USD.

## Target

Three bricks, landed in this order, suite green at every boundary. Each is
one coherent keep-or-revert unit.

### Brick 1: the resolution loud-failure audit

The claim to prove: every merge whose result cannot reconcile dies loudly -
at boot for a configured shape, as a named 400 at bind for a consumer-driven
label, at launch for the launcher's symbol - and no failure mode falls
through to a run serving a shape the operator did not state.

The refusal inventory to audit, each pinned by a test where one does not
already exist (several do; the audit names which and lays the rest):

- unknown top-level key in a resolved table (`deny_unknown_fields` on
  `ConfiguredInstrument`);
- unknown key inside `generator`/`session` and inside the five generator
  seams and the arrival family (`refuse_unknown_subtable_keys`);
- an override path the bundle does not set (`replace_dotted`), and the
  bundle-addition path in `apply_overlay`: confirm a nonsense top-level
  addition is refused downstream by the deserialize rather than surviving as
  an ignored table - this is the one site the survey could not clear by
  reading, and the audit's primary suspect;
- a preset restating an inherited key, an inheritance cycle, a missing
  instrument or provenance table, incomplete provenance;
- class contradictions (`validate_instrument_options`: margin over spot,
  missing margin on forex/future, initial below maintenance);
- generator/def grid disagreements and scalar validation
  (`profile_from_configured`);
- session and calendar validation, including the calendar-conditional path;
- the same failure surfacing on all three doors: boot
  (`build_instrument_profiles` context chain), bind
  (`ResolveRefusal::Invalid` as a 400 whose body names the cause), launcher
  (`serve_async`'s early resolve).

**The `apply_overlay` suspect is resolved before the brick is scheduled, not
during it.** The bundle-addition bullet above is the one item the survey could
not clear by reading, and its answer decides the brick's shape: if a nonsense
top-level addition is caught by the downstream deserialize, Brick 1 is tests
only; if it survives as an ignored table, Brick 1 contains a production change
and owes it a bite-checked regression test. That is a fork in what gets built,
so it is settled first and the answer written back here. The rest of the
inventory is genuinely discovery - an audit that pre-enumerates its own findings
has already done the audit - and stays as listed.

Deliverable: tests only, plus fixes for anything the audit finds silent, in
the same brick. Any fix that is itself a behavior change names its own
regression test, bite-checked per `reference/test-doctrine.md`.

Gate: `brokkr check --gate`. Bite-check each new refusal test by reverting
the refusal (text edit), observing the named failure, restoring it.

### Brick 2: the boarding valuation-reachability refusal

**The predicate has one spelling.** Promote the order-entry predicate so both
doors read it: move `settles_only_in` out of `http.rs` into
`crates/mogwai-venue/src/risk.rs` as

```rust
pub(crate) fn reaches_policy_currency(def: &InstrumentDef, currency: &str) -> bool
```

with the http.rs guard calling it. Its doc carries the one-hop argument from
the http.rs comment (spot quoted in the policy currency qualifies; the pair
itself is the hop), so the two doors cannot drift.

The name is wider than the body, which is `settlement_currency() == currency`
and one hop only. Its doc must say so in as many words - that "reaches" means
this hop and no other, and that a second hop is the rate surface the governing
ruling forecloses - or the name invites a future reader to add one under it.

**The prospective policy currency.** New accessor on `Run`, mirroring
`funded_in`'s claim semantics exactly:

```rust
pub(crate) fn policy_currency(&self, account_id: &AccountId, resetting: bool) -> Option<String>
```

- `resetting`, or no account opened yet: the currency of `minted_policy()`
  filtered on `!is_unpoliced()` - `None` today, and the accessor still asks
  rather than hardcoding `None`, for the same reason `daily_reset_minute`
  does: the day `minted_policy` grows teeth, this door follows it instead of
  being silently skipped.
- otherwise: `peek_account`, then the account's risk ledger `currency()`
  under the same lock discipline `enforce_policy` uses, **filtered on
  `!ledger.is_unpoliced()` exactly as the minted branch is filtered.**

  The original text here claimed `RiskLedger` construction guarantees a stored
  currency implies a policed policy. It does not, and the correction is
  load-bearing. `RiskLedger::new` stores the policy verbatim, `currency()` is a
  bare `self.policy.currency.as_deref()`, and `AccountPolicy::validate` only
  enforces the converse - policed implies a currency. A policy carrying
  `currency = "USD"` and no thresholds is unpoliced, valid, and reachable
  (`resolve_policy` returns an inline policy whenever `opening_balances` is
  non-empty, rules or not). Unfiltered, the new door would refuse such an
  account a bind while enforcing nothing against it, contradicting this
  brick's own "an unpoliced account binding anything boards" test.

**A pre-existing vacuous gate at the same reading, closed in this brick.**
`enforce_policy` in `crates/mogwai-venue/src/sweeper.rs` takes
`let Some(currency) = ledger.currency()` and comments the `else` arm
"Unpoliced" - a claim that expression does not check, so a currency-carrying
unpoliced policy falls through it, values the account and runs every rule
against `None`. It reaches no false breach today, which is exactly why it is
invisible; it is the vacuous-gate family verbatim. Add the `!is_unpoliced()`
filter there too, in the same change that adds it to `policy_currency`, so the
two readings cannot drift, and pin it with a test that a currency-carrying
unpoliced account is not valued.

**The door.** In the `/ws` handler, immediately after the `funded_in`
refusal and before anything is claimed:

```rust
if let Some(currency) = state.run.policy_currency(&account_id, resetting)
    && !crate::risk::reaches_policy_currency(&profile.def, &currency)
{
    return Err((StatusCode::BAD_REQUEST, format!(
        "account {account} is policed in {currency} and {symbol} settles in {settlement}; \
         a policed account may bind only shapes settling in its policy currency, because \
         the venue prices one hop and owns no exchange rate - the symbol is still served, \
         it just cannot be bound by this account under this policy",
        ...
    )).into_response());
}
```

Placement inside the existing ladder keeps the invariant the handler
documents: refused before `claim_account`, so a malformed boarding cannot
evict an incumbent. Serve-anything is untouched - the refusal is about this
account under this policy, never about the symbol, and the text says so.

**The sweeper.** The warn in `enforce_policy` stays, reworded from "risk is
not enforced this pass" being the story to being the backstop: its comment
now names the three doors (open, boarding, order entry) that bound the state,
**and names the two residues from the survey that they do not close** - the
pre-first-mark timing window, and the spot base-asset line valued through the
one hop. A firing warn is then evidence of one of those two, or of a hole in a
door, rather than accepted degradation. The comment must not say the state is
unreachable, because it is not. No behavioral change there - enforcing against
a wrong number remains worse than declining.

**Tests**, in `mogwai-venue`'s ws/socket suites, each bite-checked. The list is
rebuilt around the reachability finding above: the naive single-step case is
not a test of this door at all, and is kept only as a pin on the door it
actually exercises.

- **The principal test, two-step.** A policed-in-USD account opened via
  `POST /accounts` with a USD policy and USD balances, boarding a USD-quoted
  spot pair declared by a `[symbols.*]` overlay (see the fixture note below),
  filling a buy so the base-asset balance line exists, then binding a second
  shape settling in that base asset. Refused with the named reason, before the
  upgrade, with the incumbent on that account untouched. This is the one that
  bites: revert the door and the second bind succeeds.
- **The shadowing pin.** The same policed-USD account binding BTCUSDT is
  refused *as unfunded*, naming the funding refusal's text and not the policy
  refusal's. This records that the funding door covers the single-step case, so
  a later reader does not re-derive the wrong example the review caught.
- the same account binding MNQ (USD-settled future) boards;
- the same account binding the USD-quoted spot label boards on the first bind
  (the spot admissibility half of the predicate, pinned so a later "futures
  only" simplification fails loudly);
- an unpoliced account binding anything boards, **including one whose policy
  carries a `currency` and no rules** - this is the case the unfiltered
  `policy_currency` would have refused, so it bites the filter directly;
- the resetting claim answers from the minted policy, not the doomed ledger.

**Fixture note, because no shipped preset supplies it.** BTCUSDT is the only
spot shape shipped and it quotes USDT, so the USD-quoted spot needed by two
tests above does not exist and must be constructed. The brick declares it as a
test-config `[symbols.*]` overlay - a spot class with `base` set to a
non-currency asset and `quote = "USD"` - plus a second overlay declaring a
shape settling in that base asset for the second bind. Naming these in the
spec rather than leaving them to the implementer is the difference between a
test that exists and one that is quietly dropped as unwritable.

**Durable prose in the same landing**: the "Funding, valuation and the
policy currency" section of `reference/architecture.md` gains the boarding
door beside the opening door (the two ways currency enters, both now shut,
the sweeper warn as backstop); the stale `AccountPolicy::currency` paragraph
in `mogwai-protocol` is corrected to the current engine (spot admissible,
marks recorded for every class).

Gate: `brokkr check --gate` (serving path). Example focused runs while
building: `brokkr test -p mogwai-venue policed_account_boarding`.

### Brick 3: the default moves to USD cash equity

One landing, because the coupling documented at `DEFAULT_PRESET` makes the
preset and the funding a joint decision.

**New shipped preset** `crates/mogwai-venue/presets/nvda.toml`:

```toml
[instrument]
symbol = "NVDA"
price_precision = 2
size_precision = 0
price_increment = "0.01"
size_increment = "1"
[instrument.class]
kind = "equity"
currency = "USD"
multiplier = "1"
# lot_size defaults to 1 (serde default `one_share`); borrowable absent (no
# modeled borrow constraint); settlement_ns 0 - see the deferral note below.
[provenance]
"symbol" = { kind = "declared", rationale = "the default shape's label" }
"price_precision" = { kind = "declared", rationale = "US equity quote grid, cents" }
"size_precision" = { kind = "declared", rationale = "shares are whole units" }
"price_increment" = { kind = "declared", rationale = "US equity quote grid, cents" }
"size_increment" = { kind = "declared", rationale = "one share" }
"class.kind" = { kind = "declared", rationale = "cash equity" }
"class.currency" = { kind = "declared", rationale = "US listing, USD settled" }
"class.multiplier" = { kind = "declared", rationale = "one share per contract" }
```

Two corrections the review earned, both of which would have stopped the brick
dead at boot:

- **`multiplier` is required.** `InstrumentClass::Equity::multiplier` carries
  no serde default - only `lot_size` does, through `one_share`. The earlier
  draft omitted the key and its comment claimed both default to one, which is
  half true. Without the line the preset does not deserialize.
- **The provenance table is exact, not a placeholder.** `validate_provenance`
  matches the declared key set against the leaf paths actually present and
  fails on *missing and extra alike*, so the table above is the artifact rather
  than a sketch of one - eight entries, one per key present, `class.multiplier`
  included precisely because the key is now present. A placeholder here is not
  a spec: two implementers could not produce the same file from it.

No `generator` and no `session` table: the scalars default from the
committed fingerprint medians exactly as BTCUSDT's do, with `modal_tick`
forced to 0.01 and uncalibrated top sizes. No `margin` table: the default is
a cash account; Reg-T leverage is an operator knob (`basis = "notional"`).
No `fees`.

Two decisions taken deliberately here rather than inherited, both flagged
for the owner and both taken with a recommendation per the spec contract:

- **Calendar: the equity default keeps 24/7 water.** The default shape
  serves every unmatched label - `SEKDKK`, `DOGEUSDT.P`, anything - and real
  cash-equity hours would shut those markets most of the day. The current
  default's deliberate no-calendar-claim survives the class change; an
  operator wanting NVDA's real hours writes `[symbols.NVDA.calendar]` or
  registers a preset. The owner may overrule; the change is one table.
- **Settlement: `settlement_ns = 0`.** Real T+1 would hold sale proceeds
  unsettled, which lands in `Balance.locked` - whose three-way conflation is
  the backlog item still owed its owner ruling. Taking T+1 now would grow
  that conflation before the ruling; the knob exists and flips in one line
  when it lands. Named as owed, not asserted as the end state.

The tape-fit inversion is stated and chosen: BTCUSDT remains the best-fitted
tape and stops being the default, which is fine under the standing ruling
that tape fidelity gates nothing.

**A third consequence, surfaced for the owner rather than buried as a doc
example.** After this brick a stock `mogwai serve` cannot bind BTCUSDT at all
without config: the default funding becomes USD, BTCUSDT settles USDT, and the
funding door refuses the bind. The best-fitted tape in the repository becomes
unreachable out of the box, and the fix for anyone who wants it is one
`[balances]` line. That is a real ergonomic cost of the ruling, it is the
direct mirror of the coupling documented at `DEFAULT_PRESET`, and it should be
an owner-visible consequence of the swap rather than something discovered from
a failing smoke run. `mogwai.toml`'s rewritten comment block is where it gets
said, since that file is where the reader is already looking when it bites.

**The swap**, all in the same change:

- `PRESETS` gains `("NVDA", include_str!("../presets/nvda.toml"))`. Both
  array lengths are written in the types and both widen to 4: `PRESETS` is a
  `[(&str, &str); 3]` and `preset_names()` returns `[&'static str; 3]`. The
  earlier draft named only the first;
- `DEFAULT_PRESET = "NVDA"`, its doc rewritten around the same coupling
  paragraph with USD in the USDT positions;
- `default_balances()` becomes `USD: 1_000_000`; the committed `mogwai.toml`
  `[balances]` moves to USD and its comment block re-derives which shipped
  shapes it funds (now NVDA, MNQ, MES and every unmatched symbol; BTCUSDT
  becomes the funding-barred example instead of the funded one);
- `TAPE_PROTOCOL_VERSION` next takes 28, and this brick spends it: the
  resolved generator scalars of every unmatched symbol move, so a same-seed
  no-config run generates different bytes. The prose gate follows - the
  next-takes line in `AGENTS.md` moves to 29 in the same commit, which
  `tape_version_prose.rs` enforces. **This document is inside that gate too.**
  The walk covers every markdown file in the repository, `notes/` included, and
  the bullet you are reading carries the live gated phrasing - so the Brick 3
  commit must delete this spec, or update this line to 29, in the same commit
  that bumps the constant. It cannot be left for a follow-up: the suite goes red
  the moment the constant moves. Deleting it is the intended path, since Brick 3
  is the last brick, but the deletion is now a gate obligation rather than
  tidiness;
- every test and golden pinned to the BTCUSDT default re-blesses knowingly:
  the `mogwai-cli` serving/lifecycle tests and `gen.rs` paths that name
  `DEFAULT_PRESET`, the config tests asserting the default shape, and
  `tests/golden/fill_distribution.json` if its blessing resolves through the
  default profile (establish, then re-bless, never widen);
- the four socket-backed adapter suites are checked for a baked-in
  BTCUSDT/USDT default assumption and updated where found.

**The blast radius, enumerated.** The list above was materially incomplete;
a sweep for `BTCUSDT` and `USDT` across docs, reference, scripts, fixtures and
the CLI suites turns up every file below, and each is visited in this brick -
either changed, or checked and recorded as default-independent. Being on this
list is not a claim that it changes, only that it is looked at.

- Durable prose: `docs/presets.md` (the most default-dependent document in the
  tree - several statements about the BTCUSDT default and the USDT-settled
  unmatched shape), `docs/config.md`, `docs/cli.md`, `docs/accounts.md`,
  `reference/architecture.md` (resolution section), `reference/performance.md`,
  `reference/corpus-formats.md`.
- Config and scripts: `mogwai.toml` comments, `scripts/smoke.py`,
  `scripts/smoke-default.toml`, `scripts/smoke-stop.toml`,
  `scripts/smoke-band-swept.toml`, `scripts/caps_scan.py`.
- CLI suites: `crates/mogwai-cli/tests/presets_cli.rs` (asserts the shipped
  preset set, so it moves with the widened arrays), `completion.rs`,
  `lifecycle.rs`, `serving.rs`, `common/mod.rs`.
- CLI fixtures under `crates/mogwai-cli/tests/configs/`: `two-symbols.toml`,
  `no-warmup.toml`, `scheduled-close.toml`, `band.toml`, `perpetual.toml`,
  `empty-scheduled-close.toml`, `bounded-run.toml`.

**Chart gate reading, named for the owner rather than decided silently:**
this brick changes which shape a no-config run resolves, not how any shape
generates - the generator method, fingerprint, seed derivation and arrival
model are untouched, and BTCUSDT's tape is byte-identical under its own
name. The recommendation is that no chart verdict is owed, on the same
reasoning the crossing landing recorded; the owner can demand one by asking
for `mogwai gen --symbol NVDA` output, which exists for free.

Gate: `brokkr check --gate`, then the live path:
`brokkr run mogwai -- serve` plus `python3 scripts/smoke.py`.

## Ordering and the keep/revert path

Brick 1 is pure hardening against current behavior and moves no default;
Brick 2 builds the boarding door against the still-current USDT default and
its tests bind USD policies to MNQ, so they survive Brick 3 unchanged; Brick
3 isolates all re-blessing in one landing with the version bump. Each brick
is revertible alone on its gate; nothing in a later brick patches an earlier
one.

## Review disposition

Two independent reviews of this spec, consolidated 2026-08-26. Every finding
below was checked at its code site before being folded in or rejected; the
accepted ones are already in the text above and are listed here only so the
next reader knows the document has been through them.

**The conflict, adjudicated.** The two reviews contradicted each other on
whether Brick 2's door is reachable. One verified the insertion point in
`ws.rs` and found it real; the other found the door shadowed by the existing
funding refusal and concluded the design was invalid. Both were half right, and
neither conclusion survives contact with the code. The insertion point is real
- `profile`, `symbol`, `account_id` and `resetting` are all in scope after the
`funded_in` refusal - and the funding door does completely shadow the door for
a freshly opened policed account, so the originally proposed principal test
could not have bitten. But the shadowing rests on an assumption neither review
tested: that a policed account's balance currencies are fixed at opening. They
are not. A spot fill credits the base asset as a balance line, which opens the
two-step path the survey now documents. The mechanism stands, the reachability
argument is rewritten, and the test list is rebuilt around the case that
actually bites. Recommending durable documentation in place of the door, as one
review proposed, would have left a reachable unvaluable state open.

**Accepted and folded in.** The `RiskLedger` guarantee that does not exist, and
the same vacuous gate pre-existing in `enforce_policy`; the unearned
unreachability claim and its two residues; `Equity::multiplier` having no serde
default; the exact provenance table in place of a placeholder; the widened
re-bless inventory and the `preset_names()` array length; the missing fixture
behind the USD-quoted spot test; the `reaches_policy_currency` naming hazard;
the out-of-the-box unreachability of BTCUSDT after the swap; and, in weakened
form, the demand that Brick 1 settle its `apply_overlay` suspect up front.

**Found during this consolidation, by neither review.** The tape-version prose
gate walks `notes/` as well as the durable folders, so this spec's own
"next takes 28" bullet is a live claim that Brick 3's bump turns stale in the
same commit. Recorded against Brick 3 as a gate obligation on the deletion. The
gate is green as the document stands - the constant is 27 and it wants live
plus one.

**Rejected, with reasons.**

- *That Brick 2's design is invalid and should be replaced by documentation
  plus a strengthened funding invariant.* Rejected on the reachability finding
  above: the state is reachable through public doors, so documenting it would
  be recording a defect rather than closing one.
- *That Brick 1 must ship a complete pre-enumerated test inventory, exact test
  names, fixtures and expected refusal text before it is approved.* Rejected
  beyond the `apply_overlay` fork, which is accepted. Brick 1 is an audit of
  which refusals are already pinned; enumerating its findings in advance is
  performing the audit, and the specification contract asks for exact artifacts
  where a fork in what gets built turns on them, which is the fork that was
  accepted. The remaining inventory is a list of sites to check, and it is one.
- *That the seven provenance keys listed by one review are the required set.*
  Superseded rather than rejected: with `class.multiplier` now required by the
  deserialize, the set is eight, and `validate_provenance` fails on an extra
  key as readily as a missing one - so a seven-key table would have failed boot
  in the other direction.

## Stopping rule

Out of scope, each parked where it is tracked:

- `Balance.locked`'s split - owner ruling still owed (`notes/todo.md`);
  Brick 3's `settlement_ns = 0` is its deliberate deferral point.
- The ledger-generality residues (shares borrow costs, leverage, funding
  payments valued in a foreign currency) and the mark-staleness bound - both
  recorded in the Engine entry; the staleness is a stated model bound, not
  work.
- Any rate surface, ever - foreclosed by the ruling this spec implements.
- The `RunComplete` duration item, the `Balance` wire shape, and everything
  tape-gated.
- New fitted presets for equities - tape fidelity gates nothing, and intake
  is slate-independent.

## What survives the landing

The commit messages, the tests, and: the boarding-door paragraph in
`reference/architecture.md`, the corrected `AccountPolicy::currency` doc,
the rewritten `DEFAULT_PRESET`/`mogwai.toml` prose, and the `AGENTS.md`
version-prose line. The two `notes/todo.md` entries and this spec are
deleted as their bricks land - the reframing prose in the Engine entry that
must endure (why one-hop is a discipline, not a gap) already lives in
`Engine::valuation_in`'s doc and the architecture section, and Brick 2 keeps
both current.
