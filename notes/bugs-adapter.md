# Bug hunt: mogwai-adapter

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

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
