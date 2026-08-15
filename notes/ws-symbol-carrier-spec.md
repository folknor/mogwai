# Implementation spec: the `/ws` symbol carrier (piece 6)

Written against `reference/technical-implementation-spec.md`, which is the
contract this document is judged by. Spawned from `notes/todo.md`: piece 6 of
"Landing the grand design: fourteen pieces", and the design bullets that
inventory points to under Open issues - "THE SYMBOL IS A REQUEST PARAMETER, NOT
AN IDENTITY THE VENUE OWNS" (its item 6), "THE RIVER AND THE BOAT", "SYMBOL
RESOLUTION IS TOTAL, AND THE DEFAULT PRESET IS THE SHAPE CONTRACT", and the
ruling recorded as piece 4 (slice 1 keeps a boot symbol).

## 1. What this lands, in one paragraph

Today a `/ws` connection carries no symbol at all: `handle_socket` attaches
every socket to the run's single tape on upgrade, `mogwai-protocol` pins the
absence of any subscribe frame with a byte-level test, and the only
request-carried symbol in the whole serving path is `HistoryQuery.symbol` on
`/trades` and `/quotes`. This spec creates the missing carrier as a QUERY
PARAMETER ON THE UPGRADE - `GET /ws?symbol=MNQ` - resolves it through the same
same exact, case-sensitive resolution the history endpoints use, refuses at the
HTTP layer before the upgrade when the run cannot serve it, and threads the
resolved symbol through the connection as an owned per-connection value that
every symbol decision on that socket reads. The boot symbol survives as the
DEFAULT for a socket that names none, so every existing client keeps working;
the process-global `RunIndex`/`BOOT` singletons are untouched, which is why a
symbol other than the boot symbol is REFUSED rather than served in this landing.

**This landing does NOT deliver total resolution, and the paragraph above must
not be read as claiming it.** What lands is the CARRIER: the parameter, its
parsing, its validation, its refusal and its threading. The resolution behind
the carrier stays a one-symbol resolution until piece 7 keys the run state per
river; a socket naming any symbol other than the one this run booted gets a
`400`. That restriction is temporary and deliberate - refusing loudly is the
only alternative to serving a permanently empty tape - but it is a
COMPATIBILITY RESTRICTION, not the settled model, and every piece of prose this
landing writes says so in those terms.

## 2. Why a query parameter and not a frame

The choice is forced by four facts in the tree, not by taste:

1. **A connection owns exactly one river.** `ADMISSION_PROMISE_TICKETS = 1` is
   derived in `admission.rs` from "one connection owns one unconditional
   replay". A subscribe FRAME invites N replays per socket and therefore
   re-opens the promise-pool sizing, the per-connection lane budgets and the
   `FeedLagged` accounting, none of which piece 6 wants to touch. A query
   parameter keeps the derivation literally true - the sentence changes from
   "attached to the run's single tape" to "attached to the one river the
   upgrade named" and the constant stays 1.
2. **The sharing key must be known before any bytes flow.** Piece 9 places a
   boat when the first subscriber arrives. If the symbol arrives as a frame,
   the socket exists for an interval during which it is attached to nothing,
   and the writer, the exec pump and the heartbeat all start before the venue
   knows what they are for. On the upgrade the key is known at `ws_upgrade`,
   before `handle_socket` allocates a single task.
3. **A refusal must be a status, not a close.** An unserved symbol on `/trades`
   is `400` with a body naming what IS served. A frame-carried symbol can only
   be refused by a WS close code after a successful upgrade, which is exactly
   the "looks like an outage" ambiguity `CLOSE_VENUE_FAULT`'s doc comment
   fights. `axum::extract::Query` rejection and our own refusal both answer
   `400` before the 101.
4. **The absence is byte-level pinned, deliberately.** The protocol test
   asserts `Subscribe` and `Unsubscribe` FAIL to deserialize. Re-adding a
   frame means un-pinning a deliberate retirement and re-litigating the
   subscription model that was removed. A query parameter leaves
   `ClientMessage` untouched, so the wire-frame surface does not move at all
   in this landing.

The cost, stated so it is not discovered later: a client that wants two symbols
opens two sockets. Under "STRATEGIES ARE SINGLE-INSTRUMENT by settled premise"
and "no observer ever holds two clocks", that is the intended shape, not a
workaround.

## 3. Survey of the ground

Everything that must move, with what it does today.

