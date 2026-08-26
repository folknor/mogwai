# Bug hunt: mogwai-adapter

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: the nautilus venue
adapter - `client.rs` and `client/`, `convert.rs`, `clock.rs`, `config.rs`,
`factories.rs`, `lifecycle.rs`, `lib.rs`, and its tests including the four
socket-backed ignore-gated ones. Nautilus semantics claims were cross-checked
against `research/nautilus_trader`.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies.

The hunter ranks 1 and 2 first - both are silent order-state desyncs with a
comment asserting a safety they do not have. 3 and 6 are clear omissions
against a documented sibling. 5 silently produces wrong numbers rather than
wrong states.

## 1. The `OrderRejected` arm has no terminal-state guard, and two comments claim it does

Vacuous-gate family. High confidence.

`client/exec.rs`, `handle_exec_message`'s `VenueMessage::OrderRejected` arm
mutates unconditionally:

```rust
record.status = OrderStatus::Rejected;
record.ts_last = UnixNanos::from(ts_event);
```

with a comment arguing this is safe because "the engine emits `Rejected` as an
order's sole lifecycle event, so no reordered pair can arrive to regress a later
terminal state."

That premise is false for three of the four call sites that reach this arm, all
of which synthesize an `OrderRejected` rather than receiving the engine's own:

- `AdmissionSubject::SubmitGroup` fan-out, which re-enters
  `handle_exec_message` once per leg;
- `AdmissionSubject::Submit`, likewise re-entered;
- `synthesize_transport_reject`'s `SubmitGroup` branch, one per leg.

And `ExecState::peek_group`'s doc states the opposite outright: "The duplicate
rejection it now permits is absorbed by the terminal-state guard in
`handle_exec_message` - an order already `Rejected` does not move."

There is no such guard. Two consequences, both real:

