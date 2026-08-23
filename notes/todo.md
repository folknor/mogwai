# TODO

Open work only. How the built system works lives in `reference/architecture.md`;
the landing-by-landing history is in git; the per-crate mechanics are in code
comments.

**Not the live arc.** Work that is actively being done belongs to its own
track's document, not here - see `notes/README.md` for the map. This file is for
what is parked, deferred, unresolved or owed by someone else.

Once an item here is completed, it gets removed entirely. If the prose contains
any relevant information that must endure, it gets either (a) added as an inline
comment in the code, or (b) added to an existing or new `../reference/` document.
Or both. There are no exceptions - a ruling recorded only here is a ruling the
next bug hunter re-derives from scratch.

## The gate that blocks other work

- **Segment-sampler gate: failed 2026-08-18, still failing.** The owner viewed
  the two Asia charts and rejected both as unusable - 300-point moves inside the
  session body over one-to-twenty-minute spans, which happen at an open and never
  in Asia session body. Both arms failed, including the gaps-off control, so the
  reopen-gap injection is not the cause. The verdict and its measurements are in
  `notes/segment-sampler.md`; the probe is `analysis/asia_jump_probe.py` (the
  owner's untracked work in progress - never sweep it into a commit, and never
  put a number out of it beside a Rust-computed one, since its percentile
  convention differs from `mogwai_lab::kernel::nearest_rank_list`).

  Two repairs are owed before a re-render is worth the owner's eye:
  - the seam level discontinuity that survives `--no-reopen-gaps` and
    contaminates the control - self-contained and ours;
  - whatever the cut admits at Asia bars 1112-1113 - carried in from the segment
    data, and possibly a cut-criteria question for the owner rather than a bug.

  Nothing further is built on the composed tape until a re-render passes. In
  particular the serving wiring is a real refactor: `CheckpointIndex` is typed on
  `GeneratedSource`, so a composed river means generalizing the checkpoint and
  resume path.

  London and the two NY charts are unviewed and unprobed, so whether the defect
  is Asia-specific or general is open. Regenerating one end to end (substitute
  the window name and library path for the others):

  ```text
  brokkr run mogwai -- segments cut --symbol MNQ --month 2026-04 --window asia --out analysis/out/asia-mnq-2026-04.json
  brokkr run mogwai -- segments compose --library analysis/out/asia-mnq-2026-04.json --type bars --interval-s 60 --ticks 3000000 --seed 42 --out analysis/out/asia-endless.csv
  python3 analysis/plot_tape.py --csv analysis/out/asia-endless.csv --out analysis/out/asia-endless.html --title "Endless Asia, MNQ 2026-04 segments, seed 42"
  ```

  The cut needs the delivered corpus at
  `research/market-data/databento/mnqv/2026-04.manifest.tbbo`, which is out of
  git. April yielded: asia 22 segments and 2,976,377 ticks, london 22 and
  2,492,576, ny-morning 21 and 8,396,328, ny-afternoon 21 and 9,572,450. Times in
  these charts are composed tape time, not a calendar: a composed tape starts at
  unix ns 0 and elides the hours between sessions.

## The shared-exchange mode

The default mode - one venue owned by one run, serving N tapes to one account at
one placement - is complete. The optional shared-exchange mode has two axes still
open, both removed by the one-venue-per-run rewrite and only one of them since
undone. Neither is urgent; both modes must eventually be supported.

- **Named tape windows.** `SocketQuery` carries no start or end: every cursor is
  placed at the fixed `run_start_ns` origin and `duration_ms` is
  length-from-boarding, so there is no wire for naming a window at all. This is
  the half most strategies will use, because a named window is what makes a
  forward-test claim bindable: a run becomes a pure function of `(seed, config,
  symbol, start, end)` with no boarding instant and no wall-clock input anywhere,
  and replication pairs dealt the same window trade identical water by
  construction rather than approximately.

  A named window always gets its own river even against an identical request
  already running, because the first requester is by then some N of sim-time
  ahead and a window means being served from its start. Sharing therefore only
  happens for the unnamed form - a preset plus a duration - which is the request
  that says "wherever you are is fine".

  One constraint to design for up front: a strategy needs warmup before its
  requested start, so `[T1, T2]` asks for materialization from `T1 - warmup_ns`,
  and that floor must sit at or above `TAPE_ORIGIN_NS`. A window requested too
  near the tape origin cannot carry its own warmup. Better as a named refusal at
  request time than a short warmup nobody notices.

- **Server mode should boot riverless.** Ruled by the owner 2026-08-20; not
  designed, not landed. A shared exchange has seen no request at boot, so the
  eagerly warmed boot river is a guess paid for in full warmup synthesis before
  readiness plus a permanent boat, pacing thread and sweeper slot, while
  demand-driven materialization already serves every other river.

  The venue can tell the modes apart at boot: server mode is `mogwai serve`,
  transient mode is `mogwai_protocol::launch::launch(spec)`, and the launch path
  always passes `--launcher-pid` where an operator's command line does not. (The
  cannot-tell-them-apart fact about clients is a different question.) So a
  transient venue keeps today's eager boot river and a server-mode venue boots
  riverless. Two consequences to decide with it: readiness stops implying any
  warm river in server mode, and a `/ws` bind naming no symbol on a riverless
  venue needs a decided answer, presumably the default preset materialized on
  demand. `reference/glossary.md`'s boot-river entries describe what is and stay
  as they are until this lands.

  For the transient path, where the venue really does know its one symbol up
  front, a boot symbol supplied at launch would additionally move the funding
  refusal to boot: today a config funding only USDT boots happily and refuses
  `MNQ` at first bind, because only configured shapes are funding-checked at boot
  while presets and the fallback are merely recorded as barred.

- **One ledger, one cadence.** Seats are keyed by (river, speed) and an unserved
  speed is a second cursor on the same water, so the only surviving refusal is a
  second socket on an account already riding that river at another speed. Two
  cadences over one river share the checkpoint chain underneath, so serving this
  is a per-ledger clock question rather than a second river.

## Venue and protocol

- The connection lifecycle is four mutable structures, not one derived registry.
  `Run::lanes`, `Passenger::frozen_since`, `Passenger::seated_on` and
  `Passenger::attachments` each hold part of the answer to "is anybody reading
  this account", with the consistency rules only in prose, and nothing detects
  the next lifecycle path that updates three of the four. Wanted: one registry
  keyed by account holding the live connections, with `is_frozen` and
  `is_seated_on` as derived queries. It is a rewrite of `run.rs` rather than a
  fix, which is why it was not attempted when the two live holes were closed.

- One `/ws` refusal still sits after the eviction, and only one: the cadence
  check re-run on a ledger the seat minted or reset, which can lose to another
  upgrade racing the same account inside that window. The pre-seat check covering
  every other case cannot cover this one, because the ledger it would ask about
  does not exist yet. Closing it means making eviction and admission one
  transaction under the lane lock, which is the registry item above rather than a
  local fix. It needs two upgrades on one account interleaved inside a few
  microseconds to fire.