**`crates/mogwai-server/src/ws.rs`**
- `ws_upgrade(ws: WebSocketUpgrade, State(state))` sets message/frame size caps
  and calls `on_upgrade(move |socket| handle_socket(socket, state))`. No other
  extractor, so no request data reaches the socket today.
- `handle_socket(socket, state)` builds the lanes, the command dispatcher, the
  writer, binds `state.run.bind_lanes`, spawns the exec pump, and then does
  `state.run.tape.subscribe_with_snapshot()` UNCONDITIONALLY. That
  `subscribe_with_snapshot` call is the single place a connection is attached
  to market data; it is the seam piece 9 later replaces with a boat lookup.
- `dispatch_command` -> `process_order_cmd(cmd, state, &state.run, lanes)`
  carries no per-connection context whatsoever.

**`crates/mogwai-server/src/http.rs`**
- `history_symbol_refusal(symbol, profiles)` is the landed slice-1 refusal:
  `profiles.get(symbol)` decides, an over-long requested symbol is truncated to
  64 chars for the echo, a `warn!` is logged, and the body reads
  `requested symbol {echoed} is not served by this run; this run serves
  {served}` from `profiles.served_symbols()` (sorted, for deterministic text).
  This is the exact refusal the `/ws` carrier must reuse - two spellings of
  "unserved" would be two behaviours. Its doc comment states the predicate is
  binding: the guard "must never be a case-insensitive or otherwise looser
  comparison: a guard that admits what the synthesis misses restores the silent
  empty page", and `history_symbol_refusal_uses_the_synthesis_lookup` pins that
  with `history_symbol_refusal("btcusdt")` being `Some`. Reusing the WORDING
  while loosening the PREDICATE would be the same two-behaviours defect wearing
  one message, so `/ws` matches case-exactly too (4.2 step 3).
- `InstrumentProfiles` holds EXACTLY ONE profile today.
  `build_instrument_profiles` validates every `[symbols.*]` table and then
  deliberately drops all but the boot profile - "the point is the refusal". So
  `profiles.get(non_boot)` is `None` for every symbol this run did not boot,
  `served_symbols()` is a one-element list, and `serve.rs`'s
  `instrument_defs().next()` picks the sole profile rather than the
  alphabetically first of several. Piece 7 is what makes that map plural.
- The order path resolves an amend's symbol as
  `Some(Arc::clone(&state.run.instrument.symbol))` for `CancelOrder` /
  `ModifyOrder`, with a comment reading "A run is one instrument
  (`Run::instrument`)". That is the run-level singular assumption inside the
  per-connection path, and it is the one this spec converts to a
  per-connection read.
- `instruments()` answers `vec![state.run.instrument.clone()]`, with a comment
  about not letting a consumer "come to believe a second symbol is
  subscribable". That doc comment becomes false the moment a subscribe carrier
  exists and is corrected here; the ENDPOINT's shape is piece 13's, not this
  spec's.

**`crates/mogwai-server/src/serve.rs`**
- `profiles.instrument_defs().into_iter().next()` picks the boot instrument;
  `Run::new` takes it; `materialize_warmup` initializes `INDEX` from it at
  boot; `ReadyRecord.symbol` reports it. All of this stays, per the piece-4
  ruling.

**`crates/mogwai-server/src/config.rs`**
- `profile_for(cfg, symbol: Option<&str>) -> InstrumentProfile` is documented
  in-tree as "the seam slice 2 needs: when the symbol arrives per request, the
  server calls it with the requested symbol and nothing else changes". This
  spec does NOT call it per request - resolution against a live run needs the
  keyed state piece 7 builds - but the carrier's refusal is written so that
  swapping `profiles.get` for `profile_for` in piece 7 is a one-expression
  change at one call site.
- `boot_symbol()` and `MAX_SYMBOL_LEN = 32` (in `mogwai-protocol`) are the
  validation inputs.

**Clients of `/ws`, all of which must keep passing unchanged or be updated
here**
- `crates/mogwai-adapter/src/config.rs`: both `ws_url()` bodies are
  `format!("{}/ws", base_url.trim().trim_end_matches('/'))`.
- `crates/mogwai-adapter/tests/common/mod.rs` stub routes on
  `path.starts_with("/ws")`, so a query string is already tolerated by the
  double.
- `crates/mogwai-cli/tests/common/mod.rs` builds `format!("{}/ws", base_url)`.
- `scripts/smoke.py` hand-writes `GET /ws HTTP/1.1`.

