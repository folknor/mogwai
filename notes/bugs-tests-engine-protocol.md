# Test hunt: engine and protocol unit tests

CITATIONS BELOW TO `notes/bug-loop-carry-forward.md` ARE DEAD LINKS. That file
held the eleven-document arc's round-by-round state and was DELETED when the arc
closed on 2026-08-20, per `AGENTS.md`'s rule that a closed arc folds what still
binds into `AGENTS.md` and deletes its carry-forward. The standing lessons are
there; the deleted text resolves to git history.

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

B1 through B5 are CLOSED - see "Round 4" below.

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

D is CLOSED - see "Round 5" below. Two of its items were closed by round 4 out
of turn (the `FILL_ROW_MAX_BYTES` doc drift, fixed; the `POSITION_ROW_MAX_BYTES`
"four decimals" claim, refused as already correct), and round 5 closed the rest.

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

## Round 4: B1-B5, the wire-size constants and two one-sided fixtures

All five reproduced. Nothing was refused, and B2's parenthetical - the half the
brief called the interesting part - resolved in favour of DELETING the factor
rather than testing an unreachable case.

COLD REVIEW OF THE ROUND INDEPENDENTLY VERIFIED THE PRODUCTION CHANGE and found
four things, all fixed in the same commit. The one that mattered is first,
because it is a shape this arc had not seen: THE ROUND'S OWN PRODUCTION
IMPROVEMENT MADE AN EXISTING MEASUREMENT ERROR MATTER.

- THE ACCOUNT-STATE FIXTURES MEASURED A BARE `AccountState` while the
  reservation covers the TAGGED `ServerMessage::AccountState` frame - about 21
  bytes of `"type"` the struct alone does not carry. Not a live bug, and it had
  been harmless while the term carried 351 bytes of fixed slack; dropping the
  dead escape factor cut that to 31, at which point a fixture under-measuring by
  22 is most of what a bite-check has left to work with. Both fixtures wrap in
  `ServerMessage` now, and the halving of the `144` addend fails at the TAGGED
  size, 136 against 164. The two sibling tests added in the same change already
  wrapped correctly, so this was an inconsistency inside one commit.
- THE EQUITY-ACCESSOR CONTROL LOOP HELD ONLY `Spot` AND `Future` under a doc
  comment claiming "every other class". `Perpetual` and `Inverse` are in the
  loop now, so the comment is true and all four non-equity classes are pinned at
  the documented constants.
- THE ALPHABET TEST'S TWO HALVES ARE NOT THE SAME STRENGTH and the test now says
  so: the `0..=0x7f` sweep is a proof by enumeration, the "nothing above U+007F"
  half is a SAMPLE of six characters whose real proof is the `parse` predicate
  read. The sample is there so a widening that made the sweep stop being the
  whole domain fails something.
- The short-funding test's second row is a VALUE PIN, not a mirror: no long test
  runs a negative rate at mark 50,000 with the amount asserted, so the "same
  rule read from the other side" comment read stronger than it was. Row one is
  the exact mirror; the comment now distinguishes them.

Two further findings were REFUSED as out of scope: both concerned
`analysis/asia_jump_probe.py`, which is untracked scratch work unrelated to this
document and stays untracked.

