# Bug hunt: mogwai-adapter

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

## 1. `CurrencyPair::new` is the one panicking constructor left on the wire path - `convert.rs:instrument_any`

Every other conversion in this crate is meticulously routed through `*_checked` with
drop-and-warn, and the sibling `Future` arm uses `FuturesContract::new_checked`. The
`Spot` arm calls `CurrencyPair::new`, whose doc in
`research/nautilus_trader/crates/model/src/instruments/currency_pair.rs` says "Panics
if any input parameter is invalid". `new_checked` runs
`check_positive_price(price_increment)` and `check_positive_quantity(size_increment)`.

Failure: an `/instruments` response with `price_increment = 0` (or any positive
increment that rounds to raw 0 at the advertised `price_precision` - e.g.
`price_increment = 0.001` with `price_precision = 2`) panics. This runs inside
`emit_seeded_instruments` on the `connect()` path and inside the spawned, unsupervised
`request_instruments` / `subscribe_instrument*` tasks. `instrument_any_or_warn`'s doc
claims it "warn[s] loudly on failure instead of swallowing it" - it cannot, because the
failure is a panic, not an `Err`. Fix is one word: `new_checked(...).context(...)`,
matching the futures arm. High confidence.

## 2. `FeedLagged` is silently discarded by both halves

`ServerMessage::FeedLagged { skipped, sim_now_ns }` is the venue's only statement that
it dropped ticks for a lagging client. `client/data.rs:handle_market_message` catches
it in the terminal `_ => {}` arm - no log, no event, nothing.
`client/exec.rs:handle_exec_message` explicitly routes it to `{}` with the comment
"Subscription diagnostics are handled by the data client". They are not handled
anywhere.

`reference/architecture.md` documents that a lagging client "receives `FeedLagged` on
the priority lane and is closed with WS 1011", and `lifecycle.rs` builds a whole
argument on the premise that "mogwai's job is to state the fault as clearly as it can
and decline to repair it downstream" - the adapter reconnects on the 1011 and the
client never learns a data gap happened. The same `_ => {}` also swallows
`HavocDiagnostic` and a data-path `AdmissionRejected`. At minimum `FeedLagged` should
be a `tracing::warn!` naming `skipped`; arguably it should reach the consumer, since a
silent hole in market data is exactly the fault this venue exists to inject visibly.

## 3. `MogwaiExecutionClient::stop()` is a no-op unless `start()` was called - and `connect()` never re-arms it

`ExecutionClientCore::is_stopped()` is literally `!self.is_started()`
(`execution/src/client/core.rs`). `stop()` early-returns on `is_stopped()`. `connect()`
calls only `set_connected()`, never `set_started()`.

So after the first `stop()`, the started flag stays false forever. Sequence
`start() -> connect() -> stop() -> connect() -> reset()`: the second `connect()` spawns
a fresh reader and pump and installs a new `ws_cmd`; `reset()` calls `stop()`, which
early-returns - tasks are not aborted, `ws_cmd` is not cleared, pending waiters are not
drained - and then `reset()` wipes `ExecState` out from under a live, still-streaming
reader. Every subsequent fill/accept hits the A.11 "mirror does not know this order"
warn path and is dropped. Same shape for a host that calls `connect()`/`disconnect()`
without ever calling `start()`: the reader reconnects forever after disconnect. The
canonical shutdown order (`disconnect_clients()` then `finalize_stop()`) happens to be
safe, which is why this has not bitten. `MogwaiDataClient::stop()` has no such guard and
is correct.

## 4. `connect()` is not idempotent on either client - double readers on the shared `connected` flag

Neither `connect()` aborts previously tracked tasks before spawning. Call `connect()`
twice without an intervening `stop()` and you get two `run_ws_connection` tasks, two
sockets, two latency pumps, all writing the same `Arc<AtomicBool> connected` and the
same sink/mirror. `wait_connected` then returns instantly because the old reader already
set the flag, so the second connect "succeeds" while the duplicate is invisible. Every
tick and every exec event is delivered twice; the exec mirror's `seen_trades` dedups the
mirror but the duplicated `OrderFilled` still reaches nautilus.