**Not touched, and named so the stopping rule is unambiguous** - `RunIndex`,
`BOOT`, `RunSeeds`, the `.next()` collapse, `MarketReadingCache`,
`last_swept_ns`, `Tape`, the sweeper, `/clock`, `ReadyRecord`, the engine, the
adapter's subscription guard, `/instruments`' response shape.

## 4. The target artifacts

### 4.1 `SocketRequest` - the parsed carrier (`ws.rs`)

```rust
/// The upgrade's query string, exactly as the client wrote it.
///
/// `deny_unknown_fields` is a WIRE-COMPATIBILITY decision, taken knowingly:
/// pieces 9 and 10 add `speed` and `duration_ms` here, and until they do, a
/// client that sends one is REFUSED rather than silently served a different
/// river than it asked for. The price is that ANY unrecognized key is a `400`,
/// including one an unrelated client, proxy or tracing layer appends, and
/// including a future key added before its handling lands. That is accepted:
/// accepted-and-ignored is the failure mode this carrier exists to prevent,
/// and the venue's clients are its own. Relaxing it later is a wire change
/// that owes its own reasoning, not a tidy-up.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SocketQuery {
    /// Absent means "the run's boot symbol", which is what every client that
    /// predates this carrier sends.
    #[serde(default)]
    symbol: Option<String>,
}
```

```rust
/// What one connection is bound to, decided before the upgrade completes and
/// owned by the socket for its whole life.
///
/// One field today. It exists as a struct rather than a bare `Symbol` because
/// the boat placement (piece 9) and the per-boat clock (piece 10) attach here,
/// and because every downstream signature then changes exactly once.
#[derive(Debug, Clone)]
pub(crate) struct SocketSession {
    pub(crate) symbol: mogwai_protocol::Symbol,
}
```

### 4.2 Resolution and refusal (`http.rs`)

`history_symbol_refusal` is renamed `unserved_symbol_refusal` and its doc
comment restated as "the ONE unknown-symbol decision every request-carried
symbol makes", with `/trades`, `/quotes` and now `/ws` as its three callers.
Body text, truncation and log line are unchanged, so the landed refusal
transcript does not move.

New, next to it:

```rust
/// The symbol one `/ws` upgrade binds to, or the refusal body explaining why
/// this run cannot serve it.
///
/// TOTAL resolution is the model (`notes/todo.md`, "SYMBOL RESOLUTION IS
/// TOTAL"), and it is NOT reachable yet: `source::RunIndex` is a process-global
/// holding one symbol, so a second symbol would resolve to a profile and then
/// read `None` from every index lookup - silently, with no error and no log.
/// Until piece 7 keys that state per river, a symbol this run did not boot is
/// refused HERE, loudly, rather than served as a permanently empty tape.
pub(crate) fn resolve_socket_symbol(
    requested: Option<&str>,
    bound: &mogwai_protocol::Symbol,
    profiles: &source::InstrumentProfiles,
) -> Result<mogwai_protocol::Symbol, String>
```

It takes `bound: &Symbol` - the run's symbol - and NOT `&Run`. The function
reads exactly `run.instrument.symbol` and nothing else, while `Run::new` takes
fifteen arguments and spawns a `Tape` background task; taking the whole run
would make section 6's "unit tests, no socket" plan a fiction. The call site in
`ws_upgrade` passes `&state.run.instrument.symbol`.

Behaviour, in order:
1. `None` -> `Ok(Arc::clone(bound))`. The boot default.
2. Blank after `trim`, or longer than `MAX_SYMBOL_LEN`, or containing any byte
   outside `[A-Za-z0-9._-]` -> `Err` with
   `requested symbol {echoed} is not a legal symbol; symbols are 1 to 32
   characters of ASCII letters, digits, dot, dash or underscore`, echo
   truncated by the same 64-char rule.

   The rule is stated on the DECODED value, and that is the whole content of it
   server-side: `axum::extract::Query` percent-decodes before serde ever sees
   the field, so `?symbol=%4DNQ` arrives as `MNQ` and passes, exactly as it
   should. The server's rule is therefore "the decoded symbol is ASCII-safe",
   not "the symbol needed no encoding" - the latter is unobservable here. The
   needs-no-encoding framing belongs solely to the client-side
   `validate_wire_symbol` (4.5), where the URL is built by concatenation and an
   unencoded byte really is the difference between a config typo and a
   malformed request nobody can read in a log.

   The `trim` is for the blankness test ONLY, and the charset check rejects the
   space character, so an input with any leading or trailing whitespace is
   refused by step 2 regardless. Consequently steps 3 and 4 compare the
   REQUESTED value verbatim, never a trimmed copy: there is no input that
   reaches them for which the two differ, and stating which one is compared
   removes the ambiguity rather than relying on that.