- B1 IS CLOSED WITH FOUR TESTS IN `sizing.rs`, and every one of the six
  constants was bite-checked by HALVING its fixed addend, as the report
  proposed. `every_row_bound_covers_its_maximal_row` builds a maximal row for
  each of the five row constants - every string at its cap filled with U+0001,
  every `Decimal` at `Decimal::MIN`, every optional field PRESENT and every
  enum at its longest spelling (`TrailingStopMarket`, `PartiallyFilled`) -
  and each halving failed on its own labelled entry:
  balance 192 against 234 wire bytes, position 704 against 785, margin 384
  against 405, order status 1600 against 1830, fill 2016 against 2184.
  `the_snapshot_envelope_bound_covers_an_empty_reply_of_either_kind` measures
  the envelope with NO rows, so nothing else can pay for it: halved, 448
  against 474.
  - THE PER-ROW ASSERTIONS ARE NOT THE WHOLE CLAIM, because a row bound charges
    nothing for the comma between rows. Two aggregate tests hold the
    composition the server actually reserves against:
    `account_state_bound_covers_an_empty_and_a_maximal_snapshot` (empty, then
    seven rows of all three kinds) and
    `query_reply_bounds_cover_their_maximal_snapshots` (both query replies at
    seven rows through `worst_case_output_bytes`). The two order-status and
    fill halvings fail BOTH the row test and the reply test, which is the
    evidence the aggregate is not just restating the per-row one.
  - THE MEASURED SLACK, which section D's over-reservation item will want:
    balance 288/234, position 832/785, margin 480/405, order status 1856/1830,
    fill 2208/2184, envelope 512/474. Every row constant is inside 20 percent
    of its worst case and two are inside 2 percent, so the derivations are
    TIGHT, not 1000x loose - the over-reservation D worries about is not in
    these constants. The account-state envelope is the loose one, at 208
    against 164.
  - THE 164 IS A CORRECTION THE COLD REVIEW FORCED, and it is worth stating
    because a later round setting a slack ceiling would inherit the error. The
    round's first fixture measured a BARE `AccountState`, 142 bytes. What the
    server reserves for is `ServerMessage::AccountState`, and `ServerMessage`
    is tagged, so the real frame is 164 - the fixture under-measured the term
    it guards by 22 bytes. That mattered only because the round itself removed
    320 bytes of dead account-id slack from the same term: the two sibling
    tests added in the same change already wrapped in `ServerMessage`, so the
    inconsistency was inside one commit. Both fixtures wrap now, and the
    re-bite-check at the tagged size fails 136 against 164.
  - NO SEVENTH UNEXERCISED BOUND WAS FOUND. Sweeping every `*_BYTES` in the
    crate: `ORDER_EVENT_MAX_BYTES` already had its maximal test,
    `BOUNDARY_REFUSAL_BYTES` and `LINKAGE_MAX_BYTES` are defined in terms of it,
    `ADMISSION_FRAME_MAX_BYTES` has its own two-sided test in `messages.rs`, and
    `MAX_CLIENT_MESSAGE_BYTES` is an inbound cap rather than a derivation.
    `swept_batch_max_bytes` is the one FUNCTION with no test naming it; it is
    `swept_fill_max_bytes` with a fifth frame charged per venue-originated
    order, so it dominates a sampled bound by construction. Left alone
    deliberately.
- B2 IS SETTLED AGAINST THE FACTOR: `account_state_max_bytes` now charges the
  account id at `MAX_ACCOUNT_ID_LEN`, not at `ESC * MAX_ACCOUNT_ID_LEN`. The
  parenthetical was right - `AccountId` is a newtype whose ONLY constructor is
  `AccountId::parse` (verified by grep: no other `AccountId(` exists in the
  workspace, and `Deserialize` routes through `parse`), and its alphabet is
  ASCII alphanumerics plus `.`, `_`, `:` and `-`, none of which `serde_json`
  escapes. So the 6x was not a bound at all, it was 320 bytes of dead weight on
  every reservation naming an account - which a nine-member group pays ten
  times over, since it charges `members + 1` snapshots.
  - WHAT REPLACES THE TEST-FOR-AN-UNREACHABLE-CASE is a test for the PREMISE.
    `the_account_id_alphabet_carries_nothing_json_escapes` sweeps the whole
    accepted domain - all of `0..=0x7f`, exhaustive because `parse` requires
    `is_ascii_alphanumeric` or one of four ASCII marks so nothing above U+007F
    can be accepted - asserts every accepted character serializes to exactly
    three bytes, and pins the accepted count at 66. Bite-checked by adding
    `'\u{1}'` to `parse`'s allowed set: fails naming the character, its 8 wire
    bytes, and the fact that the raw-cap charge owes its factor back. The
    non-ASCII half is pinned by a short refusal list so the ASCII sweep cannot
    silently stop being the whole domain.
  - The claim is stated where it is spent, on `account_state_max_bytes` and in
    the module doc, both of which previously said every string is charged the
    factor.