- A passenger whose own duration ends is sent a `RunComplete` frame before the
  `close::DURATION_COMPLETE` close, identically to the whole-run arm
  (`crates/mogwai-venue/src/ws.rs`). A client classifying on the text frame reads
  a duration end as a run completion, and the close behind it is the only
  accurate statement. Nothing breaks today, since both readings stop the client,
  and the imprecision is documented at all three sites. The fix is either a
  distinct per-passenger completion frame or dropping the frame on that arm and
  letting the close carry it - both protocol changes with consumers to consider.
  Nothing detects it regressing further, because the two arms are correct in
  isolation.

- `RunComplete` reports slightly less than the declared duration and nothing on
  the wire lets a consumer tell that from a short run. The deadline is judged on
  the venue clock while `ws.rs` re-derives every announcement on the receiving
  socket's boat clock, so the announcement trails by the placement gap times
  `speed`. Both halves are deliberate and stated in `reference/clock.md`. What is
  open is whether a consumer should be able to distinguish "the run served its
  whole duration and my boat was placed late" from "the run was cut short":
  shipping the boat's epoch, or the venue's own elapsed alongside the socket's,
  would close it. A wire change nobody has asked for, and the same missing field
  as the item above.

- `reject_while_closed` judges marketability against the stated price while the
  engine judges it against the band-drawn trigger, so the two can disagree by up
  to the fill band in either direction: an order the server admits as
  non-marketable can be marketable to the engine and fill off the stale print the
  guard exists to refuse, and one the server refuses can be one the engine would
  have rested. The engine's `draw_trigger` needs the order's `band_ticks` and the
  run's `fill_seed`, neither of which the HTTP boundary holds, so closing this
  means asking the engine rather than re-deriving - `Engine::worst_case_leaves`
  is the precedent shape. Affects `Market` and `MarketToLimit`.

- `try_reserve_boundary_frames` takes a `usize` and does `frames.max(1)`, which
  makes an unreachable state harmless rather than unrepresentable. Every
  `refuse_all` call site is guarded by a `submitted_orders(...).first()`, so the
  zero-frame case cannot arrive. Taking a `NonZeroUsize` moves the guarantee from
  the call sites to the signature; it is a change across the admission boundary
  rather than a bug fix.

- Engine-arm application order is unordered across concurrent control requests.
  `Run::arm` records under the passenger map lock but applies the engine half
  after both locks drop, because the engine sits behind an async mutex, so two
  `POST /control/divergence` requests in flight at once can land on two seated
  ledgers in opposite orders. Unreachable in practice - the control plane is an
  operator surface, serialized in every scenario the venue is driven from - and
  closing it costs a second lock on a path that has never contended.

- `DivergenceRequest` accepts and ignores unknown fields: serde flatten blocks
  `deny_unknown_fields`, so the fix is structural, a kind/args request shape.
  Related and unruled: its 202 body is empty, English prose, or prose containing
  a Rust `{:?}` render, on the one route an automated scenario driver uses most.

- `enforce_funds` is a whole account mode inferred from an empty balance map at
  engine construction, invisible from the wire and undocumented anywhere durable.

- The account-claim path in `ws.rs` leaves an abandoned ride behind, safe by a
  reachability argument rather than a guard, where every other path releases
  through `Drop`. The frontier family in reverse.

- `BoatKey` carries no placement nonce, so boat identity across lifetimes is
  unrepresentable: a boat placed over a river again is the same key and
  `is_seated_on` still passes. `ws.rs`'s stale-seat hazard is held off by drop
  ordering rather than by identity, and is the remaining consumer if a nonce is
  ever wanted. (Withdrawn as unanswerable and recorded so it is not re-filed:
  whether an eviction-reconnect should retire the book it takes over. It needs
  the venue to tell a returning client from a stranger presenting the same id,
  and a session id is self-asserted with no auth behind it.)

- `GoDark` swallows the startup mass-status query, so a client armed with it can
  never complete boot and never reaches command sequencing. Decide whether that
  is correct by design - it is a blackout - or an arm too broad to be useful.

- The multi-river peak-equity bound. For a multi-river account the peak-equity
  ratchet is fed a partial, arbitrarily ordered reconstruction: equity is a sum
  over rivers, its extreme over a span need not sit at either river's extreme,
  and the sweeper judges per due boat with the other rivers at last marks.
  Closing it means a per-account extremes reconstruction across rivers. The
  honest bound is already stated in the durable prose, so this is engineering
  rather than documentation. Adjacent and unruled: the position cap is per
  symbol, and whether an aggregate cap is wanted.

- A whitespace-padded currency code is accepted and matches no balance. `" USD "`
  passes both `mogwai-protocol` validators - the risk-policy `validate()` and
  `validate_divergence` agree on refusing blank via `trim()`, but neither
  normalizes - so it is admitted as a lookup key that will match no balance and
  freeze a policed account's equity at zero. The rule wanted, on both validators
  at once: refuse any code differing from its trimmed form. Two passes of the
  2026-08 arc declined to smuggle it in under adjacent findings.

- Refusal texts spell their bounds out instead of naming the constant.
  `messages::validate_wire_symbol` refuses with "symbols are 1 to 32 characters"
  while comparing against `MAX_SYMBOL_LEN`, and says characters where the check
  is `symbol.len()` in bytes - harmless only because the arm below admits ASCII
  alone. This one is what a client sees at the venue's front door, since order
  entry routes through it. Four divergence texts in the same module have the
  shape too; count at the production sites, since the module's tests carry the
  same strings as expected values. Both refusals return `&'static str`, so fixing
  means changing the return type or reaching for a `const` formatter, which is
  why neither was fixed in passing.

- The launcher kills one process, not a process group. `launch`'s timeout arm
  issues `child.kill()` and does not join the readiness reader, which is what
  makes the readiness bound unconditional. The kill closes stdout only while the
  child holds the write end, so a `binary` naming a wrapper script that starts
  the venue without `exec`, or a venue that ever grows a helper subprocess,
  leaves a grandchild holding an inherited copy and strands that reader thread
  for the life of the process. One leaked thread per timed-out launch is the
  deliberate trade. The robust form is putting the child in its own process group
  and `killpg`ing it, which also collects a helper the venue itself spawned, but
  not a wrapper's grandchild that has left the group - so it narrows the hole
  rather than closing it, and it is a real change to the launcher's process
  model. Latent today: `mogwai serve` spawns nothing, and `docs/cli.md` states
  the supported shape.

## Engine

