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

A1 through A8 are CLOSED - see "Round 2" and "Round 3" below.

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

## C. `launch.rs` - closed, with one refusal

Round 1 closed the structural hole and the ETXTBSY note (already fixed twice
before the report was written, and now closed structurally). What remains open
is one refusal.

The round added a `write_venue_script` / `scripted_venue` fixture pair and
SEVEN tests, six of them over the previously untested second half of
`own_venue`:

- `a_venue_that_closes_stdout_and_lives_is_still_a_prompt_boot_failure` - the
  boot path, and the counterexample under the refusal below.
- `a_venue_that_ended_during_shutdown_still_records_its_exit` - the
  unconditional reap, on the SHUTDOWN arm.
- `a_crashed_venue_records_its_nonzero_code`
- `a_signalled_venue_records_no_exit_code`
- `a_venue_killed_while_healthy_records_no_exit`
- `a_recorded_teardown_failure_is_reported_and_not_repeated`
- `the_teardown_detail_is_read_after_the_owner_joins` - the ORDERING in
  `terminate`, which the direct-fill test above it cannot reach.

REFUSED: replacing `read_ready`'s `NoRecord` 50 ms pause with a condition.

The report's premise - "the child is already dead by then" - is FALSE, and it is
now a test. `NoRecord` means end-of-file on the STDOUT pipe, i.e. every write end
closed; a venue that closes its own stdout, or hands it to a grandchild that
closes it, reaches that branch while still running.
`a_venue_that_closes_stdout_and_lives_is_still_a_prompt_boot_failure` builds
exactly that venue (`exec 1>&-; exec sleep 60`) and pins that `launch` reports
the boot failure inside 5 s rather than waiting the child's 60 s out - well
under both the child's own life and the 300 s readiness bound, so neither of
those is what ended the wait.
So "drain stderr to EOF" is an UNBOUNDED wait on a live child, placed
immediately after the launcher has decided to report a failure - the same defect
shape the readiness reader's bound already exists to prevent, and it would hang
`launch`, which waits on `boot_tx` without a deadline of its own.

The other suggested condition, a drain-quiescent signal, still needs a bound (a
drain fed by a live child need never go quiet, and quiescence is not delivery),
so it swaps a fixed 50 ms for a bounded poll with the same worst case, more
production machinery, and a weaker guarantee. The span costs 50 ms once per
FAILED launch and nothing on any other path.

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

## Round 2: A1-A4, the risk-policy and havoc-validation cluster

All four reproduced exactly as reported, verified by measurement rather than by
reading. Nothing was refused.

- A1's shadow is REAL and was measured from both sides. With
  `trailing.amount <= ZERO` weakened to `< ZERO`, the old fixture
  (`currency: None`) still refuses - on the currency rule - so
  `a_nonpositive_drawdown_is_refused` stayed green over a deleted branch. The
  replacement `every_rule_carrying_an_amount_refuses_a_nonpositive_one_by_name`
  builds every fixture through a `policed()` helper that names `currency:
  "USD"`, so only the rule under test can fire, and asserts the exact `String`.
  Each of the four amount branches was bite-checked SEPARATELY as a `<=` to `<`
  text edit, and each failed on ITS OWN assertion with `left: Ok(())` - which is
  the whole point: under the old fixture the left side would have been the
  currency error. The positive case (all four rules at amount 1) is there so a
  validator refusing every amount cannot pass.
- A2's inverse is closed STRUCTURALLY, not by a test, because nothing can
  enumerate a `match`'s arms. `shipped_policy` now returns `None` for any name
  absent from `SHIPPED_POLICIES` before it reaches its match, so the list is
  authoritative and an arm forgotten in the list is unreachable rather than
  silently shipped. Bite-checked in two steps: with a `"nonsense"` arm added and
  the gate present, `a_name_this_build_does_not_ship_resolves_to_nothing` PASSES
  (the gate is what closes it); with the gate deleted it FAILS by name. The
  currency rule itself is now pinned in all three directions by
  `a_policy_naming_any_rule_must_name_its_currency` - `None`, `Some("")`, and
  the unpoliced account that owes no currency - and each direction was
  bite-checked with a different weakening of that one `if`.
- A3 IS CARRIED BY THE COMPILER NOW, which was the stronger of the two options
  on the table. `is_execution` and `is_admission` are exhaustive `match`es
  instead of `matches!`, so the "a new kind must opt IN to being delayed"
  comment cannot outlive its implementation: adding a variant fails to compile
  at four sites (`delay_for`, both predicates, and the new test's expectation
  match), measured by adding a `BiteCheck` variant. `delay_for` was already
  exhaustive; the two predicates were not. The test
  `every_event_kind_is_classified_deliberately` asserts the whole
  (is_execution, is_admission, latency bucket) table per kind, and
  `havoc_latency_composes_base` grew its missing `Admission` line. Bite-checked
  by moving `Admission` to `data_nanos` (both tests fail, naming Admission) and
  by folding `Admission` into `is_execution` (the table test fails on
  `(true, true, 1)` against `(false, true, 1)`).