- B3 GOT ALL THREE HALVES. `the_equity_accessors_report_the_terms_the_class
  _states` states `lot_size: 100`, `borrowable: Some(500)` and a T+2
  `settlement_ns`, and each accessor was bite-checked separately by making it
  ignore its field. `Some(0)` IS ITS OWN CASE and earned its place: a
  `borrowable.filter(|b| !b.is_zero())` passes the `Some(500)` assertion and
  fails only on the hard-to-borrow one, which is exactly the collapse that
  would turn "no locate available" into "no borrow market modelled". The
  non-equity control (spot and future answering the three constants) is
  bite-checked too, per the standing rule that a control is a test: with
  `lot_size`'s fallback moved to `ZERO` it fails naming the class.
  - The wire half: `instrument_def_round_trips` covers all five classes now,
    bite-checked by renaming the `equity` tag to `shares`. The `one_share`
    serde default has its own test,
    `an_equity_omitting_its_optional_terms_takes_the_documented_defaults`,
    which decodes an equity stating only currency and multiplier; bite-checked
    by returning `Decimal::ZERO` from `one_share`, which is the failure that
    would make every quantity a whole number of nothing.
- B4 REPRODUCED AND THE OLD PROBES REALLY WERE BLIND. Under a `>` to `>=` flip
  in `release_settled_cash` the credit settles at `2 * DAY + 2`, so the old
  test's `2 * DAY` still retains and its `2 * DAY + 3` still releases - both
  assertions pass over the flipped comparison. The test now names the instant
  as a `const` and probes `SETTLES_AT - 1` (false), `SETTLES_AT` (true) and
  `SETTLES_AT + 1` (false, because a credit settles once and the second pass
  must move nothing). Bite-checked: the flip fails on "and the instant itself
  settles it".
- B5 REPRODUCED EXACTLY AS THE REPORT PREDICTED, `qty.abs()` and all.
  `a_short_perpetual_receives_the_funding_a_long_pays` mirrors the long test at
  the same marks - a short of 10 marked at 60,000 at one basis point RECEIVES
  exactly the 60 the long pays - and carries the negative-rate short as a
  second row, where it pays 50. It asserts the short actually filled before it
  reads the balance, so "no position" cannot masquerade as "no funding".
  Bite-checked by taking `position.qty.abs()` in `apply_funding`: the new test
  fails with `-60` against `60` and BOTH long tests stay green, which is the
  measurement behind the report's claim.

## Round 5: section D, and one praised test that did not deserve it

Four of the six reproduced, one was refused with a measurement, and one had
already been closed by round 4. The round's own lateral find is the sizing
module's prose naming the wrong widest frame.