3. EXACT, case-sensitive comparison against `bound` -> `Ok(Arc::clone(bound))`.
   Case-exact, not case-folded, and this is load-bearing: `/trades?symbol=mnq`
   is a `400` on an `MNQ` run by a landed test and a doc comment that calls the
   looseness out by name, so a case-folding `/ws` would make `?symbol=mnq`
   succeed on a socket whose history fetches for the same string are refused -
   two behaviours under the one wording section 3 forbids. The `overlays_for`
   precedent does not carry: that is config-TABLE matching at load time, not
   the synthesis lookup a request is judged by. If case-insensitive request
   symbols are ever wanted, both endpoints change together, deliberately, in
   their own change.
4. Otherwise -> `Err`, with `unserved_symbol_refusal(requested, profiles)`'s
   body when that returns `Some`.

   `unserved_symbol_refusal` returns `Option<String>` because its predicate is
   `profiles.get`, and today that predicate cannot disagree with step 3:
   `build_instrument_profiles` keeps only the boot profile, so a symbol that
   fails step 3 also misses `get` and the `Option` is always `Some` here.
   The function must NOT rely on that. `Result<_, String>` has nothing to put
   in an `Err` if the `None` arm is ever reached, and piece 7 is precisely the
   change that makes the map plural and the arm live. So step 4 is written with
   an explicit fallback body for the `None` case from the outset -
   `requested symbol {echoed} is configured but is not the river this run
   booted; this run serves {bound}` - which names what the socket can actually
   bind rather than listing symbols it provably cannot. A test drives that arm
   directly by handing the function a `bound` outside a two-entry
   `InstrumentProfiles` built via `from_profiles`, so the branch is covered
   before piece 7 makes it reachable through the config.

Steps 3 and 4 are what piece 7 rewrites: it replaces the boot-symbol comparison
with the profile resolution plus a per-river index lookup, and the step-4
fallback body becomes the ordinary refusal. Nothing else in this file moves
again.

### 4.3 The upgrade (`ws.rs`)

```rust
pub(crate) async fn ws_upgrade(
    ws: WebSocketUpgrade,
    axum::extract::Query(query): axum::extract::Query<SocketQuery>,
    State(state): State<AppState>,
) -> axum::response::Response
```

- Extractor order here is CONVENTION, not a constraint. All three -
  `WebSocketUpgrade`, `Query` and `State` - are `FromRequestParts`, and only a
  `FromRequest` (body-consuming) extractor is position-constrained in axum, so
  any order compiles. Write them upgrade, query, state anyway, to match the
  other handlers; do not justify the order as a rule, because a later reader
  will act on that as though moving an argument could break the handler.
- On `Err` from `resolve_socket_symbol`, return
  `(StatusCode::BAD_REQUEST, body).into_response()` - no upgrade, no socket, no
  tasks spawned.
- On `Ok(symbol)`, build `SocketSession { symbol }` and
  `on_upgrade(move |socket| handle_socket(socket, state, session))`.
- A malformed query string (unknown key, repeated key) is rejected by axum's
  own `Query` rejection as `400` with its own body. That is acceptable and
  intentional: the venue's own refusals are the ones whose text is a contract.

### 4.4 Threading the session

- `handle_socket(socket: WebSocket, state: AppState, session: SocketSession)`.
- `handle_socket` logs once at INFO on entry: `symbol = %session.symbol,
  "socket bound to river"`. This is the operator-visible proof the carrier is
  live, and the line piece 9 extends with the boat id.
- `spawn_command_dispatcher(command_rx, state, lanes, session.clone())` and
  `dispatch_command(cmd, &state, &lanes, &session)`.
- `process_order_cmd(cmd, state, run, lanes, session)`: the amend branch's
  symbol becomes `Arc::clone(&session.symbol)` and the "a run is one instrument"
  comment is replaced by "an amend names no symbol on the wire, so it resolves
  to the river this SOCKET is bound to".
- The market-data attach point stays `state.run.tape.subscribe_with_snapshot()`
  in this landing, with a comment stating that the run's tape IS the river the
  session named because resolution refused anything else, and that piece 9
  replaces this expression with a boatyard lookup keyed by the session.