- Duplicate group refusal. `peek_group` was deliberately changed to leave the
  row in place because `duplicate_prob` and `DuplicateNextFill` exist to deliver
  the same `AdmissionRejected` twice. The second copy now re-emits a full
  `OrderRejected` per leg to nautilus. Nautilus' order FSM has no `(Rejected,
  Rejected)` arm, so each duplicate produces an invalid-transition log - the
  exact noise the `peek_group` change was made to avoid, moved from
  `tracing::error!` into nautilus' own log.
- Reordered or late group refusal regressing a live order. A `SubmitGroup`
  refusal delivered behind the legs' accepts and fills - reorder havoc holds one
  message and releases it behind the next arrival - walks every leg's mirror
  from `Filled` or `Accepted` back to `Rejected`, and emits `OrderRejected` for
  an order the venue actually filled. The mirror is the reconciliation truth
  source, so it now confidently reports a filled order as rejected.

The cheap fix is the same terminal guard every sibling arm carries; the comment
is then either true or deleted. Per the doctrine's blast-radius rule, the guard
has to be on the arm, not on the group fan-out, because
`synthesize_transport_reject` reaches it by a different path.

## 2. `stop()` and `connect()` abort the reader, which defeats the entire undelivered-command receipt book

High confidence.

`lifecycle::run_ws_connection` reports swallowed commands in two places, both
after the connection loop returns: the `unwritten` residue drain, and the
`rx.close(); while try_recv -> on_undelivered` at the tail of the outer
wrapper. Neither is reachable when the task is aborted.

Both clients abort it:

- `MogwaiExecutionClient::stop()` calls `abort_tasks(&self.task_handles)`,
  which holds the reader handle;
- `MogwaiExecutionClient::connect()` does the same teardown at its top, and
  again on its two failure paths.

So a `Submit`, `Cancel` or `Modify` queued in the window before a `stop()` or
reconnect is dropped with no synthesized reject: the order wedges in
`Submitted`, `PendingCancel` or `PendingUpdate` forever, in both nautilus and
the mirror. This is exactly the AE9 wedge that `on_undelivered` was built to
close, reopened through the lifecycle path the receipt book cannot observe.
`stop()` clears `pending` queries for precisely this reason ("so a waiter
blocked on a dead socket errors out instead of waiting out its timeout"); the
order commands got no equivalent.

`reconciliation.rs::reconnect_after_stop_can_be_stopped_again` exercises
stop-then-connect but asserts only on socket count, so this is invisible to the
suite.

Structurally, the right fix is probably not another drain in `stop()` but
making the residue reporting run on drop: the receipt book and the command
receiver both belong to the connection future, so an `AbortOnDrop`-style guard
that reports on unwind is the only shape that survives cancellation. That is a
real, small rewrite of `run_ws_connection`'s ownership, and the hunter would
take it over another compensating drain.

## 3. `MogwaiDataClient::stop()` is missing the exec side's teardown, in three ways

High confidence on the mechanism.

Compare `exec::stop()` - clears `ws_cmd`, aborts, retires the flag, drains
`pending`, clears `groups` - against `data::stop()`, which aborts, retires the
flag and flushes bars. Missing:

- `pending_history` is never drained. A `request_bars`, `request_trades` or
  `request_quotes` session in flight when `stop()` lands has its reader aborted,
  so no reply and no `on_undelivered` (see finding 2) will ever arrive. It parks
  for the full havoc-scaled request timeout - 30 s by default - and then answers
  empty. The exec side's identical hazard is closed by clearing `pending`; this
  one is not, and `collect_until` even has the fail-fast arm (`Ok(Err(_)) =>
  "history request abandoned: the data client stopped"`) sitting there
  unreachable from `stop()`.
- `ws_cmd` is never cleared - not by `stop()`, nor by either of `connect()`'s
  failure paths, both of which the exec twin clears. So `history_session()`
  returns `Ok` against a dead generation and the failure surfaces as "history
  request not sent: the data socket is gone" from inside a spawned task rather
  than as a synchronous "not connected".
- `stop()` also does not clear it before the abort, so there is a window where
  the abort has landed and a new `request_*` can still enqueue onto a receiver
  that is being dropped.

## 4. A held reorder message can wedge an order indefinitely

Medium confidence - possibly intended, but undocumented.

`HavocFilter::apply` holds a message when the reorder draw fires and releases
it only when the next message arrives on that socket, or at `flush()` on
disconnect. On the execution socket, message arrivals are order-driven, not
tape-driven: the exec leg discards the market tape. So if the held frame is an
order's terminal event - a `Filled`, a `Canceled` - and the strategy then does
nothing further, nothing arrives to displace it, and the order sits unresolved
in nautilus for as long as the socket stays up. Heartbeats do travel this
socket, which would release it, but only if `heartbeat_interval_ms > 0`, and
that is the venue's cadence rather than a guarantee this filter can name.

The data leg is immune - the tape keeps flowing. The exec leg's exposure is not
mentioned anywhere the hunter found. If it is intended, it wants saying in
`HavocFilter`'s doc; if not, the hold wants a deadline.

## 5. `Forex` instruments are silently flattened onto `CurrencyPair`, dropping the multiplier

High confidence that data is lost; medium on impact.

`convert::instrument_any` matches `InstrumentClass::Spot { base, quote } |
InstrumentClass::Forex { base, quote, .. }` into the same `CurrencyPair`
construction. The `Forex` variant carries `multiplier`, `pip_size`,
`point_size`, `rollover_minute_utc`, `swap_long`, `swap_short`; all six are
discarded by the `..`. Nautilus' `CurrencyPair` has no multiplier, so it
computes notional at an implicit 1. A leveraged-FX preset with a standard-lot
multiplier therefore reaches the host with its notional and P&L wrong by that
whole factor, silently - the venue's ledger and the host's disagree by
construction.

`Equity` had exactly this problem and it was solved: the venue's un-mappable
facts are preserved in the `Params` info bag (`mogwai_borrowable`,
`mogwai_settlement_ns`). `Forex` got no such treatment, and it is the one class
where the dropped field changes an arithmetic result rather than a policy.
There is no `Forex` test in `convert.rs`'s test module either - the class is
entirely unexercised at this seam.

`Perpetual`'s four funding fields (`funding_interval_ns`, `funding_rate`,
`index_symbol`, `funding_clamp`) are dropped the same way. Lower impact -
nautilus' `CryptoPerpetual` has nowhere to put them, and `DataEvent::FundingRate`
exists as a separate channel - but the loss is equally silent.

## 6. `request_quotes` does not pin its window's `end`; `request_trades` and `request_bars` do

Medium-high confidence.

`request_trades` and `request_bars` both compute:

```rust
let end = date_to_unix_nanos(request.end)
    .or_else(|| self.data_origin_ns.is_some().then(|| now_unix_nanos(sim)));