- THE SLACK CEILING IS `bound < 2 * actual` AND IT IS IN `sizing.rs`, NOT IN THE
  ENGINE TEST THE FINDING NAMED. Both halves of that are measured.
  - Every bound in `sizing.rs` now goes through one `brackets(label, bound,
    actual)` helper that asserts BOTH sides against the maximal fixture that
    defines the constant. Re-measured this round rather than inherited: balance
    288/234, position 832/785, margin 480/405, order status 1856/1830, fill
    2208/2184, snapshot envelope 512/474 and 512/466, account-state envelope
    208/164, account state at seven rows of each kind 11408/10163, the two query
    replies 13504/13290 and 15968/15760. Round 4's table is confirmed exactly.
    Loosest is the account-state envelope at 1.27x. The factor 2 is the one
    `admission_frames_fit_their_ceiling` already uses in `messages.rs`, and a
    tighter one is deliberately NOT taken: these addends are rounded up to round
    figures by design, so a small struct's rounding can legitimately approach 2.
    Bite-checked by QUADRUPLING two addends - `BALANCE_ROW_MAX_BYTES` 192 to 768
    and `account_state_max_bytes`'s 144 to 1000 - and both fired on the CEILING
    assertion, at 864 against 234 and 1064 against 164.
  - THE CEILING BELONGS TO THE DOMINATING FIXTURE, which cost the round one
    correction and produced its lateral find. `ORDER_EVENT_MAX_BYTES` is the max
    of two derivations, and measured, the WIDER frame is `OrderRejected` at 3545
    bytes, not `OrderFilled` at 2205 - one `MAX_REASON_LEN` at the escape factor
    outweighs four ids, a symbol and a currency. So the constant resolves to the
    REJECTION arm (4032 against the fill term's 2400), the ceiling is asserted
    against `OrderRejected` alone, and the module's prose saying "Widest shape is
    `OrderFilled`" was FALSE and is corrected.
  - REFUSED, WITH THE MEASUREMENT: a per-case ceiling in
    `worst_case_reservation_covers_actual_output` itself. The bound there covers
    the widest output a command CLASS can produce while the test feeds one
    command to one book, so the ratio over the matrix runs 2.2x (a query against
    the deep book, where the row terms really do carry it) to 249x (a cancel
    refused on the deep book: one 152-byte frame against a bound that must also
    cover a full linkage reap and a widened snapshot). Any table of per-case
    numbers over that spread reads as a gate and is not one. The reasoning is now
    in the test beside the assertion, naming where the two-sided bracket lives.
- `a_zero_initial_margin_policy_cannot_drift_the_reservation_cache` IS
  STRENGTHENED RATHER THAN GATED, which is the better of the two and was
  available because the test module sits inside `lib.rs` and can read
  `engine.order_locked` directly. It now asserts the cache is EMPTY while the two
  zero-hold orders are still RESTING - the only instant the defect is visible,
  because by the time both cancels have run the incremental remove has deleted
  the key in the broken build too, so the end state cannot tell them apart.
  Bite-checked by deleting the zero-hold `filter` in `order_reservation_entry`:
  fails in the WORKSPACE (dev) sweep and in the TIMING (release) sweep alike,
  naming `{"USD": 0}`. Worth recording: the reconciliation panic did NOT fire
  first on that perturbation, so the debug-only witness the old test relied on
  would not have caught it even in dev.
  - THE CLASS WAS SWEPT AND THE FILE HAS EXACTLY ONE INSTANCE. Every
    `debug_assert` and `cfg!(debug_assertions)` site in both crates was read:
    two reconciliation call sites in `lib.rs`, two in `orders.rs`, three bare
    `debug_assert!`s, and the `corrupt_order_locked_for_test` helper. The two
    tests whose ONLY witness is a debug panic are correctly
    `#[cfg(debug_assertions)]`-gated already; this was the third and it needed no
    gate once it had an assertion of its own.
- `messages.rs` GOT FOUR TESTS, chosen by what has NO coverage on either side
  rather than by the function list. The audit first: `validate_submit_order`,
  `validate_client_order_id` and `validate_submit_group` ARE covered, directly
  and by name, from the engine's test module (the trailing-stop, conditional and
  link rules) and through `mogwai-server`'s boundary. What was uncovered:
  - `validate_session_id`, exactly as the hunter said. Its three existing uses
    are all POSITIVE - two production call sites and `adapter_smoke.rs` using it
    as an oracle on a minted session - so a body of `Ok(())` passed the whole
    workspace. `session_ids_are_the_url_safe_alphabet` pins the alphabet, both
    ends of the cap and the explicit empty ruling. Bite-checked by neutering the
    length guard: fails on `""`.
  - The char-boundary walk in `truncate_reason` / `truncate_client_id`, which
    runs on the ECHO path of a refusal and is reached only by a multi-byte
    character straddling the cap. Bite-checked by flipping `end -= 1` to
    `end += 1`: fails at 66 against 62.
  - `validate_request_id`'s own boundary, and the fact that BOTH echo guards
    accept an empty id deliberately. Bite-checked with `<=` to `<`.
  - `trades_through` / `touches_trigger` / `touches_toward` AT the price, which
    is the only argument that separates them, plus `ScanKind::hit`'s dispatch.
    The complement claim in `trades_through`'s doc comment - same side, same
    price, exact logical complements - is now runnable. Bite-checked by relaxing
    `trades_through` to `<=`: fails naming the side and the price.