- `SubmitOrder` carries its own symbol on the wire and is NOT overridden by the
  session. It is instead CHECKED against it, case-exactly, matching 4.2 step 3
  and the history predicate. Without this check a client could bind a socket to
  one river and trade another, which is precisely the cross-river leak piece
  9's sharing model must not have to defend against.

  THE FRAME IS `OrderRejected`, NOT `AdmissionRejected`. A symbol mismatch is a
  malformed request - a client error - and `messages.rs` states the rule in
  terms: a malformed request is "refused with the existing rejection mechanism,
  never with `AdmissionRejected` (which reads as a capacity signal)". The
  adapter's exec client maps `AdmissionRejected{Submit}` into `OrderRejected`
  anyway, so routing it that way buys nothing and mislabels backpressure at
  every other observer. Emit it exactly as the price-less-market and
  market-closed branches do: take a `lanes.try_reserve_boundary()` reservation,
  fall back to the existing `AdmissionRejected` "execution output admission
  budget exhausted" only when that reservation fails, and return
  `OrderOutcome::Refused` with reason `order symbol {echoed} does not match the
  symbol this connection is bound to ({bound})`, echo truncated by the same
  64-char rule.

  PLACEMENT IS PART OF THE CONTRACT: immediately after `boundary_outcome` and
  BEFORE the act delay, the `market_reading` call, the calendar lookup, the
  boundary reservation and the engine lock. `process_order_cmd` today calls
  `market_reading` before engine admission, and that path consults the SUBMITTED
  order's symbol - so a check placed near engine admission lets a mismatched
  MARKET order drive price synthesis, checkpoint-mutex and cache work for a
  river the socket is not bound to before being refused. Refusing at the
  boundary also keeps the refusal's `ts_event` the entry-time sample, which is
  what `boundary_outcome`'s neighbours already use and what the comment there
  says entry-time is for.

### 4.5 Client-side carriers

- `MogwaiDataClientConfig` and `MogwaiExecClientConfig` each gain
  `pub symbol: Option<String>` (`#[serde(default)]`, `None` in `Default`), and
  `ws_url()` becomes `format!("{base}/ws?symbol={symbol}")` when set, unchanged
  when not - where `base` is still
  `self.base_url.trim().trim_end_matches('/')`. The trim is not incidental: its
  doc comment records that a whitespace-padded `base_url` otherwise passes
  validation and fails inside the reconnect loop with no diagnostic (D.4), and
  a naive `format!("{}/ws?symbol={}", self.base_url, symbol)` reintroduces
  exactly that.
- **The DATA side needs a reconciliation, not just a field.** The data client
  already derives a symbol PER SUBSCRIPTION from the nautilus `instrument_id`
  (`symbol_from_instrument` in `subscribe_trades` / `subscribe_quotes` /
  `subscribe_bars`), so a config `symbol` creates a second source of truth for
  the same fact. Unreconciled, a host that subscribes ES on a socket bound to
  MNQ receives MNQ ticks relabelled ES by nautilus - silently, with no frame
  and no log: the cross-river leak the exec side is being fixed to prevent,
  arriving through the door nobody guarded. So `subscribe_symbol` gains the
  mirror check: when `config.symbol` is set and the derived subscription symbol
  does not match it case-exactly, the subscription FAILS with an `anyhow` error
  naming both symbols, alongside the existing `ensure!` refusals in that
  function. When `config.symbol` is `None` the socket takes the server default
  and no check applies, which is the pre-landing behaviour unchanged.
  A unit test in `client/data.rs`,
  `subscribe_refuses_an_instrument_outside_the_bound_symbol`, covers it.
- `validate()` gains the same charset and length check as
  `resolve_socket_symbol` step 2, with the reason stated in the message: the
  URL is built by concatenation, so an illegal symbol must fail at config
  validation rather than as an unreadable `400` inside the reconnect loop.
  Shared spelling: a `pub fn validate_wire_symbol(&str) -> Result<(), &'static
  str>` in `mogwai-protocol` next to `MAX_SYMBOL_LEN`, called by both ends. One
  rule, one place, no drift. It lives in `messages.rs` and MUST be added to the
  `pub use messages::{...}` list in `crates/mogwai-protocol/src/lib.rs`:
  `messages` is not the public access path (`MAX_SYMBOL_LEN` is re-exported
  there for the same reason), and both the server and adapter call sites in
  this spec name it at the crate root.