- An equity sell's hold hands the same held shares to every resting sell.
  `Engine::order_hold`'s margin-equity sell arm computes `uncovered = leaves -
  max(0, net_position)`, so a margin account holding 100 shares with two resting
  sells of 100 posts collateral for neither, where the worst fill order leaves it
  short 100. Admission is already safe, since `validate_submit`'s short check
  reads `Engine::worst_case_leaves`; what is left is the hold carried between
  acceptance and fill.

  The obvious fix breaks a different invariant. `order_hold` is per-order by
  construction: the incremental `order_holds` cache adds and removes one order's
  entry at a time, and `reconcile_order_holds` panics on any drift from a fresh
  fold, so any formula reading the other resting sells makes one order's hold a
  function of the book. Closing it properly means moving the cover allocation out
  of the per-order derivation into the aggregate, which is a redesign of the
  cache. The report's own suggested expression does not do it either, since
  summing `leaves - max(0, net - other_sells)` over both sells holds for 200.
  There is a product call inside it too: what a venue should hold against a
  covered sell that another resting sell might consume first.

- `projected_qty` takes a bare `Decimal`, so an incoming order that is itself one
  leg of an `Oco` pair is counted additively against the exclusive group it
  belongs to. The resting book is counted correctly by
  `Engine::worst_case_leaves` - held children contribute nothing, an exclusive
  group contributes its max - but the `additional` argument cannot be, because
  the caller does not pass the order. The effect is a conservative
  over-projection in `mogwai-venue`'s optional `max_position` cap: it can refuse
  an order the book could not actually have reached, never admit one it could.
  Fixing it means giving `projected_qty` the `SubmitOrder` rather than a
  `Decimal`, a signature change through `http.rs`.

- `MarketToLimit` is an open engine defect documented on the wire type and
  nowhere actionable: the fill takes the whole quantity at the order's own limit
  with no reference to the tape, and a divergence-manufactured remainder rests
  `Inert` - unable to fill, unable to expire, ended only by a consumer cancel.
  `Resting::Inert`'s doc describes the mechanism and neither side points at the
  other.

- A zero-price fill is still warned about and booked by `warn_zero_px`, so a
  position can carry `mark_px == 0` if the tape produces one.
  `position_unrealized_checked`'s zero answer is the backstop for exactly that
  case. Refusing at the fill was considered and rejected in the 2026-08-20
  ruling, because by then the tape has already produced the print and aborting
  the serving path over it is the one thing no venue does. Open only as a
  known-covered case.

- The venue-wide fee surcharge replay in `ArmRecord::open_engine` has no direct
  assertion. It is covered only by its neighbour, the engine-queue replay,
  biting, because `fee_surcharge_multiplier_for` is `pub(crate)` and
  `mogwai-engine` exposes no reader outside its own crate. Closing it means
  either a public accessor or a socket-level test that fills on a late-connecting
  account and reads the commission.

- Account valuation residue, none of it blocking:
  - One hop only. An asset is valued through an instrument quoting it directly in
    the policy currency, so holding ETH under a USD policy with only ETHUSDT and
    BTCUSD listed leaves the account unvaluable rather than valued through a
    chain. A rate surface would fix it and buys cross-currency accounts too.
  - The mark is as stale as the last sweep, inherited from the margin ledger.
  - The ledger-generality ruling still wants shares, leverage and funding
    payments; each needs a holding valued in a currency it is not denominated in,
    so this machinery is the part of that which now exists.

- The late-boarder rule is open-coded twice with nothing shared: the fee
  surcharge window in `mogwai-engine` and the FlowSurge branch of
  `arm_divergence` in `mogwai-venue`.

- Two project-owned `BreachAction` types share one name with disjoint variants,
  and only one is defined anywhere. A wrong import compiles.

- A linkage release emits no wire frame, so a consumer watching bracket exit legs
  waits forever. Stated only in doc comments; it is either a protocol gap to
  close or a refusal to state on the wire.

## Instruments and account policy

- Account policy and the new instrument classes, still open:
  - Funding is paid on the fill sweeper's cadence, so a funding instant is
    honoured on the pass that crosses it rather than at the instant itself.
  - Nothing has been fitted for equity, perpetual or inverse. A symbol configured
    as one is served the default tape wearing a different shape; the intake
    sequence is what makes a preset honest and none has been run.
  - A footprint that never contains the policy's daily reset instant - an
    Asia-only loop, 8pm to 3am ET, under a 17:00 ET reset - never resets the
    daily budget, so a daily loss limit silently becomes a run-lifetime limit.
    Flagged, not solved.
  - Not answered: whether a havoc-induced disconnect should behave differently
    from a real one. The venue armed the blackout so it knows the client is
    merely blinded, and `GoDark` is arguably toothless if the world stops while
    it is armed. Freeze-and-resume covers it for now and nothing is blocked.
  - Open and small: whether a strategy should see its own remaining budget in
    order to size against it. It can derive peak and threshold from its own fills
    and marks, so blind trading is workable. Decide it when one asks.
  - Recorded so the symmetry is not rediscovered: instrument presets are still
    compile-time (`include_str!` against a fixed table in `config.rs`) and could
    gain the runtime registration account policies have. Not a precondition of
    anything.

- Forex conventions. Leverage landed with the notional margin basis, and the 24/5
  session `[instrument.calendar]` already expresses. Still missing: pip and point
  conventions, and rollover or swap charged on a position held across the daily
  boundary. Open question whether that needs a `Forex` arm on `InstrumentClass`
  or rides `Spot` plus a margin policy.

- Futures expiry and roll. The continuous cash-settled `Future` forecloses
  anything keyed to expiry, and any strategy whose horizon crosses a roll is
  tested against a contract that never rolls. Recorded, not pressed.

- Options are excluded on the owner's stated ground that they are not understood
  well enough to specify. Revisitable for the same reason the order-type
  exclusions were reversed: "the owner does not need it" stopped being the
  scoping rule when mogwai went public. It stands until someone who understands
  them argues it in.

- `[balances]` and `[account_policies]` are separate tables, so no named policy
  can state its own opening equity, which is what a funded-account programme is.

- `[regime]` is run-wide, so a per-passenger generator arm has no operator
  expression. A config-schema gap.

- Owner call: should an order-list release carry a market reading? A standalone
  `MarketToLimit` takes the market, while the same type released as an order-list
  child rests at its stated price, because `Engine::release_child` runs inside
  `apply_linkage_after_fill` with no `MarketReading` to price against. Resting is
  the consistent behaviour and the carve-out is stated in `docs/oms-types.md`, so
  nothing is broken. Deciding yes would make a released `MarketToLimit` execute
  on arrival and would reopen whether a `Market` child can be admitted too.

- A trigger-act latency havoc arm, if a scenario ever needs a trigger fired later
  than the sweep interval already allows. Deliberately not built: the sweep
  interval already bounds trigger lateness, and a per-trigger delay knob is a new
  arm rather than an extension of an existing one.

## Data and generator

- `SegmentSource` overrides neither `seek_to` nor `fault`. An effectively
  infinite source inherits the O(distance) default walk that `mogwai-data`'s own
  `TickSource` doc warns about - the shape `GeneratedSource` needed
  `CheckpointIndex` for - harmless today only because `mogwai segments compose`
  walks forward from the origin and never seeks. It becomes a hang the moment
  anything serves a composed river or asks it for a window.

  `fault` is the harder half: the composer's one terminal condition, clock
  exhaustion, has no `TickFault` variant and is reported only through the
  inherent `SegmentSource::clock_exhausted`, which a `dyn TickSource` consumer
  cannot see, and adding a variant ripples into `mogwai-venue`'s `http.rs` fault
  rendering. The same item owns `emit_price`'s panic: a named panic inside
  `TickSource::next_tick` in a library crate, which becomes a serving-path abort
  the moment a composed river is served, where `GeneratedSource`'s equivalent
  failures go through `TickFault`. Giving the composer a `TickFault` closes both
  halves at once.

- Generator havoc must fork the river. The tape machinery deliberately mutates a
  canonical boatless river instead, with the pinned control-boundary snapshot,
  the coarsen exemption and the walk-back floor built to make non-forking
  correct, so the gap has known size and known work to undo. The seated-boat
  refusal standing in for the fork names a remedy no route exposes, and its gate
  reads boat presence in the non-awaiting form, so it is vacuous against a
  concurrent board.

- The synthetic top of book is uncalibrated: quoted width, top sizes and trade
  displacement are placeholder constants pending CME TBBO. The layer exists since
  tape protocol 7; what is absent is the calibration.

- Numerical stability in `AutoCorr`
  (`crates/mogwai-lab/src/characterize/mod.rs`), needing cadence-impact analysis
  before anyone touches it. Its `acf()` guards zero variance with `if var <= 0.0
  { return vec![0.0; self.k] }`, which catches zero and any negative residue but
  not a positive one, so a series constant at an irrational value - the fixture
  case is `abs(log return)` constant at ln2 - can leave a tiny positive residue
  from `sumsq / n - mean * mean`, slip the guard, and return an ACF that came out
  of catastrophic cancellation rather than measurement. Which side of zero the
  residue lands on is not something the caller controls. Both branches substitute a number where the honest
  answer is that the quantity is undefined for a constant series.

  Deliberately not fixed. `AutoCorr` also computes the F1 duration ACFs and is
  bit-exact against `analysis/cadence.json` (`duration_acf_lag1`
  0.32204142581620676, `duration_acf_lag5` 0.22388204486699373), which is the
  lineage the fingerprint's cadence half rests on, and changing the estimator
  invalidates that equivalence. A fix returns an explicit unavailable rather than
  zeros, uses a relative rather than absolute variance floor, and probably a
  two-pass or Welford accumulation - all of which move numbers, so the work is
  the analysis of what moves in the cadence targets and whether the fingerprint
  must be refitted. Real monthly series carry positive return variance and come
  nowhere near the degenerate case.

- The `ARRIVAL_MEAN_CAL` gate covers the bare side only: no cheap observable
  reaches the corrected side, because the composition is inline in
  `GeneratedSource::new` and lands in a private field, and the factor cancels in
  every ratio probe. A `pub(super)` accessor returning the composed active mean
  would close the second half in one line - for `quiet_share` 0 it must equal the
  calibration times the bare mean, bit for bit.

- The sub-table typo guard covers `[instrument.generator]` and
  `[instrument.session]` one level deep only; the nested seams inside `generator`
  (`quoted_width`, `top_sizes`, `trade_displacement_ticks`, `arrival`) stay
  permissive, and the guard's doc says so.

- Two public `SessionSegment` types in `mogwai-lab` (`summary` and `session`)
  share field names, so a wrong import compiles. Renaming the narrow one keeps
  the recorded reasoning and removes the trap.

## Adapter

- `MogwaiDataClient::sink` is an `Option` filled in `start()`, and several
  delivery sites are `if let Ok(sink) = self.sink()`, so a data client connected
  without being started drops ticks and bars with no error - the exec side's
  finding-1 family on the data leg. The symmetric guard would have to be checked
  against the `data_client_transport` binary's own start/connect orderings first.
  `get_data_event_sender()` panics rather than returning `None`, so the
  start-with-no-runner case is at least loud; it is the connect-without-start
  case that is silent.

- `await_account_registered` and `wait_connected` are busy-wait shims inside
  `connect()`. Both poll on a 10 ms sleep for up to 5 s, roughly 500 wakeups on a
  slow boot, and neither is sim-scaled unlike every other sleep in the lifecycle,
  so under an accelerated clock they are the only wall-time waits in the boot
  path. Both want a notifier: the connected flag is the adapter's own and could
  carry a `tokio::sync::Notify` beside it, and the cache poll wants nautilus to
  signal registration.

  `wait_connected`'s hardcoded 5 s is the sharper half: neither sim-scaled nor
  configurable, it can fail `connect` against a venue correctly paying a fresh
  river's warmup inside the first request. The dial path names the cold-river
  case; the connect wait does not.

- The adapter cannot name a speed or a duration: `ws_url` emits neither, both
  configs carry no field for them, and the venue reads both. Related, the venue's
  second-cadence refusal reaches the adapter as a failed upgrade retried forever
  with backoff - a configuration error handled as a transient outage, unlike the
  identity mismatch, which refuses terminally.

- The adapter reads the wrong clock: `fetch_clock` hits bare `GET /clock` with
  neither symbol nor speed, so every timestamp, havoc deadline, quota interval
  and backoff sits on the venue axis. The envelope's `boat_clock` flag has no
  reader in the crate.

- The many-rivers shape is not expressible through the adapter's public API: one
  data client binds one river and refuses every other subscription, and nothing
  presents multi-pair composition as supported.

- The equity conversion drops the three facts an equity carries: lot size,
  borrowability and settlement period all pass as `None` in `convert.rs`.

- `ship_server_havoc` serializes each divergence bare, so transport arms default
  venue-wide instead of riding the configured account, and generator arms lose
  their symbol scope.

- `for_run` discards `account_ttl_ms` and `reset_account_on_reconnect`, so the
  adapter's reconnect loop can back off past a freeze TTL blind.

- `HavocSpec.data` (`Option<MarketRegime>`) appears to have no reader on the
  adapter side now that the `Subscribe` carrier is retired, so an operator
  setting `[havoc.data]` may be arming a field nothing consumes - the
  looks-armed-and-is-not shape. Wants a verdict: route it or refuse it.

- The adapter's `fetch_account` names the configured account, but what a
  reported-id mismatch should mean under the per-account venue is still only a
  cosmetic log line (`note_account_label`). Whether it should ever be treated as
  an error is undecided.

- Reconciliation: the silent-degrade property is a class, shared by every report
  path mogwai relies on. `crates/mogwai-adapter/tests/reconciliation.rs` seeds
  venue truth and pins each granular generator, `query_order` and their
  mass-status composition over both query carriers. Known limitation: it proves
  the adapter would answer when asked, not that the node asks.

## Tests and tooling

- Triage every test for parallel safety, and kill every fixed duration and wait.
  `[test.profiles.gate]` sits at `test_threads = 8`, which is a measured
  compromise rather than a resolution: at 16 the run goes red as a wrong answer
  rather than a watchdog timeout, so the ceiling is set by our least robust test
  rather than by the machine, and every fixed wall-clock wait in the suite is a
  piece of that ceiling.

  What the measurement found. Serial, the gate spent 164s executing 1,608 tests,
  and the top 20 of them were 54 percent of it while the other 1,451 came to 3.8s
  combined. Almost none of that concentration is computation: the lifecycle gates
  spend a declared `--duration` in wall time and the reconnect ladders spend
  their attempts the same way. The genuinely CPU-bound tests are the tape walks,
  and they are the minority.

  The work is triage before repair. For every test ask two things: can it run
  beside its siblings, and does it wait on a duration rather than on a condition.
  A test that waits for a state to be reached, with a generous deadline as the
  failure path, is both parallel-safe and fast; a test that sleeps a fixed span is
  neither, and it silently prices the whole gate. The lifecycle family is the
  obvious start - `completion.rs`, `serving.rs`, `lifecycle.rs` and the adapter's
  four socket binaries - but the sweep is every test, because the point is a
  property of the suite. Check the fixed-path unit tests while triaging too:
  nothing collides today, since every one writes a distinct `target/...` name and
  ports are kernel-assigned, but that is convention rather than structure and one
  duplicated literal breaks it only under load.

  What it unlocks: `test_threads` can go to 0 (num_cpus) with the cliff gone
  rather than merely avoided. What is not the answer, already measured and
  rejected in `brokkr.toml`: a serial lane for the socket-backed tests floors at
  74s against the flat setting's 53s, because those tests are the best parallel
  citizens in the suite precisely by being idle.

  Anything called settled here needs repeated runs. `test_threads = 8` went red
  after three green runs at 8, having already gone red at 16; three passes are
  not evidence about an intermittent race, a failure rate is. The parked list is
  empty as of 2026-08-19.

- The venue counts sweep passes for nobody, which is the missing observable
  behind the last fixed sleeps. Wanted: a monotonic count of completed sweep
  passes per river, readable from a route that already exists - a field on
  `/clock` or `/health` would do - so a test can read it, wait for it to advance
  by N, and say exactly what it waited for. Nothing on the wire carries it today;
  the sweeper is internal to `mogwai-venue`'s fill path and emits no frame,
  counter or log a test can consume. Blocked on it:
  `serving::the_tape_is_identical_with_and_without_a_resting_stop` polls
  sim-clock advance as a proxy, sound only because that config's clock is
  wall-affine, and `serving::a_perpetual_position_pays_funding_across_an_interval`
  still bets on a wall sleep, the same poll having been found vacuous at
  `speed = 0.0`.

- Nothing on the wire says whether a submit took a market reading, which forces
  `serving::a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths`
  to key on the venue's log. When `read_market` refuses - a cold volatility
  estimator, a truncated walk - the engine falls back to the order's stated price
  and logs a warn, and on a price-less market order the venue stamps the last
  print either way, so the fill lands on the tape whether a reading was taken or
  not. That is exactly the path the venue used to get wrong.

  What the venue would have to ship: the reading's own instant, or a bare
  "reading taken" flag, on `OrderFilled`. Two things follow. The gate stops
  reading logs, and the adverse-slippage invariant - a market buy fills at or
  above the print the venue read - becomes an exact per-fill statement instead of
  the bracket it is now. The bracket exists only because the reading instant is
  unidentifiable from outside: it is neither the acceptance instant nor the fill
  instant, and `MarketReadingCache` buckets it further.

- History-splice clamp has no test for the case that motivated it. The cutoff is
  the tighter of the run clock and the asking passenger's own boat clock, so a
  slow boat is no longer served its own future, but every socket test runs at the
  venue's default speed where the two clocks agree, so nothing exercises the
  branch where they differ. Wants a passenger on a deliberately slow boat reading
  history past the point its own tape has reached, with the boat's frontier as an
  observable rather than a sleep. Filed 2026-08-23: the code is right, the
  coverage is a hole, and this is the shape that passes for the wrong reason
  later.

- The abandoned-upgrade path has no socket-level test, and no client behaviour
  found so far reaches it. `Passenger::attachments` exists for the upgrade a
  client walks away from before `handle_socket` runs - no lane bound, no lane
  released - pinned only by `run.rs` unit tests that drop an `Attach` directly.
  Sixteen connections writing a well-formed upgrade request and then resetting
  with `SO_LINGER` at zero all landed on the handled path instead: on loopback
  the venue has read the request, written the 101 and started the handler before
  the reset arrives. The race is inside hyper's upgrade handoff, so
  parameterizing an interval has nothing to take hold of. Closing it needs a seam
  the venue does not have, most plausibly a test-only delay or counter between
  the response and the handoff.

- `run_b1`'s build-identity guard (`crates/mogwai-cli/src/arrival_control.rs`)
  has no test, and the `bugs-cli` round-3 fix pass refused the binary split
  partly on the strength of that guard. It bails unless `current_exe()` ends with
  `target/release/mogwai`, so B1's byte comparison is generated by the binary
  under test - the property the refusal record names as the reason `gen` and
  `arrival-control` must stay in one executable. Nothing asserts it: a test
  binary's `current_exe` is never `target/release/mogwai`, so only the refusal
  arm is reachable. Wants a seam that lets the accepted path be supplied. Low
  priority, since the guard is three lines and its consequence is a refusal
  rather than a wrong answer.

- `mogwai gen --type trace` has no end-to-end CLI coverage. The one test sits at
  the `write_trace` seam in `gen.rs`; nothing drives the argv through the shipped
  binary the way `presets_cli.rs` does for `presets` - `--trace-from` and
  `--trace-until` parsing, the four-part window validation, the `--interval` and
  `--burn-in` rejections. The window validation admits `until == end`, which is
  the case the truncated-`child_count` defect lived in, and no test states that
  it is legal.

- Watch the first live `mogwai measure` run against the tightened
  `session_dates_are_23_sorted_unique` gate in `crates/mogwai-cli/src/measure.rs`.
  It has only ever executed under its unit test, because the gate sits mid-way
  through a multi-minute walk driver behind a Brick G cache that no test sweep in
  this workspace populates.

- The roll conformance fixture's Python half is manual: `python3
  analysis/roll_estimator.py conformance` is run by no lane, so its
  fixture-version guard fires only for a human who thinks to invoke it. The dwell
  pair has automated tests on both sides; this pair does not, because a Rust test
  may not spawn Python.

- Neither shared conformance fixture detects a quietly widened `tolerance`. The
  fixture version is a schema version, and a tolerance edit weakens both
  implementations at once, so the second implementation - whose whole purpose is
  to catch a one-sided drift - is structurally blind to it. Unlike the arrival
  vectors there is no re-derivation to compare against; a fix needs an
  independently derived bound on the tolerance itself.

- Nothing routes a new wall-clock budget into the `timing` sweep. `brokkr.toml`
  states the policy - a latency assertion is `#[ignore]`d at the source, listed
  in the gate's `skip`, and named in the `timing` sweep's `only` - and the tool
  enforces it in one direction only: an `only` entry the gate does not skip is an
  orphaned pair, and a filter matching nothing is dead. The converse, that every
  budget-carrying test appears in some `only` filter, is not a syntactic property
  and is therefore not checkable, so a plain `#[test]` asserting 50 ms in the
  parallel dev lane is admitted silently. Where the quantity is a parameter,
  inflating the interval under test is the better answer; where it is not, there
  is no answer. An owner-level question if anyone wants it mechanised - a source
  scan for `Duration::from_` inside an `assert!` is the crude form, and would
  have to justify its false-positive rate against a repository full of legitimate
  loose bounds.

