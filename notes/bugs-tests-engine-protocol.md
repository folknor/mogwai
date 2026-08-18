# Test hunt: engine and protocol unit tests

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope: every
`#[cfg(test)]` module under `crates/mogwai-engine/src/` and
`crates/mogwai-protocol/src/`.

This hunt looks for defects in the TESTS, not in the code they test, weighted
toward "can it fail at all" because the scope is pure logic.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.

## Scope shape

`mogwai-engine/src/{lib,orders,account,divergence}.rs` - all roughly 200 tests
live in one module in `lib.rs`; `orders.rs` at 3078 lines, `account.rs` at 782,
and `divergence.rs` have ZERO in-file tests. `mogwai-protocol/src/*` - 13
modules, 9 with test modules. Neither crate has a `tests/` directory. No
`#[ignore]` anywhere in scope, so nothing to reconcile against `brokkr.toml`'s
skip list.

WHAT IS ALREADY RIGHT, because it is load-bearing for the rest: the engine test
module is completely hermetic - no `static`, no `OnceLock`, no `thread_local`, no
filesystem, no `SystemTime`, no `sleep`, no `Instant`, no wall-clock budget. It
is safe under `--test-threads=8` by construction. The two `cfg!(debug_assertions)`
reconciliation checks that need pinning have correctly
`#[cfg(debug_assertions)]`-gated tests
(`reservation_cache_reconciliation_catches_drift_before_a_funded_command`,
`a_group_member_defunded_by_its_own_group_is_the_disclosed_carve_out`), and both
carry a comment explaining the gate. Someone already paid the profile-split
tuition here.

## A. Tests that cannot fail for the reason they name

**A1. `risk.rs :: a_nonpositive_drawdown_is_refused` - shadowed by a different
rule.** The fixture is
`AccountPolicy { trailing_drawdown: Some(..amount: ZERO..), ..default() }`, which
leaves `currency: None`. Setting any rule makes `is_unpoliced()` false, so
`validate()`'s CURRENCY check fires unconditionally for this fixture. The test
asserts only `is_err()`. Delete the `trailing_drawdown.amount <= ZERO` branch
from production and this test stays green. The same fixture shape would shadow
the daily/overall/max_position amount checks too - except those have no tests at
all. Fix: assert the exact message (the module already returns distinct
`String`s), and set `currency: Some("USD")` so only the rule under test can fire.
Then add the four missing sibling cases plus a positive case.

**A2. `risk.rs` - the currency rule and the `SHIPPED_POLICIES`/`shipped_policy`
inverse are unpinned.** `every_shipped_policy_is_usable` iterates the constant
list, so a policy added to `shipped_policy()` but forgotten in
`SHIPPED_POLICIES` is invisible to every test. Nothing asserts
`shipped_policy("nonsense") == None` either, and nothing tests the currency
requirement directly.