- `for_run(record, account_id)` sets `symbol: Some(record.symbol.clone())` -
  the readiness record already reports the run's symbol, so a launched-venue
  client names its river explicitly instead of relying on the server default.
- `scripts/smoke.py` requests `GET /ws?symbol=<symbol from the ready record>`.
- `crates/mogwai-cli/tests/common/mod.rs` gains a `ws_url_for(symbol)` helper
  alongside the existing bare `ws_url()`, since tests need both the defaulting
  and the naming paths.

## 5. Landing sequence

Two landings. The suite is green at the boundary between them, and each is
kept or reverted whole on its own gates.

**Landing A - the server-side carrier.** `SocketQuery`, `SocketSession`,
`resolve_socket_symbol`, the renamed `unserved_symbol_refusal`, the
`ws_upgrade` signature, the `handle_socket` threading, the amend-symbol
change, the `SubmitOrder` cross-river check, `validate_wire_symbol` in
`mogwai-protocol`, and the corrected `instruments()` and
`ADMISSION_PROMISE_TICKETS` doc comments. Every existing client keeps working
because the parameter is optional and defaults to the boot symbol.

**Landing B - the clients name their river.** The two adapter configs, the
`subscribe_symbol` mirror check, `for_run`, `smoke.py`, and the CLI test
helper. Depends on A being in, and only on A: it changes what clients SEND, and
A already accepts both forms.

Landing B also carries a MECHANICAL tail that belongs in its inventory, so that
"keep or revert whole" means what it says: adding a field to two PUBLIC config
structs touches every struct literal that constructs one without `..default()`.
Today that is `config.rs` itself, `client/data.rs`'s tests, and the socket-
backed `tests/data_client_transport.rs` and `tests/havoc.rs` (six literals in
the latter). None of it is interesting; all of it must compile in the same
commit.

VERSION SKEW ACROSS THE TWO LANDINGS IS SAFE, and worth stating because it
looks like it should not be: a pre-A server has no `Query` extractor at all and
ignores the query string outright, so a landing-B adapter pointed at an old
server degrades to the boot default rather than failing. The ordering
constraint is therefore about test coherence, not about breaking clients.

## 6. Gates, per brick

Landing A:

- `brokkr check` - gremlins, clippy and the whole changed-file test scope.
- `brokkr check --gate` is NOT required for landing A (it touches no
  `mogwai-adapter` file) but IS required for landing B, which does.
- New tests in `crates/mogwai-server/src/http.rs`, unit, no socket:
  - `an_absent_socket_symbol_binds_the_boot_symbol`
  - `a_socket_symbol_matching_the_boot_symbol_binds_it` (send `MNQ` against an
    `MNQ` run)
  - `a_miscased_socket_symbol_is_refused_like_history` - send `mnq` against an
    `MNQ` run and assert the refusal, pinning that `/ws` is exactly as
    case-exact as `history_symbol_refusal_uses_the_synthesis_lookup` requires
    of `/trades`. This is the test that stops a later reader from "fixing" the
    comparison into a case-fold
  - `a_configured_but_unbooted_socket_symbol_names_the_bound_river` - build a
    two-entry `InstrumentProfiles` via `from_profiles` with a `bound` that is
    one of them, request the other, and assert the fallback body from 4.2 step
    4. `build_instrument_profiles` cannot produce that map today, which is
    exactly why the test constructs it directly: the arm must be covered before
    piece 7 makes it reachable
  - `an_unserved_socket_symbol_is_refused_in_the_history_wording` - asserts the
    body equals `unserved_symbol_refusal`'s for the same input, which is what
    stops the two spellings from drifting
  - `an_illegal_socket_symbol_is_refused_before_the_unserved_check` - a symbol
    containing `%` or a space, asserting the CHARSET message, not merely that
    an error occurred (a test observing only an error cannot distinguish which
    check fired)
  - `an_absurd_socket_symbol_is_truncated_in_the_refusal` - 4096 chars, echo
    capped at 64
  - Run focused: `brokkr test -p mogwai-server socket_symbol`