- `rustdoc::broken_intra_doc_links` is nowhere enabled, so the next dangling doc
  reference goes undetected. The wrong-item doc-comment class is likewise
  detected by nothing.

- The no-shouting textlint is blind to several classes, so shouted words survive
  in three places at once. Check the lint's coverage before sweeping by hand,
  because a hand sweep will miss whatever the lint misses next time.
  - Rust comments it does not reach. The `run.rs` and `http.rs` survivors were
    swept 2026-08-23; `mogwai-venue`'s `config.rs` still carries `RESOLVED`,
    `ASK`, `CASH`, `PER BOAT` and `NOT`, and `account.rs` was never checked.
    All are single words that rewrite cleanly.
  - Test fixture headers, which are `.toml` rather than Rust and were never in
    scope: `crates/mogwai-cli/tests/configs/` wants a sweep, one file having been
    de-shouted in passing.
  - Assertion and panic message strings, which read to a human exactly like prose
    (`the venue CLOSED the perpetual socket`, `never fully WATCHED a run` in
    `serving.rs` and `completion.rs`). Whether the rule covers them is worth
    deciding once rather than per site.

- Open lead, not reproduced and not closed:
  `sigterm_stops_the_venue_within_the_shutdown_grace` in
  `crates/mogwai-cli/tests/lifecycle.rs` failed one full `brokkr check --gate`
  run on 2026-08-19 and passed the identical tree's second, with the tree's
  changes nowhere near the serving or shutdown path. Hunted twice on the harness
  that did fire the completion-path race: 20 rounds at 16 threads and 30 at 32,
  both under 64 busy processes, zero failures.

  Read the new output before theorising. The original failure message folded two
  opposite verdicts into one sentence, because `Venue::wait_for_exit` clamps its
  bound to the test's remaining wall budget, so the wait may have been far
  shorter than the 10 s it reported; the helper now reports how long it waited
  and which bound produced that. Host contention is the boring reading to rule
  out first. If the clamp is not it, the two candidates are `spawn_blocking` work
  in flight at signal time - the sweeper's tape walk and the boatyard's
  `worker.join()` both run there, and a dropped tokio runtime waits for blocking
  tasks that have started - and the boat worker's responsiveness to its cancel
  flag. The completion-path session wait does not touch this: a signal
  deliberately does not wait for sessions. When it fires it aborts the
  instrumented sweep and the gate then reports every `mogwai-data` test as
  orphaned; the tell is the orphan count equalling the missing sweep's pass
  count.