**A3. `havoc.rs :: havoc_latency_composes_base` omits `EventKind::Admission`.**
The production doc argues at length that `Admission` buckets with `Exec` and that
`is_execution()` is the single place implementing the `DelayAcks` exemption ("a
new kind must opt IN to being delayed"). Neither `is_execution()` nor
`is_admission()` has a single test, and `delay_for(Admission)` is never called.
Move `Admission` to `data_nanos`, or flip `is_execution` to `!matches!(Data)`,
and nothing in either crate goes red.

**A4. `havoc.rs` - `FlowSurge` and `FeeSurcharge` validation is entirely
untested.** `validate_divergence` has nine distinct bound checks across those two
variants (`rate_mult` in (1,1000] WITH an explicit `== 1.0` rejection,
`children_mult` in [1,100], both `duration_ms`/`window_ms` ceilings, `mult` in
(0,100]). Zero coverage. The "every non-numeric variant" loop also skips
`RejectNextCancel` and `CancelOpenOrderSilently`.

**A5. `engine :: arm_drops_temporal_variants_without_blocking_engine_divergences`
covers 4 of 8 dropped variants.** It arms
`DelayAcks`/`GoDark`/`StallData`/`ClearDivergences`. The production match in
`divergence.rs` also drops `CommandLatency`, `FlowSurge`, `FeeSurcharge` and
`CancelOpenOrderSilently` - and the comment beside them says listing them
explicitly "is what stops a future enum variant from falling through into engine
behaviour by accident." Exactly the claim a test must hold. Move any of those
four into the `other` arm and they become permanent dead queue entries with no
test failing. The test also INFERS dropping from `out.len() == 4` rather than
asserting `e.armed.is_empty()` - assert the queue directly and loop over all
eight.

**A6. `engine :: a_market_buy_slips_up_and_a_market_sell_slips_down` asserts
`>=` / `<=`.** Delete slippage entirely (always fill at `last_px`) and it passes.
Its sibling `a_nonzero_band_displaces_a_trigger_adversely_from_its_stated_price`
gets this right - it runs a 64-order fixture, filters to a nonzero draw, and
asserts strict inequality - but that test exercises `draw_trigger`, not the
market-fill slip path. Rewrite A6 on the same pattern.

**A7. `protocol/lib.rs :: default_request_timeout_secs_is_thirty`** asserts a
constant equals its own literal. It is a change-detector with no external
referent - there is no second definition of 30 anywhere for it to pin against.
Delete it or make it pin the adapter's use.

**A8. `instruments.rs :: default_instruments_matches_engine_btcusdt_seed`** - the
name promises agreement with the engine's seed, but `mogwai-protocol` cannot
depend on `mogwai-engine`, so the test is a pure self-pin of the same function
three lines above it. Either rename it to what it does, or move the real
cross-check into the engine, where both sides exist.

## B. Fixtures that exclude the bug by construction

**B1. `sizing.rs :: account_state_bound_covers_a_max_length_account_id` uses
`BookShape { balances: 0, positions: 0, margins: 0, .. }` and an `AccountState`
with three empty `Vec`s.** It exercises exactly the
`144 + ESC * MAX_ACCOUNT_ID_LEN` envelope term and NONE of the row terms.
`BALANCE_ROW_MAX_BYTES`, `POSITION_ROW_MAX_BYTES`, `MARGIN_ROW_MAX_BYTES`,
`ORDER_STATUS_ROW_MAX_BYTES`, `FILL_ROW_MAX_BYTES` and
`SNAPSHOT_ENVELOPE_MAX_BYTES` have NO MAXIMAL-ROW TEST AT ALL - halve any of them
and only the engine's sampled `worst_case_reservation_covers_actual_output` might
catch it, and only if the matrix happens to reach that row shape. The module's
own doc says "every constant below carries a field-by-field derivation," and
`order_event_bound_covers_both_maximal_lifecycle_frames` is the model to copy: it
constructs both maximal frames with U+0001 fill. Do the same for each of the six
row constants.

**B2. Same test, second problem: the id is `"Z".repeat(MAX_ACCOUNT_ID_LEN)`,
which JSON does not escape.** The `JSON_ESCAPE_FACTOR` term is therefore never
load-bearing in the assertion. Its sibling three tests down does this correctly
with `char::from(1)` and says so in the doc comment. (It is arguably harmless
because `AccountId::parse` restricts the alphabet - but then the `ESC *` in
`account_state_max_bytes` is 6x dead weight on every reservation, which is worth
knowing either way.)

**B3. `instruments.rs` - every `InstrumentClass::Equity` accessor is tested only
at its default.** `an_equity_credits_no_currency_balance` is the sole equity test
in the module, and it builds `lot_size: ONE, borrowable: None, settlement_ns: 0`
and then never calls `lot_size()`, `borrowable()` or `settlement_ns()`. Implement
all three as `Decimal::ONE` / `None` / `0` ignoring the field and every protocol
test passes. (The engine's `Shares` fixture DOES parameterize these correctly and
covers the behaviour - so this is a coverage-attribution issue rather than a live
hole, but the accessors themselves are unpinned.) Relatedly,
`instrument_def_round_trips` covers only `spot` and `future`; the `equity`,
`perpetual` and `inverse` wire tags and the `one_share` serde default are
unpinned byte-wise.

**B4. `engine :: sale_proceeds_are_held_unsettled_until_their_instant` steps over
its own boundary.** Sale at `ts=2`, settlement `2*DAY`, so the instant is
`2*DAY + 2`. The test probes `2*DAY` (false) and `2*DAY + 3` (true) and never the
instant itself. A `>` / `>=` flip in `release_settled_cash` is invisible.

**B5. No perpetual-short funding test.**
`a_long_perpetual_pays_funding_on_its_marked_notional` and
`a_negative_funding_rate_pays_the_long` both hold a LONG; the sign is varied only
via the rate. If `apply_funding` took `qty.abs()`, a short would also pay funding
- direction inverted, no test red. The short side is the half where the sign
convention is actually easy to get wrong.

## C. The biggest structural hole: `launch.rs` never launches a venue that works

Every `launch()` test in `mogwai-protocol` exercises a FAILURE path: missing
binary, `true` (no line), `echo` (malformed line), zero timeout, and
`silent_venue()` (timeout). The parse tests call `parse_ready` directly. NOTHING
IN THE CRATE EVER GETS PAST READINESS, so the entire second half of `run_venue` -
lines roughly 689 to 751 - is untested here: the `OWNER_POLL` loop,
`child.try_wait()`, the `VenueExit` record, the "reap is unconditional on the way
out" fix and its long comment about a bounded run reaped with no `VenueExit`,
`LaunchedVenue::shutdown()`'s return value, and the `teardown` failure detail. A
code hunt earlier the same day found a vacuous `shutdown()` result and a skipped
reap in exactly this region. Those are the two things a unit test would have
caught and there is no unit test.

The fix is cheap and the file already contains the pattern: `silent_venue()`
writes a 2-line `#!/bin/sh` into `target/` and chmods it. Generalize it into a
`scripted_venue(readiness_line, then)` fixture that prints a valid `ReadyRecord`
JSON line, flushes, and then either sleeps, exits 0, or exits nonzero. That
unlocks, as pure unit tests: a clean bounded-run exit records
`VenueExit { success: true, code: Some(0) }`; a crashed venue records the nonzero
code; `shutdown()` on a venue that will not die reports rather than returning
`Ok(())`; `exited()` is `None` only for a venue killed while healthy. The hunter
would write that before anything else on this list.

Two smaller launch notes:

- `silent_venue()` writes to a FIXED SHARED PATH
  (`crates/mogwai-protocol/../../target/mogwai-silent-venue.sh`) with no
  uniquification. One test claims it today, so it is fine - but any second test
  using the same helper concurrently under `--test-threads=8` gets a write racing
  an `exec` (ETXTBSY). Uniquify by test name when you generalize it.
- `read_ready`'s `NoRecord` path sleeps a fixed 50 ms to let the stderr drain
  thread deliver. That is a fixed span on the success path of error construction,
  in PRODUCTION code. It is the one place in either crate that waits on a
  duration where it could wait on a condition (e.g. a drain-quiescent signal, or
  draining to EOF since the child is already dead by then).

## D. Weaker signals worth a note

- **`worst_case_reservation_covers_actual_output` bounds only one direction.** It
  asserts `actual <= bound` across a genuinely excellent matrix (escaped ids,
  deep book, full-size `Ouo` group, hedged futures, armed divergences - this is
  the best test in either crate). But a derivation that over-reserves by 1000x
  also passes, and over-reservation silently costs every connection its byte
  budget. The admission-frame test in `messages.rs` gets this right with
  `ADMISSION_FRAME_MAX_BYTES < 2 * analytic`. Consider recording a slack ceiling
  per case.
- **`a_zero_initial_margin_policy_cannot_drift_the_reservation_cache`** has bite
  only under `debug_assertions` (its whole point is that the reconciliation must
  not panic), but it is NOT `#[cfg(debug_assertions)]`-gated. In a release sweep
  it asserts only `engine.open.is_empty()` - vacuous. Not a failure, but its
  coverage silently disappears in `brokkr test`, unlike its correctly-gated
  sibling 20 lines up.
- **`messages.rs` has 5 tests for 1992 lines.** `validate_submit_order`,
  `validate_modify_order`, `validate_submit_group`, `validate_client_order_id`,
  `validate_request_id`, `validate_session_id`, `AccountId::parse`,
  `truncate_client_id`, `truncate_reason`, `touches_toward`, `touches_trigger`
  are all exercised only indirectly through engine tests, or not at all.
  `MAX_SESSION_LEN` / `validate_session_id` the hunter could find no test for on
  either side.
- **Doc/constant drift in `sizing.rs`:** `FILL_ROW_MAX_BYTES` doc says "rounded
  to 320", the constant is `384`. `POSITION_ROW_MAX_BYTES` doc says "four
  decimals" while listing six fields. These are the derivations the module doc
  says are the proof, so drift in them is drift in the argument.
- `engine :: submit_rejects_semantically_invalid_inputs` is the counter-example
  to the recorded `min_price = 10.0` incident and is worth copying elsewhere: it
  pins exact reason strings AND asserts balances, positions and the open book are
  all untouched after each refusal.
- Assertion-strength outliers: `reason.contains("trigger")` appears twice (lines
  around 4074 and 8631) and admits a lot; every other `contains` in the engine
  names a specific phrase and is fine.

## The hunter's own ordering

C first (a scripted-venue fixture unlocks the whole untested half of the shipped
launcher), then A1/A5/A3 (three cases where production comments assert a property
no test holds), then B1 (six unpinned wire-size constants that a reservation
system depends on).