The hazard is already named in the timeout arm's own comment ("a retry would spawn a
second reader racing the first") but the guard was only applied to the timeout path.
`connect()` should begin with `abort_tasks(&self.task_handles); self.ws_cmd = None;
self.connected.store(false, ...)`.

Related, smaller: on the timeout path only the reader is aborted (`handles.pop()`); the
latency pump spawned just above it stays alive holding the sink/exec context.

## 5. `register_ws_query` leaks its waiter on send failure - the doc says otherwise

`client/exec.rs`: "unregister on a send failure so a dead socket does not leak the
entry." The code registers, then `tx.send(cmd).context(...)?` - the `?` returns without
unregistering. The oneshot sender stays in `PendingQueries` until `stop()` drains the
map. Small leak, but the comment asserts a behaviour that is not there, which is worse
than the leak.

## 6. `order_status_report_from_info` drops the venue's `position_id`

`OrderStatusInfo` carries `position_id: Option<String>`; `OrderStatusReport` has
`with_venue_position_id` (`model/src/reports/order.rs`) and the adapter never calls it.
In `reconciliation/orders.rs:reconciliation_position_id`, a missing `venue_position_id`
falls back to `PositionId::new(format!("{}-EXTERNAL", instrument.id()))`. Under
`OmsType::Hedging` - which `MogwaiExecClientConfig::with_oms_type` explicitly supports -
every reconciled order collapses onto one synthetic `-EXTERNAL` position instead of the
venue's actual per-position ids. The fill path already does this correctly
(`fill_report_from_wire` sets `position_id`), so it is an inconsistency, not a design
decision.

## 7. Partially-filled open orders reconcile with no `avg_px`, and mass status cannot always pair fills

`order_status_report_from_info` never sets `avg_px` (the wire `OrderStatusInfo` has no
such field). `resolve_fill_price` (`reconciliation/orders.rs`) falls back
`avg_px -> report.price -> order.price()`. For a market order the residual inferred fill
has no price at any rung and nautilus logs "Cannot determine fill price ... no avg_px,
report price, or order price" and voids it.

This is normally masked because `generate_mass_status` supplies fill reports that get
paired by `venue_order_id`. But the two generators use asymmetric filters, deliberately:
order reports under `open_only` bypass the `[start, end]` window entirely (AE10), while
`generate_fill_reports` applies it. A long-resting partially-filled market remainder
whose fills predate `lookback_mins` therefore arrives as an unpaired status report with
`filled_qty > 0` and no price source. The venue-side fix is to add `avg_px` to
`OrderStatusInfo`; the adapter-side mitigation is to drop the time filter on fills whose
`venue_order_id` appears in the order reports.

## 8. `fetch_trades_windowed` silently loses same-nanosecond trades at a page boundary

`next = page.last().ts_event + 1`. If a full page ends mid-nanosecond, every remaining
trade at that `ts_event` is skipped and the loop continues - `truncated` stays `false`,
so nothing is reported. The `request_trades`/`request_bars` warnings literally say
"(trade limit reached or same-ts wedge)", but no wedge detection exists; the wedge case
never sets the flag it is named in. Either detect it
(`page.first().ts_event == page.last().ts_event` on a full page) or drop the phrase.
`fetch_quotes_windowed` has the identical cursor and returns no truncation signal at
all.

## 9. `account_id` config machinery is vestigial and its documentation is false