## Measurement and method still owed

- The price-span-per-inferred-match-event measurement. Triggered stops and plain
  market orders slip by the existing fill band, reused rather than separately
  fitted, and how wide that band should be for a triggered stop is a scale
  question that has never been computed. The sweep tail quoted elsewhere - up to
  2,213 aggTrade rows in one inferred event on BTC - counts rows rather than
  distinct prices, so it does not establish how far a marketable order actually
  walks. One probe extension over archives already on disk would settle it; until
  then the slippage magnitude is an unquantified mechanism shared by every order
  type that slips.

- Build a positive dead-feed watchdog (formerly sweep item AD12). No liveness
  timer, tick counter or "0 ticks in N s" log exists on either transport. The
  negative diagnostics are all in place, but nothing positively proves a
  subscribed feed is alive rather than genuinely quiet. `idle_timeout_ms` does
  not cover it: it defaults to 0, and even armed the idle clock resets on any
  application frame, so a data-silent-but-frame-active socket never trips it -
  deliberately, since that is what reproduces the 4255 case. The threshold comes
  from the landed default-tape dwell bound (the realism gate's era-windowed p999
  gap, empty-hour fraction and longest empty-hour run), and an armed
  LiquidityDrought legitimately silences the feed while remaining visible via the
  control plane, so the watchdog can account for it.

  Priority note: a real venue failure mostly shows up as a crashed or stalled PID
  rather than a protocol event, and the silent-but-socket-alive failure this was
  designed for was largely a property of the deleted long-lived daemon. Useful,
  not structural.

