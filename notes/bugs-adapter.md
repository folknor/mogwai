# Bug hunt: mogwai-adapter

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: the nautilus venue
adapter - `client.rs` and `client/`, `convert.rs`, `clock.rs`, `config.rs`,
`factories.rs`, `lifecycle.rs`, `lib.rs`, and its tests including the four
socket-backed ignore-gated ones. Nautilus semantics claims were cross-checked
against `research/nautilus_trader`.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies.

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