- A4's nine bounds all have cases now, in
  `validate_divergence_bounds_flow_surge` and
  `..._bounds_fee_surcharge`, each with the in-range edges as well as the
  refusals. Bite-checked one perturbation per bound: dropping `rate_mult`'s
  `== 1.0` clause, widening `children_mult` to `[0, 1000]`, doubling each of the
  two ms ceilings, dropping `mult`'s lower gate, and widening its upper one -
  six edits, each failing on its own named case. The skipped variants are in the
  valid loop now (`RejectNextCancel`, `CancelOpenOrderSilently`), with the
  cancel-side reason ceiling and the blank-id refusal added; both were
  bite-checked (`2 * MAX_REASON_LEN`, and `trim().is_empty()` to `is_empty()`,
  which is what catches the whitespace-only id).

COLD REVIEW OF THE ROUND FOUND THREE, all real, all fixed in the same commit:

- `a_reset_minute_outside_the_day_is_refused` WAS LEFT IN THE EXACT A1 SHAPE -
  bare `is_err()`, no message, no boundary - because it was not one of the four
  branches A1 named, even though it is the sixth branch of the same `validate()`.
  It was unshadowed only by accident: its fixture was unpoliced, so the currency
  branch could not fire. Now pinned through `policed()` WITH a real rule set, at
  1439/1/0 accepted and 1440/1441/`u32::MAX` refused, against the exact message.
  Bite-checked as a `>=` to `>` text edit: fails on the 1440 case naming it.
- WHITESPACE CURRENCY WAS ACCEPTED, and the two validators in the crate
  disagreed about what blank means - `validate_divergence` refuses a blank
  `client_order_id` on `trim().is_empty()` while `validate()` used a bare
  `is_empty()`. Made to agree on TRIM, which is the production side moving.
  The reason: the currency is a lookup key that equity is summed over, so a
  whitespace code matches no balance and freezes a policed account's equity at
  zero rather than refusing the policy at registration, which is what `validate`
  is for. `a_policy_naming_any_rule_must_name_its_currency` now loops six blank
  forms; bite-checked by reverting the `trim()`, which fails on `" "`.
  NOT CLOSED, and deliberately: `" USD "` is still accepted and still matches no
  balance. Neither validator normalizes. The stronger rule - refuse any code
  differing from its trimmed form - belongs on both sides at once, not smuggled
  in here.
- The two adjacent docs read as contradicting each other, `SHIPPED_POLICIES`
  "AUTHORITATIVE" ten lines above `shipped_policy` "ILLUSTRATIVE RATHER THAN
  AUTHORITATIVE". They are about different axes - the set of NAMES versus the
  TERMS behind them - and the second now says which axis it disclaims.

LATERAL, fixed in passing: `messages.rs :: ServerMessage::category`'s doc glossed
`is_execution` as "everything but `Data`", which is false - `Admission` is not
execution, and that exemption is exactly what the paragraph three lines up is
about.

## Round 3: A5-A8, the arm classification and three false referents

All four reproduced. Nothing was refused, and one finding's recommended remedy
was overtaken by a stronger one.

COLD REVIEW OF THE ROUND FOUND FOUR, all real, all fixed in the same commit;
the details are in `notes/bug-loop-carry-forward.md`. The one that changes what
this section claims is first: A5's remedy secured the ENGINE's classification
and left the VENUE'S ROUTER unguarded. `mogwai-server`'s `arm_divergence` still
ended in a catch-all whose only guard was a `debug_assert!` - compiled out of
the release profile the socket suites run in - so a variant newly classified
server-owned in `divergence.rs` would still have compiled there and been
forwarded to `engine.arm()`, which now DROPS it: the control would have gone
silently dead. That router enumerates the five engine-armed variants now, and
its stale prose (it named four server-owned variants three lines above an
assert listing eight, and concluded "four engine-side variants" where there are
five) is corrected. The other three were the vacuous zero-band control, an
`assert_eq!(out.len(), 4)` moved after the indexing it licenses, and a
`field_reassign_with_default` in the new adapter test.