- The cadence document invalidated a currently-green gate, the 0.1603 duration
  ACF anchor, without naming a successor. That debt belongs to whichever spec
  descends from it.

- Per-instrument fitting belongs to whoever builds each preset: whether BTC and
  ETH genuinely differ enough to warrant different values - the measured 2.8x
  dispersion across three crypto majors suggests so, and one month of one venue
  cannot settle it. The evidence asymmetry stays relevant to preset authors: BTC,
  ETH and SOL have trade-level archives while MNQ and MES had 15-second bars and
  nothing else, so a CME preset's cadence is derived arithmetic and its
  clustering comes from nowhere. Each preset says where its numbers came from.
  Re-derive the asymmetry from the DBN bulk download now on disk before repeating
  it.

- Candidate symbols for the missing session classes: a perp like ETHUSDT.P, a
  second CME future like MGC, and AAPL for cash-equity hours. Terabytes of DBN
  data are already downloaded on another host, and the Databento account
  additionally holds about twelve months of MNQ/ES/MES tbbo plus mbp-1
  server-side, re-fetchable by job id at no new cost.

- Decide whether the protocol-12b Stage A refinement pass should run at all.
  Deferred by the owner 2026-08-09 rather than settled, so the frozen pass stands
  and the budgets were raised to fund it.

  For cutting: refinement is 29,200 s of the 35,526 s Stage A cost model, 82
  percent, and its entire product is a finer loss ordering over cells that Stage
  B then truncates to `STAGE_B_CELL_CAP = 24` per family. It cannot rescue a
  family whose coarse admissible region is empty - the outcome that would close
  the landing - because it subdivides around that region's own boundary cells.
  And `SELECTION_INDIFFERENCE = 0.01` already declares losses inside that margin
  as not separating candidates, so a half-spacing lattice buys precision the
  selection is defined not to use. Cutting drops Stage A to about 6,326 s.

  Against: the selected parameter point would sit on the coarse lattice, and
  nobody has shown the coarse spacing is fine enough for the mechanism to be
  found at all.

  Not the same question as `STAGE_B_CELL_CAP`, which earns its place: a Stage B
  cell is a full month-scale walk per seed at about 250 s, so an uncapped
  1,508-cell region genuinely is tens of hours. Changing `REFINEMENT_DEPTH` or
  `REFINEMENT_CELL_CAP` is a section 17 amendment against the contract of record.

- Reconcile the protocol-12b section 5.5 rescale with the shipped preset
  convention. That section freezes the negative control's re-centring as
  "rescale the 24 values to sum to 1, which the `SessionProfile` schema
  requires", and the schema requires no such thing: nothing in `config.rs` or
  `session.rs` enforces sum-to-one, and the shipped MNQ `intensity_hour` sums to
  23.862306, a mean-one curve. It moves no generated rate either way, since
  `SessionModulator::new` divides by an exposure-weighted normalizer so a common
  factor cancels at every instant, and the control's committed `new_curve` is
  therefore a correct re-centring on a different scale from its own `old_curve`.
  What it cost is readability, and it will cost the same again at any later
  reader who compares the two curves elementwise. Fixing the frozen sentence is a
  section 17 amendment through review, not an edit.

## Values that want to become named constants or knobs