`config.rs:validate_account_id` refuses `UNSET_ACCOUNT_ID` with a 12-line rationale: "it
names WHICH server-side account slot the socket binds to ... server-owned divergence
windows (`StallData`, `GoDark`, the delay atomics), which live on that slot ... silently
miss the data feed entirely." `reference/architecture.md` says "clients do not subscribe
or supply an account identity", and `grep account crates/mogwai-server/src/ws.rs`
returns nothing - the venue's WS handler has no account concept.
`exec.rs:note_account_label` already documents this ("one venue is now one run is one
LEDGER"), so the retirement is known; the config doc was not updated.

Both `ws_url()` implementations carry the same stale comment: "`ws_url` already carries
the `/ws` path and the account query; do not join a path onto it", warning about
`ws://host?account=X/ws`. `ws_url()` produces `format!("{}/ws", ...)` - there is no query
string and the described hazard cannot occur. The comment is repeated verbatim at both
`connect()` call sites.

The field itself is still needed (nautilus requires an `AccountId`), but the hard refusal
is now enforcing a venue invariant that no longer exists, and `for_addr`'s "pass the SAME
account_id to both" advice is no longer load-bearing.

## 10. Dead mirror state maintained with elaborate care

`OrderRecord` keeps `quantity`, `price`, `filled_qty`, `avg_px`, `ts_accepted`. Since the
report generators moved to venue truth, none of these five is ever read. The fill handler
still does checked-mul/checked-div notional accumulation to maintain `avg_px`, and the
`OrderUpdated` arm still reconciles `filled_qty = (quantity - leaves_qty).max(0)` - for a
value nothing consumes. The live readers are only `strategy_id`, `instrument_id`,
`order_side`, `order_type`, `status`, `venue_order_id`, `ts_last`, `seen_trades`.

This is a genuine simplification: cut the five fields and the arithmetic goes with them.
The one caveat - and it is the better fix - is that `avg_px` is precisely what finding 7
needs; either wire it into `with_avg_px` on the report path, or delete it. Keeping it
computed and unread is the worst of both.

## 11. `FuturesContract` hard-codes `size_precision = 0`; the adapter converts against the wire's

`futures_contract.rs` sets `size_precision: 0, size_increment: Quantity::from(1)`
unconditionally - the constructor takes neither. But `handle_order_filled`, `trade_tick`,
`acc_to_bar` and every report generator convert quantities at `def.size_precision` from
the wire. For a futures `InstrumentDef` with `size_precision > 0`, the adapter emits
`Quantity` values whose precision disagrees with the instrument nautilus has cached. No
live impact (MNQ is precision 0) but nothing enforces it. Either assert
`size_precision == 0` for `InstrumentClass::Future` at conversion, or use the nautilus
instrument's precision rather than the wire def's throughout.

## Smaller notes

- `retain_quote` does `states.entry(quote.symbol.clone()).or_default()` and caches the
  full quote for any inbound symbol, subscribed or not. The row is then never retired
  (`unsubscribe_symbol` only removes rows with `cached_quote.is_none()`), so `subs` grows
  one resident `QuoteTick` per distinct wire symbol, unbounded. One-instrument runs make
  this harmless today; a hostile or multi-symbol server makes it a leak.
- `record.filled_qty += fill.last_qty` is the one unchecked `Decimal` arithmetic in
  `handle_order_filled` - everything around it uses `checked_*`. `Decimal` `+=` panics on
  overflow. Bounded in practice by the `QUANTITY_MAX` check on `last_qty`, so it needs a
  great many fills; moot if finding 10 is taken.
- `WsCommand` in `data.rs` is an uninhabited enum. The whole `ws_cmd` channel,
  `ws_command_to_client_message`, and the `cmd_rx` plumbing on the data socket are dead -
  the data client can never send a frame. Worth deleting outright and giving
  `run_ws_connection` a no-command specialization, rather than threading an unbounded
  channel that structurally cannot carry anything.
- `warn_missing_instrument_once` uses a process-global `OnceLock<HashSet<String>>`. Two
  clients (or two test runs in one process) against different venues share the dedup set,
  so the second one's genuine black-holing is never reported. Acknowledged in the doc
  comment; flagging that "process-global" is a stronger claim than "per-client" in a
  crate whose own test binaries run several clients per process.