```

with a long comment explaining why: the venue clamps each page against the run
clock at admission, so an unpinned window is re-cut against a later present on
every page and the logical window grows as it is fetched. `request_quotes` in
`data.rs` does:

```rust
let end = date_to_unix_nanos(request.end);
```

Same paging session, same `collect`, same venue. Either the quote path has the
growing-window bug the other two document, or the comment on the other two is
wrong about all three. Nothing states an exemption. The hunter believes this is
a straightforward omission when the pin was added.

## 7. `generate_order_status_report` does not verify the returned row is the order it asked for

Medium confidence - currently latent.

The singular generator passes `cmd.client_order_id` into the query and then
filters the reply by `venue_order_id` and `instrument_id` locally, but never by
`client_order_id`. `query_order` is looser still: `snapshot.orders.first()`, no
check at all. Both then hand the report to the execution manager as the answer
for the probed order.

The engine does filter correctly today (`order_status_snapshot`,
`client_order_id` match, and a targeted query correctly ignores `open_only`; the
test stub in `tests/common/mod.rs` reproduces both, so the double is honest
here). So this is not a live bug. But it means the adapter's in-flight-probe
correctness rests entirely on the venue's filter, on the one path where a wrong
answer resolves a different order's in-flight state. The other two locally
re-applied filters show the intent was to re-check; `client_order_id` is the one
that matters most and is the one omitted.

## 8. `dial_timeout_secs` is unvalidated; zero disables every dial

Medium confidence, low severity.

Neither config's `validate()` touches `dial_timeout_secs`. Zero produces
`tokio::time::timeout(Duration::ZERO, connect_async(..))`, which fails
immediately on every attempt, and `wait_connected(.., Duration::ZERO)`
likewise. The result is a client that walks the reconnect ladder to exhaustion
having never opened a socket, reporting "no upgrade within 0ns; the venue may
still be materializing this river" - a message that points at the venue for a
purely local config error. Every other knob in these configs is validated
against the venue's own rule specifically so this class of failure surfaces at
config time; this one slipped.

## 9. Smaller things

- `ExecutionClient::reset()` uses a bare `lock()` where the whole file uses
  `lock_recover` (`exec.rs`). `commit_submitted`'s doc makes the case at length
  that refusing work over a poisoned mirror mutex is worse than recovering it;
  `reset()` is the one site that still returns "execution state mutex poisoned"
  and leaves the mirror populated - which is precisely the
  leak-into-the-next-passenger state `reset()` exists to prevent. It is
  inconsistent with the ruling written directly above it.
- `task_handles` pruning is insert-time only. `track_task` retains only
  unfinished handles on each insert, so the vec is bounded by in-flight tasks at
  insert time. `query_order` and the `request_*` handlers are the
  high-frequency inserters, so this is fine in practice, but a long run whose
  last spawn was hours ago holds every finished handle until the next one.
  Cosmetic.
- `HavocFilter::emit_candidates` clones the whole `VenueMessage` on every
  message (`out.push((msg.clone(), delay))`) purely so the duplicate draw might
  need a second copy. On the data socket that is a clone per tick on the hot
  path for a branch that is not taken. Restructure so the clone happens inside
  the `duplicate` branch.
- The four socket suites are ignore-gated and `brokkr check` does not run them,
  which is documented and accepted - but findings 2 and 3 both live exactly in
  the stop/reconnect seam those suites cover, and neither suite asserts on
  command or waiter fate across a stop. The coverage gap here is not the ignore
  gate; it is that the existing socket tests assert on socket counts rather than
  on what happened to in-flight work.

Hunter's sanity check: every finding was read at its site in the current tree,
not transcribed; the two cross-repo semantics claims that mattered were verified
- `open_only` being ignored for a targeted query, where the stub matches
`mogwai_engine::order_status_snapshot`, and `ExecutionEventEmitter`'s
`set_sender` / `is_initialized` / `send_order_status_report` surface in
`research/nautilus_trader/crates/live/src/execution/emitter.rs`.

## 10. Handed over from the protocol/CLI arc: `wire_submit` forwards a host-stated price on a Market order into a new wire refusal

Filed by the protocol/CLI close pass, 2026-08-26. Low severity, decide rather
than assume.

The wire now refuses a consumer-supplied `price` on a `Market` order
(`SubmitPhase::PreStamp`, "Market order must not carry a price"). `wire_submit`
in `client/exec.rs` maps `init.price` through for every order type except
`TrailingStopLimit`, so a host handing the adapter an `OrderInitialized` with
`order_type` Market and a price now earns a boundary refusal - translated to a
named `OrderRejected` - where it previously earned a fill at that price.

Nautilus's own `MarketOrder` cannot carry a price through the supported
constructor, so this is unreachable on the supported path, and the protocol/CLI
round left it alone deliberately: a named refusal beats silently dropping the
price, and a defensive drop would hide a host bug. The adapter round should
either ratify that ruling in `wire_submit`'s doc, which currently says nothing
about the Market case while the `TrailingStopLimit` arm documents its drop at
length, or argue for the drop with the same care that arm did. What must not
happen is a later reader "fixing" the refusal by silently dropping the price
without meeting the round-1 argument.