- `ReopenGap`'s `halt_secs > 86_400` bound is inline in `validate_*`, the one
  temporal bound in `mogwai-protocol` with no named const behind it, unlike its
  sibling `MAX_DIVERGENCE_MS`.
- `mogwai-protocol`'s `default_instruments()` BTCUSDT seed is seven inline
  literals, duplicated verbatim in two of that crate's own tests, and the smoke
  test's fixed order shape depends on it implicitly. Its own doc comment
  signposts multi-instrument growth.
- `mogwai-venue`'s HTTP route strings are inline literals with no shared registry
  against the adapter's route segments, so a renamed route breaks the pair
  silently.
- `mogwai-venue`'s test-side `HORIZON_S 86_400.0` stands in for the production
  `warmup_ns` default as a plain literal and can drift from it silently.
- `mogwai-venue`'s channel capacity `1024` is duplicated inline for the writer
  channel and the exec-delay pump - different traffic classes, so either they
  share a const or they get two named ones.
- `mogwai-adapter`: `wait_connected` re-hardcodes a 5s/10ms pair matching
  `ACCOUNT_REGISTRATION_TIMEOUT` and `_POLL` by value without sharing them, and
  `1_000_000_000` appears inline five or more times across `client.rs` and
  `lifecycle.rs` where a `NANOS_PER_SEC` const belongs.
- `mogwai-data`'s `1e9` mid-price runaway ceiling is duplicated at two sites.
- `arrival_screen`'s `DEFAULT_MAX_JOBS` carries no comment naming the measurement
  behind it, and `arrival_envelope_diagnostic` applies no 16-job cap at all while
  `arrival_screen` caps its default at 16 for a measured SMT regression. Whether
  the diagnostic should share the cap is open.
- `MIN_WALL_REQUEST_TIMEOUT_SECS` in the adapter is flagged in its own comment as
  the tightest cap on usable sim speed. If sim speed is ever pushed hard, that
  constant is the first wall.

## Owner rulings still open

- `Balance.locked` carries order holds, maintenance collateral and unsettled
  credits in one wire number with opposite remedies. Two scopes recommend a
  split; `Account::unsettled`'s doc in `mogwai-engine` argues the conflation is
  fine.
- Whether the evidence toolbox stays on the binary's top level - eighteen
  subcommands for one audience beside three for another - and the repeated leaf
  names inside it: `preflight` is three different commands and `fit` is two, all
  operator-typed, all producing plausible evidence output, with no collision
  warning in `docs/cli.md`.
- Whether `leg`, one of the two connections a nautilus consumer necessarily holds
  under one account, gets a glossary entry.
- The tape-identity vocabulary: river is the sequence, tape the delivery, and the
  process the version constant identifies has no name. Adjacent and real:
  `reference/architecture.md`'s version narrative walks 5 through 18 and then
  asserts the current identity, six unnarrated bumps.
- The `held` collision: `exec_held_budget_bytes` and the "held lane" use the word
  for the outbound byte-budget sense that kept `reservation`, at a
  consumer-visible config key.
- `mogwai-engine`'s fourth sense of admission - acceptance of an order onto the
  book, about 60 sites - is unruled and must not be swept as retired.
- `stage_m_tier2.rs`'s append-only candidate ledger is an unruled sixth sense of
  ledger, and `mogwai_lab::delivery` still owns the git-cleanliness oracle
  (`TreeOracle` and kin), a second unrelated job under a module named for the
  delivery manifest.
- `reference/glossary.md`'s Strategy entry says "single-instrument by settled
  premise", which sits oddly beside the Account entry's many-rivers model. The
  glossary is owner-only, so this is a question rather than an edit.

## Documentation owed

- Durable prose is owed for the account, river and passenger design. These notes
  carry no truth guarantee and nothing durable may cite them, so any part of the
  design whose reasoning lives only here is invisible to a user. Owed: the symbol
  as a label rather than an identity; the three-step resolution and its total
  third step; river identity and what forks a river; one clock per river; the
  exogeneity that gives passengers non-interference and the no-queue-competition
  contract that follows; and the boot-versus-runtime split on funding. Durable
  prose states river and passenger and never the boat, which is a cache with no
  semantics, and states the two properties a passenger is owed separately.
  `docs/presets.md` and `docs/config.md` are where a user looks;
  `reference/architecture.md` is where the why belongs.

- Half of `mogwai --help` is protocol jargon resolving to retired notes: "Brick
  B4", "Stage M", "Amendment 2" reach an operator who has no way to look them up.

- `docs/cli.md` cites "the storage policy" three times as a named authority no
  document defines.

- Structural proposals recorded and unadopted: `reference/architecture.md` is
  about 1,300 lines doing four jobs, and its contradictions have all sat where
  one job's old text survived another's landing; `docs/havoc.md` was patched
  rather than rewritten.

- Glossary and vocabulary standing rules: glossary entries are admitted by the
  owner alone, so an agent that meets an undefined load-bearing word escalates
  and never adds an entry. Vocabularies that want definitions want them in
  `reference/`, not in the glossary.

## Open upstream: nautilus_trader

Read the source from `research/nautilus_trader`; build against the pinned
crates.io release. Nothing here can be fixed from this tree.

- **`ExecutionEventEmitter` cannot share its sender**, so this adapter can only
  refuse rather than heal. The emitter derives `Clone` and owns `sender:
  Option<UnboundedSender<ExecutionEvent>>` by value, installed once from
  `try_get_exec_event_sender()`, which reads a `thread_local!` in
  `nautilus_common::live::runner` set on the runner's thread. Every clone taken
  after that point freezes the sender state of the instant it was taken, and
  `send_order_event` on a sender-less clone only logs a warning.

  What the other side would have to ship: an emitter holding its sender behind a
  shared cell, or resolving it per send from a process-wide rather than
  thread-local slot, so a clone taken before `set_sender` still emits. Our
  workaround is a refusal, not a repair - a host that starts its clients on one
  thread and connects them on another gets a named error rather than a working
  client - and this item dies when nautilus ships a shareable emitter.

- **No channel for a declared feed gap.** Mogwai's `VenueMessage::FeedLagged`
  carries `skipped` and `sim_now_ns`, which is a strictly better signal than most
  real venues give, and the adapter has nowhere to put it. No `DataEvent` variant
  means "the stream you are aggregating has a hole", the client is handed to the
  host boxed as `dyn DataClient` so an adapter-owned counter or health accessor
  is unreachable, and `is_connected` is true throughout because the socket never
  broke. Bar aggregation over the missing span is silently wrong and the polling
  cursor resumes past it, so a strategy cannot distinguish a quiet market from a
  dropped one.

  The execution socket cannot self-heal either, and for a different reason.
  `ExecutionEvent::Report` exists, so the emitter is not the missing piece; what
  is missing is a truthful report to push. Every truthful set in this adapter
  comes from an asynchronous venue-truth query, and the frame translator that
  sees `FeedLagged` runs as `handler(msg).await` inside the reader's own frame
  loop, so a query issued there can only be read by the loop awaiting the
  handler - a deadlock by construction. Spawning it off is unavailable too, since
  the client owns `Rc<RefCell<Cache>>` through `ExecutionClientCore` and is
  `!Send`. Fabricating from the local mirror is the exact falsehood the
  venue-truth move removed, since the mirror is built from the frames the venue
  just said it dropped.

  What nautilus would have to ship, either half closing half of this: (1) a
  data-side degradation signal, a `DataEvent` variant or a `DataClient` health
  callback the engine surfaces, so a gap is an event rather than a log line; and
  (2) an execution-side client-initiated reconciliation request the
  ExecutionEngine services on the client's behalf, so the adapter can say "my
  mirror is suspect, re-run mass status" without owning an async handle to
  itself. Until they exist, a host driving mogwai should treat an error from
  `mogwai-adapter` mentioning a feed gap or a refused frame as a
  reconcile-and-distrust-the-window signal.