- THE PRAISE IN ITEM 4 IS HALF DESERVED AND THE HALF THAT IS NOT IS THE ARC'S
  SIGNATURE DEFECT. `submit_rejects_semantically_invalid_inputs` really does pin
  every reason by exact string, and `reject_reason` really does enforce exactly
  one frame. But its "the ledger is untouched" assertions ran on `Engine::new()`,
  which starts with NO BALANCES - so `balances.is_empty()` was the claim "no row
  was created", not "no funds moved", and a refusal that debited or locked funds
  it had no right to passed it for free. The fixture is `funded(10_000)` now and
  the assertion is a real before/after on `(total, free, locked)`. Bite-checked
  by debiting one USDT immediately before `validate_submit` in `on_submit_from`:
  the new form fails at `(9999, 9999, 0)` against `(10000, 10000, 0)`, and the
  OLD form passes that same perturbation untouched, because `and_modify` on an
  absent key does nothing and the empty map stays empty.
- BOTH `reason.contains("trigger")` SITES ARE EXACT STRINGS NOW. The two FOK
  refusals they discriminate are one clause apart - "fill-or-kill could not fill
  at its trigger" and "fill-or-kill could not fully fill" - and the second site's
  positive `contains` admitted any future refusal that mentions a trigger price.
  Bite-checked by swapping the two production strings and renaming one: each test
  fails on its own expected text. Two OTHER tests in the file already pinned
  these same strings exactly, which is the evidence that the two `contains` sites
  were outliers rather than the house style.
- LATERAL, fixed in passing: the `ORDER_FILLED_MAX_BYTES` doc's "Widest shape is
  `OrderFilled`" (above) - the same doc-drift family round 4 spent itself on, in
  the same file, found by measuring rather than by reading.
- NOT CHANGED, deliberately: no document under `docs/` or `reference/` was
  touched. The over-reservation claim lives in `sizing.rs`'s module header where
  it is spent - which the cold review then made the round correct, see below -
  the two-sided rule lives on the helper beside the tests that apply it, and
  neither `reference/architecture.md` nor `docs/oms-types.md` makes a statement
  this round falsified.

COLD REVIEW OF THE ROUND FOUND SIX, all real, all fixed in the same commit. The
first is the EIGHTEENTH instance of this arc's signature defect and a sub-shape
it had not shown before: A FIX THAT TRADES ONE BLIND SPOT FOR ANOTHER while
reading as strictly stronger.

- THE FUNDED REWRITE ABOVE LOST THE CLAIM IT REPLACED. The old
  `balances.is_empty()` said "no row was CREATED"; the new tuple on the USDT row
  says "no funds moved" - and `funded()` seeds exactly ONE currency, so a
  refusal that MINTED a second row is invisible to it. Not hypothetical:
  `sizing.rs`'s own derivation notes the fill path mutates balances through
  `entry(..).or_default()`, so a refusal reaching that path before refusing
  introduces a zero BTC row. `assert_eq!(state.balances.len(), 1)` alongside the
  tuple keeps BOTH claims, and both directions were bite-checked as text edits
  in `on_submit_from` immediately before `validate_submit`: debiting one USDT
  fails the TUPLE at `(9999, 9999, 0)` against `(10000, 10000, 0)` and leaves
  the row count green, and `balances.entry("BTC").or_default()` fails the ROW
  COUNT at 2 against 1 and leaves the tuple green. Neither assertion covers the
  other.
- THE `sizing.rs` MODULE HEADER CONTRADICTED THE CEILING THE SAME CHANGE ADDED.
  It said the fixed addends "are rounded generously upward, because
  over-reserving costs a connection some budget while under-reserving voids the
  bound" - after `brackets`, rounding generously upward is a TEST FAILURE, and
  the account-state fixture has only 208 against 164 of room. The header is what
  a future implementer reads first, so it now states the ceiling, names
  `brackets`, and says round generously WITHIN the factor.
