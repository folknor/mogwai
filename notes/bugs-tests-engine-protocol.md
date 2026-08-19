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

A1 through A4 are CLOSED - see "Round 2" below.

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

## The hunter's own ordering

C and A1-A4 are done. Then A5 (a production comment asserting a property no test
holds), then B1 (six unpinned wire-size constants that a reservation system
depends on).