- New socket-backed test in `crates/mogwai-cli/tests` (the only crate with
  `CARGO_BIN_EXE_mogwai`), named
  `ws_upgrade_refuses_an_unserved_symbol_with_400`: boot a venue, attempt
  `GET /ws?symbol=NOT-A-SYMBOL-HERE`, assert the response status is 400 and
  that NO upgrade occurred, then open `GET /ws?symbol=<boot symbol>` on the
  same venue and assert frames arrive. Asserting the second half is what makes
  this a carrier test rather than a rejection test.
  Run: `brokkr test -p mogwai-cli ws_upgrade_refuses`
- New socket-backed test, same crate,
  `an_order_for_another_symbol_is_refused_on_a_bound_socket`: bind to the boot
  symbol, submit an order naming a different symbol, assert an `OrderRejected`
  frame arrives carrying the mismatch reason - specifically NOT an
  `AdmissionRejected`, since the whole point is that this is a client error and
  not a capacity signal - and that no accepted-order event does. Per the
  standing rule, the assertion drains to a deadline rather than reading the
  next frame, because every socket is attached to the live tape on upgrade.
  The test must also assert that the mismatched request caused NO
  SYMBOL-DEPENDENT WORK, not merely that no order event appeared: a test
  observing only the refusal cannot distinguish a check at the boundary from
  one performed after synthesis already ran for the wrong river, which is the
  placement 4.4 makes contractual. Assert on the resource the finding names -
  the venue's own act-delay/market-reading path is not entered, observed
  through the sim-clock advance the act delay would cause or the absence of the
  synthesis-failure log line, whichever the fixture can see without racing.
  A unit-level companion in `http.rs` covering `process_order_cmd`'s ordering
  directly is acceptable evidence for the same claim if the socket fixture
  cannot observe it cleanly.
  Run: `brokkr test -p mogwai-cli an_order_for_another_symbol`
- Protocol round-trip gate: `mogwai-protocol`'s
  `client_and_server_messages_round_trip` must still pass UNCHANGED. This
  landing adds no frame, and that test passing untouched is the evidence.
  Run: `brokkr test -p mogwai-protocol client_and_server_messages_round_trip`
- Live end-to-end: `brokkr run mogwai -- serve` in one shell, then
  `python3 scripts/smoke.py`. Under landing A the smoke test still sends a bare
  `GET /ws` and must pass, which is the compatibility gate.

Landing B:

- `brokkr check --gate` - mandatory, it touches `mogwai-adapter`, and the four
  socket-backed adapter binaries are exactly what a URL-format change can
  break.
- New unit tests in `crates/mogwai-adapter/src/config.rs`:
  `ws_url_appends_a_configured_symbol`, `ws_url_omits_an_absent_symbol`,
  `for_run_carries_the_records_symbol`,
  `validate_refuses_a_symbol_needing_percent_encoding`,
  `ws_url_keeps_trimming_a_padded_base_url_with_a_symbol_set`.
  Run: `brokkr test -p mogwai-adapter ws_url`
- New unit test in `crates/mogwai-adapter/src/client/data.rs`:
  `subscribe_refuses_an_instrument_outside_the_bound_symbol`, plus its
  companion asserting that a `None` config symbol subscribes anything.
- Live end-to-end again: `brokkr run mogwai -- serve` plus
  `python3 scripts/smoke.py`, now with the symbol-carrying request. This is the
  gate that proves the two ends agree on the URL, which no unit test on either
  side can.

BITE-CHECK, per the standing rule, on every new test above: revert the
production change as a TEXT EDIT, observe the named failure, restore it as a
text edit. Never `git checkout -- <path>`. The two that are easiest to write
non-biting, and therefore need it most, are the wording-equality test (delete
the shared call, paste the literal, and it still passes unless the assertion
compares against the function) and the 400-before-upgrade test (assert on the
absence of the 101, not merely on a non-200).

## 7. Re-bless expectations

- No tape byte moves. This spec touches no generation input, no seed and no
  arrival parameter, so `TAPE_PROTOCOL_VERSION` is NOT bumped here. The symbol
  dimension on `RunSeeds` is piece 8 and owes its own bump.
- No wire frame changes, so no serde golden is re-blessed.
- `ReadyRecord::VERSION` does not move: piece 12 owns dropping `symbol` from
  it, and this landing still needs that field (landing B reads it).
- Three doc comments become false and are corrected IN the landing that makes
  them false: `ADMISSION_PROMISE_TICKETS`' "with no subscribe frame" becomes
  "with one river named on the upgrade, so still exactly one replay";
  `instruments()`' "would come to believe a second symbol is subscribable"
  becomes a statement that the run serves one river and the upgrade may name
  only that one; `process_order_cmd`'s "a run is one instrument" becomes the
  per-socket statement in 4.4.