- **The Rust trait default for mass status does not compose** the way the Python
  base does. Queued in the maintainer's PR tracker. Not a substitute for our own
  reconciliation guard: mogwai overrides the method, so this protects the next
  adapter author rather than this repo.

- **Tape sparsity has no attribution channel.** An empty historical window is
  correct behaviour here - the fitted ACD arrival process is persistent and
  heavy-tailed, so a short window can legitimately hold zero trades and
  `/trades` correctly answers `200 []` - but it still costs the consumer a fatal
  halt, and one of the two fixes is blocked on the same gap as `FeedLagged`: an
  empty historical response carries no feed identity, so it cannot be attributed.

## Open at broadarrow

- **Owed: tell them, in one message. Nobody has.** The whole account surface
  moved under them and several entries below are stale in their favour.
  - The break: they set no `account_type`, inherit `MOGWAI-001`, POST no account,
    and have no handling for a run that ends by liquidation. Their orchestrator
    runs the shared shape, so 50 subagents inheriting `MOGWAI-001` would take
    each other's ledger in turn; the id belongs in their per-subagent account
    TOMLs. The account-id contract is in `docs/config.md`: a client on a shared
    venue names its own account, and a client that spawns its own ephemeral venue
    owes nothing. That break is designed, but designed-to-break only works if the
    other side is told it is coming.
  - The second break: an expired order now reports `VenueMessage::OrderExpired`
    with a terminal `Expired` status where it reported `OrderCanceled`. Exhaustive
    matching stops compiling; loose matching stops seeing `Day` and `Gtd` orders
    end at all, which is the dangerous reading and the reason this belongs in a
    message rather than a changelog nobody diffs.
  - In their favour, same message: trailing stops, the full order-type surface
    including `TrailingStopLimit`, order lists (so the two-independent-legs
    workaround is no longer required) and `RejectNextCancel` are all served, so
    their three unrun scenario files can now be written. `translate_trailing_exit`
    can emit the limit form as well as the market one; the venue derives the
    limit price from a `limit_offset`, so they send an offset and not a price.

- **Item 4 of the strategy-search route, consuming the multi-instrument venue.**
  `run_prep::mogwai_facts` refuses a `/instruments` answer of anything but
  exactly one instrument, precisely so a relaxed mogwai breaks their build loudly
  instead of having broadarrow pick an instrument arbitrarily. Closing it means
  selecting by the strategy's frontmatter `MOGWAI:<symbol>`, per worker rather
  than per venue, after which the readiness record's `symbol` field needs its
  one-venue-one-symbol meaning reconciled.

- `POST /accounts` at run-prep preflight, so each worker opens its own ledger
  with its own balances before the node is built. Nothing here blocks it.

- Their profile row becomes `AtomicOuo` and brick 3 of
  `notes/venue-order-list-oco-spec.md` lands. Note the carve-out they must read
  before citing the group-admission guarantee: a member whose funds an earlier
  member's fill consumed is rejected on the second pass with its earlier siblings
  already accepted, so admission is atomic for everything the venue can decide in
  advance and not for a balance the group's own fills moved.

- Decide whether a refusal marked `RETRYABLE_REJECT_PREFIX` should be treated as
  retryable at all. Their standing reasoning - a rejection wrongly treated as
  retryable is worse than a run that stops when the venue said no - is still
  sound, and the marker only changes what the decision rests on. Nothing here
  pushes them either way.

- Boot-storm pacing for concurrent `/trades` and `/quotes` warmup, because their
  daemon decides when workers spawn. Our bounded wait makes staggering an
  optimization for ordinary paging rather than a precondition of correctness,
  which is the change worth telling them about.

- Undecided and theirs: `submit_order_list` is the only route that emits a group
  frame, so a consumer wanting an atomic group by any other route has no API for
  it. None is owed until one is wanted.

- Their own repo, listed so the coordination is not lost: (a) the feed-stale
  message hard-codes the issue-4255 hypothesis ("the connection looks
  healthy...") as fact even when the venue process is dead; (b)
  `reference/mogwai.md` and `ba man mogwai` still describe the venue as
  unfundable, stale since the `[balances]` seed landed; (c) stored scenario TOMLs
  setting `transport_profile` on either adapter config no longer parse, since the
  field went with `TransportProfile` itself, and want a sweep.

### Runs owed against mogwai

Theirs to run, not ours to build, but each is a venue exercise that would surface
mogwai defects, and several have been owed for weeks.

- The restart run, the realized-PnL baseline, legs 1 to 3: serve durably, trade
  to a non-zero realized figure, SIGKILL the worker, re-run against the same
  `[attach]` scenario, verify the carried baseline, the brake mark, and no
  duplicate booking. Leg 3 is load-bearing and rests on a verdict reached by
  reading the dependency rather than by observing a reconciliation, landed as an
  explicit operator override of its own gate - a known-unrun verification on a
  capital bound.
- `go_live` restart de-duplication: kill a non-flat worker with orders resting at
  the durable venue, restart, verify the batch de-duplicates against the
  surviving book.
- The futures run against a `preset = "MNQ"` venue: warmup, fed fills, a resting
  stop triggering on the multiplied instrument, a settlement-currency commission
  actually charged, and the brakes marking in that currency.
- The conditional half of the fed-fill path: a fed fill from an order that
  genuinely rested and then filled at venue timing, ideally under havoc.
- Flip plus pyramid plus partial in one bar, end to end.
- Gate B, the anchored-warmup overlap drop. Their `handoff.rs` covers Binance,
  Kraken and Bybit but not mogwai, and is a consistency test rather than ground
  truth.
- The poll-heal end-to-end test, which drives our control plane directly: rest a
  far-from-market limit, POST `CancelOpenOrderSilently`, assert the local order
  converges to Canceled within the retry ladder's bound. Their fixture notes
  still hold: carry no protective exits, and census the whole rotated log family.

## Notes and gotchas

- The CLA check is not yet a required status check. cla-assistant.io is wired up
  and its webhook delivers, but nothing blocks an unsigned merge until a
  repository ruleset requires the check by name. The trap: an owner-authored PR
  produces no status at all, since the CLA assigns copyright to the owner and the
  bot correctly has nothing to ask, which means the check cannot be picked from
  the suggestion list and cannot be validated against a real run. Type the
  context name in by hand and leave the rule in evaluate mode until an outside PR
  confirms it, because a required check that never reports blocks every merge
  with no visible cause.