- A5 IS CARRIED BY THE COMPILER, which the report did not propose and round 2's
  A3 predicted. The production comment claims listing the server-owned variants
  "stops a future enum variant from falling through into engine behaviour by
  accident" - a claim about variants that do not exist yet, so no test can hold
  it. `arm`'s match had an `other =>` catch-all, so nothing held it at all: a
  new server-owned variant fell straight into the queue as a dead entry. The
  five engine-side variants are now enumerated too
  (`queued @ (PartialFillNext | RejectNextSubmit | RejectNextCancel |
  DuplicateNextFill | DropNextAccountUpdate)`), so the match is exhaustive on
  both arms and the crate does not build until a new variant is classified.
  Measured by adding a `BiteCheck` variant: E0004 at `divergence.rs` AND at the
  new test's expectation match, both sweeps.
  - The test half is `arm_classifies_every_divergence_variant`, which loops all
    thirteen variants, reads THE QUEUE (`e.armed`) rather than an event count,
    and derives its expectation from a second exhaustive match - deliberately
    not from `!is_server_owned`, because one shared list would let a new variant
    be classified once and read twice. Bite-checked by moving `CommandLatency`
    into the queued arm: fails naming that variant and the dead-entry
    consequence. `CommandLatency`, `FlowSurge`, `FeeSurcharge` and
    `CancelOpenOrderSilently` - the four the old test never touched - are all
    exercised now.
  - WHAT STILL CANNOT BE HELD, and it is stated on the fixture: the case list is
    hand-built and nothing checks it stays complete. A variant added to the enum
    and forgotten there is still classified deliberately on both sides (neither
    match compiles until it is); what it loses is the end-to-end exercise. Same
    shape as round 2's `SHIPPED_POLICIES` ruling - state the rule the code can
    enforce.
  - The original test KEPT its name and its second half, which is the "without
    blocking engine divergences" claim the new one does not make. It asserts the
    queue directly now - after five arms, `DuplicateNextFill` alone and at the
    FRONT - so a dropped variant that instead queued a dead entry ahead of it is
    visible rather than inferred from `out.len() == 4`.
- A6 REPRODUCED EXACTLY. `>=` / `<=` survives deleting slippage: with
  `draw_market_price` returning `last_px` on both sides the old assertions pass.
  Rewritten on the trigger-band test's pattern, but END TO END through
  `process_with_market` rather than on the draw, because `draw_market_price` is
  private to `orders.rs`: 64 orders per side, a fresh `banded(42)` engine each
  (so no order pays for another's balance), every order that slips must slip the
  adverse way, and at least one per side must slip. THE ZERO BAND IS THE
  CONTROL and is what makes "nothing slipped" distinguishable from "the engine
  ignores the band": every order is submitted at `band_ticks = 0` first and must
  fill at the last print exactly. THE CONTROL'S LAST PRINT IS 99 AGAINST A
  STATED PRICE OF 100, which the cold review had to correct - at 100 against 100
  it could not tell the band from the stated price it is documented to ignore,
  so it was itself the vacuous test it exists to prevent. Bite-checked twice as
  text edits in
  `draw_market_price` - no slip at all fails "some Buy in the fixture must slip",
  inverted sides fail the buy direction.
- A7 HAD NO REFERENT AND THE TEST IS DELETED. Verified rather than assumed: the
  constant is defined once, every adapter site
  (`clock.rs`, `client/shared.rs`, `data.rs`, `exec.rs`) reads it by name, and
  the literal 30 appears nowhere else in the workspace, so the assertion could
  only restate the definition. THE CLAIM THAT DOES HAVE A REFERENT is the
  SUBSTITUTION the constant exists for - `ConnHavoc.request_timeout_secs == 0`
  keeps the shipped default - and it had NO coverage anywhere: deleting the
  `if configured == 0` branch left the whole workspace green.
  `an_unset_request_timeout_takes_the_shipped_default` in
  `mogwai-adapter/src/client/shared.rs` now pins all three directions (no spec,
  an explicit zero, a stated 7) at speed 1.0 so neither the scaling nor the
  `MIN_WALL_REQUEST_TIMEOUT_SECS` floor is in the way. Bite-checked by deleting
  the substitution: the unconfigured client's timeout collapses to the 1-second
  wall floor and the test fails 1 against 30. A test in `mogwai-protocol` could
  not have reached this; the crate the constant lives in was the wrong place to
  look for its second side.
- A8 GOT BOTH HALVES. The protocol test is renamed
  `default_instruments_ships_one_btcusdt_spot_definition`, which is what it does
  - a value pin on the shipped wire defaults, legitimate on its own - and its
  doc comment now says why this crate cannot make the cross-check and where the
  one that can lives. THE REAL GATE IS IN THE ENGINE:
  `the_default_seed_puts_the_engine_on_a_btcusdt_cent_and_satoshi_grid` reads
  the increments back out of `Engine::new`'s ORDER VALIDATION - an on-grid order
  accepted, a tenth of the price increment refused with "price violates price
  increment", a tenth of the size increment refused with "quantity violates size
  increment". Bite-checked with one text edit per increment in
  `default_instruments`: each fails on its own named case. It does NOT use
  `reject_reason`, whose panic on an accepted order names the helper's shape
  instead of the grid - the refusal is destructured locally with a message
  naming the increment.
- LATERAL, fixed in passing: none found. The one thing worth knowing is that the
  A8 perturbations fail the protocol value pin AND the engine gate together,
  which is correct - the value pin is not redundant, it just cannot be the
  cross-check its old name claimed.

## The hunter's own ordering

C and A1-A8 are done. Next is B1 (six unpinned wire-size constants that a
reservation system depends on).