- Durable prose owed with the code, per piece 14 and the standing item: `docs/`
  gains the `/ws?symbol=` query grammar wherever the WS endpoint is documented
  (`docs/cli.md`'s endpoint list at minimum), including the optionality, the
  charset rule, the case-EXACT match, the 400 refusal, the one-river-per-socket
  contract, and - stated as plainly as section 1 states it - that only the
  run's own symbol is accepted until piece 7, which is a temporary restriction
  and not the model;
  `reference/architecture.md` gains the carrier as the seam pieces 7, 9 and 10
  attach to, with the reason a query parameter was chosen over a frame
  (section 2 above, compressed). `notes/todo.md`'s piece 6 entry is struck and
  the detail left to git history, matching how pieces 1 through 3 and 5 were
  retired.

## 8. Stopping rule

IN: the query parameter, its parsing, its validation, its refusal, its
threading through the connection, the order/session symbol agreement check, the
client-side URL construction, and the prose above.

OUT, each because it is a named separate piece and not because it is hard:
serving a second symbol at all (piece 7 - `RunIndex`, `BOOT`, the `.next()`
collapse, lazy engine registration); the symbol dimension on seed derivation
(piece 8); boats, sharing keys and idle-river retirement (piece 9); per-boat
clocks (piece 10); `MarketReadingCache` and `last_swept_ns` (piece 11);
`ReadyRecord`'s fields (piece 12); `/instruments`' response shape and the
adapter's subscription guard (piece 13); `speed` and `duration` as query
parameters (they belong to pieces 9 and 10, and `deny_unknown_fields` is what
keeps them from being accepted-and-ignored in the meantime).

The blast radius in landings A and B combined, stated completely so the
keep-or-revert boundary is real:

- `crates/mogwai-server/src/ws.rs`, `crates/mogwai-server/src/http.rs`
- `crates/mogwai-server/src/admission.rs` - one doc comment
- `crates/mogwai-protocol/src/messages.rs` - `validate_wire_symbol`
- `crates/mogwai-protocol/src/lib.rs` - its re-export, without which neither
  caller in this spec compiles
- `crates/mogwai-adapter/src/config.rs` - two config structs, two `ws_url`s,
  two `validate`s, `for_run`, and the struct literals in its own tests
- `crates/mogwai-adapter/src/client/data.rs` - the `subscribe_symbol` mirror
  check, and the config literals in its tests
- `crates/mogwai-adapter/tests/data_client_transport.rs` and
  `crates/mogwai-adapter/tests/havoc.rs` - config struct literals only,
  mechanical, but they do not compile without the edit
- `crates/mogwai-cli/tests/common/mod.rs` - the `ws_url_for` helper
- `scripts/smoke.py`
- the new tests named in section 6, and the durable prose in section 7.

## 9. Findings considered and rejected

Recorded so they are not re-raised as new.

- **"Step 4 can produce an `Err` with no body, because a two-profile config
  booted on BTCUSDT answers `Some` from `profiles.get("MNQ")`."** REJECTED as
  stated: `build_instrument_profiles` validates every `[symbols.*]` table and
  then returns `from_profiles(vec![boot])`, so `InstrumentProfiles` holds
  exactly one entry and the divergence between step 3 and `profiles.get`
  cannot arise today. `serve.rs`'s `instrument_defs().next()` likewise picks
  the sole profile, not the alphabetically first of several. The finding's
  UNDERLYING requirement was accepted regardless and is folded into 4.2 step 4:
  a `Result<_, String>` may not be built by unwrapping an `Option` whose `None`
  arm piece 7 will make live, so the fallback body and its test are specified
  now.
- **"Resolve the case divergence by dropping the history endpoints' case
  exactness instead."** REJECTED as the resolution taken. The two reviews agree
  the divergence is real and must go; the direction is `/ws` becoming exact,
  because the exactness is pinned by a doc comment, a landed test and the
  silent-empty-page argument behind both, and loosening it is a change to
  history behaviour that has nothing to do with this carrier.
- **"Derive the adapter's WS URL from the first subscription rather than
  carrying a config field on the data side."** REJECTED. The URL is needed when
  the transport connects, which precedes any subscription, so deriving it there
  would either delay the connection or make the bound symbol depend on
  subscription arrival order. The config field stays and the mismatch is caught
  by the mirror check in 4.5 instead.