- `ScanKind::hit`'s DISPATCH ASSERTION CLAIMED MORE THAN IT CHECKS. `hit` is a
  three-arm match over exactly the three functions the loop compares it against,
  in the same file, so what it holds is that the arms are not TRANSPOSED - it
  cannot see the engine assigning the wrong `ScanKind` to an order, which is the
  drift the comment advertised. The comment is narrowed to what runs.
- `session_ids_are_the_url_safe_alphabet` DID NOT PIN WHAT ITS LAST LINE SAID.
  `assert_eq!(MAX_SESSION_LEN, 64, "the refusal message states the cap")`
  asserts the CONSTANT against a literal; the production message is its own
  hardcoded "session ids are 1 to 64 characters" and nothing held the two
  together - the durable-prose-asserting-a-live-fact shape, in a string literal.
  It now takes the refusal from an over-length id and asserts it contains
  `MAX_SESSION_LEN`. Bite-checked by editing the literal to 32: fails naming
  both the message and the cap, where the old form passes untouched.
- THE TRUNCATION TEST WAS ASYMMETRIC. `truncate_reason` had its ASCII-overflow
  case cutting exactly AT the cap; `truncate_client_id` had only the at-cap and
  straddling cases, and the straddling cut lands SHORT of the cap, so nothing
  pinned the ceiling `ORDER_EVENT_MAX_BYTES` charges `2 * MAX_CLIENT_ID_LEN`
  for. One line added, bite-checked with `end = MAX_CLIENT_ID_LEN - 1`: fails at
  63 against 64 while the straddling case stays green.
- Style: `validate_client_order_id(&String::new())` reads as though the two echo
  guards had different signatures. `ClientOrderId` is `pub type ... = String`,
  so it is `ClientOrderId::new()` now, which says so.

Findings two through four are ONE FAMILY - a durable statement or a comment
that no longer matches what the code does or checks - and the round's own
subject was assertions weaker than their names. A round that closes that class
produces instances of it in its own prose; the check is to re-read every
sentence the change made false, not only the code.

## The hunter's own ordering

C, A1-A8, B1-B5 and D are all done. The document is empty of open findings; what
remains in it is the two recorded refusals - `read_ready`'s `NoRecord` pause in
section C, and the per-case ceiling in section D's round-5 entry above.

## Close pass, 2026-08-19

CLOSED. The five commits `a3a796d..0e2c32d` were reviewed as one arc against a
green unscoped gate (1220 + 440, 1717 pairs, 1660 run, 57 ignored, 0 orphaned).

All six production changes were re-derived rather than read off the commit
messages: the `shipped_policy` membership gate, both catch-all deletions and the
two exhaustive `EventKind` predicates are behaviour-preserving; `validate()`'s
`trim()` and `account_state_max_bytes` dropping the escape factor are the two
deliberate behaviour changes and each has its premise pinned by a test
(`AccountId::parse` is the only constructor, `Deserialize` routes through it,
and the 0..=0x7f sweep is the whole accepted domain); `launch`'s poll counter
and parameterised interval are additive.

Two bite-checks were re-run as text edits rather than trusted, and both hold
exactly as recorded - the launcher's unconditional reap (fails on the exit
record, poll assertion green, dev and timing sweeps alike) and the zero-hold
reservation filter (fails in both profiles naming `{"USD": 0}`).

ONE FINDING, and it is prose in the unreviewed second half, as on the three
documents before this one. `sizing.rs`'s module header and the `brackets` doc
comment both said EVERY bound in the module is bracketed from both sides. The
composed bounds - `LINKAGE_MAX_BYTES`, `BOUNDARY_REFUSAL_BYTES`,
`swept_fill_max_bytes`, `swept_batch_max_bytes` - are not, deliberately, and
`worst_case_output_bytes` is bracketed on its two query arms only. That is the
same refusal section D records, so the header was describing a gate wider than
the gate. Both statements now scope to the per-struct bounds, enumerate what is
covered, and carry the refusal's reason where an implementer will hit it.

The third residual, carried rather than closed: `" USD "` with surrounding
whitespace is accepted by both validators and matches no balance. Round 2
declined to smuggle the stronger rule in under a finding about blanks, and this
pass agrees - it belongs on both validators at once.
