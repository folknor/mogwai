// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! State owned by one venue process: one ledger, and keyed paced boats over
//! many rivers.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use mogwai_engine::{Engine, EngineConfig};
use mogwai_protocol::{CommandClass, InstrumentDef, RunSeeds, SimClock, Symbol};
use rust_decimal::Decimal;
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::{admission::ExecLanes, boatyard::Boatyard, source};

/// A havoc window, armed at a WALL instant for a SIMULATED span, judged on
/// whatever clock the reader owns.
///
/// Stored as `(wall_armed_ns, sim_span_ns)` rather than as an absolute sim
/// deadline because the venue has no single sim axis to express a deadline on:
/// the same window must mean `ms` simulated milliseconds to a passenger on a
/// fast boat and to one on a slow one. The armer cannot know who will read it
/// either - a passenger may board afterwards - so the window carries no clock
/// and every reader opens it on its own.
///
/// Behind a `Mutex`, not two atomics. Arming is a cold path - an operator
/// control - and two independent `AtomicU64`s are a TORN READ: a concurrent
/// reader can pair the new wall instant with the old span, and a clear can race
/// a re-arm and erase the new span. The single `AtomicU64` this replaces was
/// tear-free by construction, so an atomic pair would be a regression
/// introduced by the fix. No packed encoding either: two independent nanosecond
/// quantities do not fit one u64 without a range limit nobody can audit later.
pub(crate) struct HavocWindow(Mutex<Option<ArmedSpan>>);

#[derive(Clone, Copy)]
struct ArmedSpan {
    wall_armed_ns: u64,
    sim_span_ns: u64,
}

impl HavocWindow {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// STORE, not extend: the whole span is replaced under the lock, so
    /// re-arming with a shorter span shortens an in-flight window.
    pub(crate) fn arm(&self, wall_armed_ns: u64, sim_span_ns: u64) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ArmedSpan {
            wall_armed_ns,
            sim_span_ns,
        });
    }

    /// The cleared state, which is closed on EVERY reader's clock - the
    /// property the old `0` deadline sentinel had, now expressed as absence.
    pub(crate) fn clear(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Judged on the reader's own clock.
    ///
    /// THE LATE-BOARDER RULE: the opening instant is
    /// `max(sim.sim_ns(wall_armed_ns), sim.sim_epoch_ns)`. Projecting the
    /// arming instant through the clock of a boat anchored LATER than the arm
    /// would put the window in that boat's past, where it never opens - arm a
    /// blackout, connect 50 ms later, and the blackout silently does not
    /// happen. Such a reader instead treats its own epoch as the opening and
    /// consumes the FULL span.
    pub(crate) fn open_at(&self, sim: SimClock, sim_at_ns: u64) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|span| {
                let opening = sim.sim_ns(span.wall_armed_ns).max(sim.sim_epoch_ns);
                sim_at_ns >= opening && sim_at_ns < opening.saturating_add(span.sim_span_ns)
            })
    }
}

/// One connected trader: its own account, its own ledger, its own orders.
///
/// This is the noun the venue serves. A river is the tape and is shared; a
/// passenger is never shared, because the moment a ledger hangs off something
/// two connections have in common they share a balance and a position book.
///
/// The engine is per passenger for exactly that reason. It was one per PROCESS,
/// which is right while a venue serves one run and wrong the moment an
/// orchestrator points fifty subagents at one exchange: every subagent's fills
/// moved every other subagent's net.
pub(crate) struct Passenger {
    pub(crate) account_id: mogwai_protocol::AccountId,
    pub(crate) engine: AsyncMutex<Engine>,
}

impl std::fmt::Debug for Passenger {
    /// The account only. A ledger's contents are not something a diagnostic
    /// should splice into a log line, and the engine is behind an async mutex a
    /// formatter cannot take.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Passenger")
            .field("account_id", &self.account_id.as_str())
            .finish_non_exhaustive()
    }
}

/// What every passenger's ledger is opened from. Held on the run because a
/// passenger is created on demand - when a connection first names an account -
/// and the values it is built from are venue configuration.
///
/// This is the venue-wide `[balances]` seed, which the account-policy design
/// retires in favour of a client-named opening balance and risk policy. Until
/// that lands it is the OPENING balance applied to each passenger rather than
/// the balance of one shared ledger, which is the same value doing a different
/// job.
struct LedgerTemplate {
    opening_balances: std::collections::HashMap<String, Decimal>,
    fill_seed: u64,
    oms_type: mogwai_protocol::OmsType,
    fill_band_max_ticks: u32,
}

pub(crate) struct Run {
    /// The shape this run placed its BOOT boat on, and the river a `/ws`
    /// upgrade binds when it names no symbol. Every configured shape has a
    /// river in `rivers`, is servable for history, and gets a boat of its own
    /// when a passenger boards it; this one is only distinguished by carrying
    /// a boat from boot, and therefore by never lagging the venue clock.
    pub(crate) boot_symbol: Symbol,
    /// Every configured river, created on first use and keyed independently, so
    /// two symbols never serialize on each other's checkpoint chain.
    pub(crate) rivers: Arc<source::Rivers>,
    pub(crate) oms_type: mogwai_protocol::OmsType,
    /// Every account this venue has served, created on demand and keyed by
    /// account id. An id is the CLIENT'S to choose and outlives its connection,
    /// so a returning socket finds its own ledger rather than a fresh one; that
    /// is what makes a reconnect a continuation instead of a new trader.
    passengers: Mutex<std::collections::HashMap<String, Arc<Passenger>>>,
    template: LedgerTemplate,
    /// The account a connection that names none is served under. It exists for
    /// the ephemeral single-client venue, where making the one client name an
    /// id would be ceremony; it is NOT the venue's one account.
    default_account_id: mogwai_protocol::AccountId,
    pub(crate) seeds: RunSeeds,
    pub(crate) boatyard: Arc<Boatyard>,
    boot_ticket: Mutex<Option<crate::boatyard::Ticket>>,
    /// The VENUE clock, and not the now of any seated river. It is the venue's
    /// one wall-to-sim reference, kept for the three answers no boat can give:
    /// a boatless river's history ceiling, the venue deadline, and the
    /// venue-scoped account ledger. Owned HERE rather than beside the router
    /// state: a run has one such clock, and a second copy in the HTTP state is
    /// a second thing that could be re-anchored independently of the tape it
    /// dates.
    pub(crate) sim: SimClock,
    /// Sim placement origin for every boat and epoch every duration is measured
    /// from. Computed before any river's warmup is materialized.
    pub(crate) started_ns: u64,
    /// Sim instant at which the run stops itself, or `None` for indefinite.
    /// Equals `started_ns + run_duration_ns`.
    pub(crate) deadline_ns: Option<u64>,
    /// Uniform servable sim span before `started_ns`. The boot river is
    /// materialized before readiness; other rivers materialize it on first read.
    pub(crate) warmup_ns: u64,
    pub(crate) delay_ms: AtomicU64,
    pub(crate) submit_act_ms: AtomicU64,
    pub(crate) modify_act_ms: AtomicU64,
    pub(crate) cancel_act_ms: AtomicU64,
    pub(crate) submit_ack_ms: AtomicU64,
    pub(crate) modify_ack_ms: AtomicU64,
    pub(crate) cancel_ack_ms: AtomicU64,
    pub(crate) dark: HavocWindow,
    pub(crate) stall: HavocWindow,
    complete_tx: watch::Sender<Option<(u64, u64)>>,
    /// Every live connection's outbound lanes, so venue-ORIGINATED output - a
    /// trigger fill nobody commanded - reaches the connection it belongs to.
    ///
    /// This was a BROADCAST target: the argument was that with a single ledger a
    /// fill is a fact about the run rather than about whichever socket submitted
    /// the order. That is true of the ledger and false of the client, which is
    /// told about orders it never placed. Delivery is now ATTRIBUTED through
    /// `order_owners` below; the table stays a list of every live connection
    /// because venue-wide frames (the account snapshot, a venue fault) still go
    /// to all of them.
    lanes: Mutex<Vec<BoundLane>>,
    /// Which connection submitted each live order, so a sweep-produced fill goes
    /// to the passenger that owns it and to nobody else.
    ///
    /// Keyed by `VenueOrderId` because that is what every sweep-produced frame
    /// and every query row carries; the value is the ACCOUNT ID that submitted
    /// it. An order absent from this table - one the VENUE originated - is
    /// delivered to EVERY lane and visible to every query, which is the old
    /// behaviour and the conservative direction to fail in: a passenger seeing a
    /// stray frame is the defect this closes, but a passenger MISSING its own
    /// fill would be a worse one.
    ///
    /// BY ACCOUNT, NOT BY CONNECTION, and the difference is not cosmetic. A
    /// ledger belongs to an account, so two sockets presenting the same id are
    /// the SAME TRADER and must each see the whole account's orders; keying on
    /// the connection hid a client's own resting order from its own second
    /// socket. Different accounts is what invisibility is about.
    ///
    /// A claim survives its order's TERMINAL state. Retiring on the ending frame
    /// would bound the table more tightly and was the first shape of this, but
    /// it makes a closed order unattributed - so `QueryOrders` and `QueryFills`,
    /// which report terminal rows by design, would show every account's history
    /// to everyone.
    order_owners: Mutex<std::collections::HashMap<mogwai_protocol::VenueOrderId, String>>,
}

impl Run {
    #[expect(
        clippy::too_many_arguments,
        reason = "the boot-only values are explicit so the single-run ownership is visible at construction"
    )]
    pub(crate) fn new(
        instrument: InstrumentDef,
        rivers: Arc<source::Rivers>,
        balances: std::collections::HashMap<String, Decimal>,
        sim: SimClock,
        started_ns: u64,
        warmup_ns: u64,
        run_duration_ns: Option<u64>,
        seeds: RunSeeds,
        fanout_depth: usize,
        zero_speed_stall_ms: u64,
        oms_type: mogwai_protocol::OmsType,
        fill_band_max_ticks: u32,
        account_id: mogwai_protocol::AccountId,
        fault_tx: std::sync::mpsc::Sender<mogwai_data::TickFault>,
    ) -> Arc<Self> {
        let boatyard = Boatyard::new(
            Arc::clone(&rivers),
            fanout_depth,
            zero_speed_stall_ms,
            fault_tx,
            started_ns,
        );
        let (complete_tx, _) = watch::channel(None);
        // No engine is built here. A ledger belongs to a PASSENGER, and no
        // passenger exists until a connection arrives, so the run carries only
        // what one is opened from.
        Arc::new(Self {
            passengers: Mutex::new(std::collections::HashMap::new()),
            template: LedgerTemplate {
                opening_balances: balances,
                fill_seed: seeds.fill,
                oms_type,
                fill_band_max_ticks,
            },
            default_account_id: account_id,
            boot_symbol: instrument.symbol,
            rivers,
            oms_type,
            seeds,
            boatyard,
            boot_ticket: Mutex::new(None),
            sim,
            started_ns,
            deadline_ns: run_duration_ns.map(|duration| started_ns.saturating_add(duration)),
            warmup_ns,
            delay_ms: AtomicU64::new(0),
            submit_act_ms: AtomicU64::new(0),
            modify_act_ms: AtomicU64::new(0),
            cancel_act_ms: AtomicU64::new(0),
            submit_ack_ms: AtomicU64::new(0),
            modify_ack_ms: AtomicU64::new(0),
            cancel_ack_ms: AtomicU64::new(0),
            dark: HavocWindow::new(),
            stall: HavocWindow::new(),
            complete_tx,
            lanes: Mutex::new(Vec::new()),
            order_owners: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// The passenger trading under `account_id`, created on first sight.
    ///
    /// Creation is the whole lifecycle: an account outlives the connection that
    /// named it, so a second connection presenting the same id gets the SAME
    /// ledger, with its positions and order history intact. That is what a
    /// reconnect is from the venue's side, and it is indistinguishable from a
    /// stranger claiming the id, which is why nothing here tries to tell them
    /// apart.
    pub(crate) fn passenger(&self, account_id: &mogwai_protocol::AccountId) -> Arc<Passenger> {
        let mut passengers = self
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(seated) = passengers.get(account_id.as_str()) {
            return Arc::clone(seated);
        }
        // The ledger starts EMPTY of instruments. One becomes tradable when this
        // passenger binds a symbol or names it on an order, through
        // `ensure_instrument` - which is per passenger for the same reason the
        // engine is.
        let mut engine = Engine::build(EngineConfig {
            account_id: account_id.clone(),
            instruments: Vec::new(),
            balances: self.template.opening_balances.clone(),
            fill_seed: self.template.fill_seed,
        });
        engine.set_oms_type(self.template.oms_type);
        engine.set_liquidation_band_ticks(self.template.fill_band_max_ticks);
        let seated = Arc::new(Passenger {
            account_id: account_id.clone(),
            engine: AsyncMutex::new(engine),
        });
        passengers.insert(account_id.as_str().to_owned(), Arc::clone(&seated));
        seated
    }

    /// The passenger a connection that named no account is served under.
    pub(crate) fn default_passenger(&self) -> Arc<Passenger> {
        self.passenger(&self.default_account_id)
    }

    /// Open an account with a CLIENT-NAMED opening balance, before anything
    /// trades on it.
    ///
    /// This is step one of the three-step account resolution, the one where the
    /// client states its own terms. Steps two and three - a named policy preset,
    /// and the default account preset - are what a connection gets when it never
    /// calls this, which is why calling it is OPTIONAL and its absence is not an
    /// error anywhere.
    ///
    /// REFUSED IF THE ACCOUNT IS ALREADY OPEN, rather than resetting it. An
    /// account outlives the connection that named it, so re-opening one is
    /// ambiguous between "I am starting a fresh experiment" and "I reconnected
    /// and re-sent my config", and the second reading would silently wipe a
    /// live position book. A client that wants a clean ledger names a different
    /// id, which costs it nothing.
    pub(crate) fn open_account(
        &self,
        account_id: &mogwai_protocol::AccountId,
        balances: std::collections::HashMap<String, Decimal>,
    ) -> Result<(), AccountRefusal> {
        let mut passengers = self
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if passengers.contains_key(account_id.as_str()) {
            return Err(AccountRefusal::AlreadyOpen);
        }
        let mut engine = Engine::build(EngineConfig {
            account_id: account_id.clone(),
            instruments: Vec::new(),
            balances,
            fill_seed: self.template.fill_seed,
        });
        engine.set_oms_type(self.template.oms_type);
        engine.set_liquidation_band_ticks(self.template.fill_band_max_ticks);
        passengers.insert(
            account_id.as_str().to_owned(),
            Arc::new(Passenger {
                account_id: account_id.clone(),
                engine: AsyncMutex::new(engine),
            }),
        );
        Ok(())
    }

    /// The passenger whose book holds `client_order_id` as a RESTING order, and
    /// the symbol it rests on.
    ///
    /// The control plane names an order without naming an account, so the target
    /// has to be found. Ids are client-chosen and therefore not unique across
    /// passengers; the first match wins, which is a real ambiguity to close when
    /// the control plane grows an account parameter of its own.
    pub(crate) async fn passenger_holding(
        &self,
        client_order_id: &str,
    ) -> Option<(Arc<Passenger>, Symbol)> {
        for passenger in self.passengers() {
            let symbol = passenger
                .engine
                .lock()
                .await
                .open_order_symbol(client_order_id);
            if let Some(symbol) = symbol {
                return Some((passenger, symbol));
            }
        }
        None
    }

    /// Every passenger this venue has served, for the venue-wide walks: the fill
    /// sweeper, and the control plane's reach into every ledger.
    pub(crate) fn passengers(&self) -> Vec<Arc<Passenger>> {
        self.passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(Arc::clone)
            .collect()
    }

    pub(crate) fn retain_boot_ticket(&self, ticket: crate::boatyard::Ticket) {
        *self
            .boot_ticket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ticket);
    }

    /// Make `symbol` tradable on ONE PASSENGER'S ledger: register the def and
    /// install the margin policy and fee schedule from its profile. Called when
    /// a socket binds a symbol and before an order for it is admitted. An `Err`
    /// means no profile resolves it, and the caller lets the engine produce its
    /// own unknown-instrument rejection rather than inventing a second wording.
    ///
    /// Per passenger because the engine is: two traders on one venue each keep
    /// their own registered instruments, margin policy and fee schedule, and
    /// nothing one of them binds is visible in the other's ledger.
    ///
    /// This is the ONE path from a profile to engine policy - `Run::new` no
    /// longer has a copy - and the installs are guarded on the registration
    /// having been NEW, so re-binding a symbol a client is already trading never
    /// resets its configuration.
    pub(crate) async fn ensure_instrument(
        &self,
        passenger: &Passenger,
        symbol: &str,
    ) -> Result<Arc<crate::source::InstrumentProfile>, crate::source::ResolveRefusal> {
        let profile = self.rivers.resolve_profile(symbol)?;
        let mut engine = passenger.engine.lock().await;
        if !engine.ensure_instrument(profile.def.clone()) {
            return Ok(profile);
        }
        if let Some(margin) = profile.margin.clone() {
            engine.set_margin_policy(
                Arc::clone(&profile.def.symbol),
                mogwai_engine::MarginPolicy {
                    initial_per_contract: margin.initial_per_contract,
                    maintenance_per_contract: margin.maintenance_per_contract,
                    breach_action: match margin.breach_action {
                        crate::config::BreachAction::Refuse => mogwai_engine::BreachAction::Refuse,
                        crate::config::BreachAction::Liquidate => {
                            mogwai_engine::BreachAction::Liquidate
                        }
                    },
                },
            );
        }
        if let Some(fees) = profile.fees.clone() {
            let convert = |rate: crate::config::FeeRate| match rate {
                crate::config::FeeRate::BasisPoints { rate } => {
                    mogwai_engine::FeeRate::BasisPoints { rate }
                }
                crate::config::FeeRate::PerContract { amount } => {
                    mogwai_engine::FeeRate::PerContract { amount }
                }
            };
            engine.set_fee_schedule(
                Arc::clone(&profile.def.symbol),
                mogwai_engine::FeeSchedule {
                    maker: convert(fees.maker),
                    taker: convert(fees.taker),
                },
            );
        }
        Ok(profile)
    }

    /// Enrol one connection's lanes for venue-originated output. The returned
    /// id is what `release_lanes` retires, so a reconnecting client cannot
    /// retire the lanes of the connection that replaced it.
    ///
    /// The id is the LANES' OWN, minted when they were constructed rather than
    /// here. That is what lets `process_order_cmd` - which is handed the lanes
    /// and nothing else naming the connection - record who owns an order it just
    /// saw accepted.
    pub(crate) fn bind_lanes(&self, lanes: ExecLanes, account_id: &str) -> u64 {
        let id = lanes.id();
        self.locked_lanes().push(BoundLane {
            id,
            account_id: account_id.to_owned(),
            lanes,
        });
        id
    }

    /// Retire one connection. The ACCOUNT'S order claims survive: an account
    /// outlives every connection that ever presented it, so a returning socket
    /// must still find its own orders attributed to it.
    pub(crate) fn release_lanes(&self, id: u64) {
        self.locked_lanes().retain(|bound| bound.id != id);
    }

    /// Record that the account `owner` submitted `venue_order_id`.
    pub(crate) fn claim_order(&self, venue_order_id: mogwai_protocol::VenueOrderId, owner: &str) {
        self.order_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(venue_order_id, owner.to_owned());
    }

    /// Which account submitted `venue_order_id`, or `None` for an order nobody
    /// claimed.
    pub(crate) fn order_owner(
        &self,
        venue_order_id: &mogwai_protocol::VenueOrderId,
    ) -> Option<String> {
        self.order_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(venue_order_id)
            .cloned()
    }

    /// Claim every order a just-produced batch accepted for `owner`.
    ///
    /// Only the command dispatcher calls this. A sweep-produced batch is never
    /// claimed: an order appearing there was originated by the VENUE - a margin
    /// liquidation - so there is no submitting connection to attribute it to,
    /// and inventing one would address its frames to a lane that never asked.
    pub(crate) fn track_ownership(&self, events: &[mogwai_protocol::ServerMessage], owner: &str) {
        for event in events {
            if let mogwai_protocol::ServerMessage::OrderAccepted { venue_order_id, .. } = event {
                self.claim_order(venue_order_id.clone(), owner);
            }
        }
    }

    /// Drop the rows of a query reply that belong to another connection.
    ///
    /// The engine answers `QueryOrders` and `QueryFills` from ONE book, because
    /// there is one ledger; scoping happens here, where the connection is known.
    /// A row for an unclaimed order stays, on the same rule the delivery filter
    /// uses: better a stray row than a missing one, and a venue-originated
    /// liquidation genuinely concerns whoever holds the position.
    pub(crate) fn scope_query_rows(
        &self,
        events: &mut [mogwai_protocol::ServerMessage],
        owner: &str,
    ) {
        use mogwai_protocol::ServerMessage as M;
        let mine = |venue_order_id: &mogwai_protocol::VenueOrderId| {
            self.order_owner(venue_order_id)
                .is_none_or(|found| found == owner)
        };
        for event in events {
            match event {
                M::OrderStatusSnapshot(snapshot) => {
                    snapshot.orders.retain(|row| mine(&row.venue_order_id));
                }
                M::FillSnapshot(snapshot) => {
                    snapshot.fills.retain(|fill| mine(&fill.venue_order_id));
                }
                _ => {}
            }
        }
    }

    /// The lanes to deliver one venue-originated batch to. Cloned out under the
    /// lock rather than held across the delivery: delivery serializes JSON and
    /// touches per-connection budgets, and doing that while holding a run-wide
    /// mutex would let one connection's cost block every other's teardown.
    pub(crate) fn bound_lanes(&self) -> Vec<BoundLane> {
        self.locked_lanes().clone()
    }

    fn locked_lanes(&self) -> std::sync::MutexGuard<'_, Vec<BoundLane>> {
        self.lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Earliest sim instant the tape can serve.
    pub(crate) fn data_origin_ns(&self) -> u64 {
        source::TAPE_ORIGIN_NS
    }

    /// Announces the one planned terminal transition.  Receivers get the
    /// simulated instant and elapsed duration before the listener is drained.
    pub(crate) fn complete(&self, sim_now_ns: u64, elapsed_ns: u64) {
        if self
            .complete_tx
            .send(Some((sim_now_ns, elapsed_ns)))
            .is_err()
        {
            tracing::debug!("run completed after all websocket receivers closed");
        }
    }

    pub(crate) fn completion(&self) -> watch::Receiver<Option<(u64, u64)>> {
        self.complete_tx.subscribe()
    }

    pub(crate) fn act_ms(&self, class: CommandClass) -> u64 {
        match class {
            CommandClass::Submit => self.submit_act_ms.load(Ordering::Relaxed),
            CommandClass::Modify => self.modify_act_ms.load(Ordering::Relaxed),
            CommandClass::Cancel => self.cancel_act_ms.load(Ordering::Relaxed),
        }
    }
    pub(crate) fn ack_ms(&self, class: CommandClass) -> u64 {
        match class {
            CommandClass::Submit => self.submit_ack_ms.load(Ordering::Relaxed),
            CommandClass::Modify => self.modify_ack_ms.load(Ordering::Relaxed),
            CommandClass::Cancel => self.cancel_ack_ms.load(Ordering::Relaxed),
        }
    }
}

/// Why an account could not be opened on the terms asked for.
#[derive(Debug)]
pub(crate) enum AccountRefusal {
    /// Something already trades under this id. Never a reset: see
    /// `Run::open_account`.
    AlreadyOpen,
}

impl std::fmt::Display for AccountRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyOpen => f.write_str(
                "this account is already open; an account outlives its connections, so it is \
                 never re-opened with new terms - name a different account id for a fresh ledger",
            ),
        }
    }
}

/// One live connection: its identity, the account it trades under, and its
/// outbound machinery. The account rides along because delivery is attributed
/// by ACCOUNT - two sockets on one account are one trader and both hear about
/// its fills - while retirement is per connection.
#[derive(Clone)]
pub(crate) struct BoundLane {
    pub(crate) id: u64,
    pub(crate) account_id: String,
    pub(crate) lanes: ExecLanes,
}

/// The order a frame is ABOUT, for account attribution. `None` means the
/// frame is not order-scoped - a venue fault, a completion, the account
/// snapshot - and belongs to every connection.
///
/// The two `Option<VenueOrderId>` rejections are addressed only when the venue
/// recognized the order: a rejection naming an id the venue never issued cannot
/// be attributed, so it goes to everyone. That is right rather than merely
/// conservative, because the only connection that could care is the one that
/// asked, and it is in that set.
pub(crate) fn addressed_order(
    event: &mogwai_protocol::ServerMessage,
) -> Option<&mogwai_protocol::VenueOrderId> {
    use mogwai_protocol::ServerMessage as M;
    match event {
        M::OrderAccepted { venue_order_id, .. }
        | M::OrderTriggered { venue_order_id, .. }
        | M::OrderCanceled { venue_order_id, .. }
        | M::OrderUpdated { venue_order_id, .. } => Some(venue_order_id),
        M::OrderModifyRejected { venue_order_id, .. }
        | M::OrderCancelRejected { venue_order_id, .. } => venue_order_id.as_ref(),
        M::OrderFilled(fill) => Some(&fill.venue_order_id),
        _ => None,
    }
}

#[cfg(test)]
mod havoc_window_tests {
    use super::HavocWindow;
    use mogwai_protocol::SimClock;
    use std::sync::Arc;

    fn sim(anchor: u64, speed: f64) -> SimClock {
        SimClock {
            sim_epoch_ns: 1_000,
            wall_anchor_ns: anchor,
            speed,
        }
    }

    #[test]
    fn arming_replaces_rather_than_extends() {
        let window = HavocWindow::new();
        window.arm(10, 100);
        window.arm(20, 5);
        assert!(window.open_at(sim(0, 1.0), 1_022));
        assert!(!window.open_at(sim(0, 1.0), 1_026));
    }

    #[test]
    fn a_cleared_window_is_open_for_no_clock() {
        let window = HavocWindow::new();
        window.arm(10, 100);
        window.clear();
        assert!(!window.open_at(sim(0, 1.0), 1_010));
    }

    #[test]
    fn the_same_window_spans_equal_sim_time_on_two_different_speeds() {
        let window = HavocWindow::new();
        window.arm(10, 100);
        let slow = sim(0, 1.0);
        let fast = sim(0, 10.0);
        let slow_open = slow.sim_ns(10);
        let fast_open = fast.sim_ns(10);
        assert!(window.open_at(slow, slow_open + 99));
        assert!(window.open_at(fast, fast_open + 99));
        assert!(!window.open_at(slow, slow_open + 100));
        assert!(!window.open_at(fast, fast_open + 100));
    }

    #[test]
    fn a_reader_anchored_after_the_arm_opens_at_its_own_epoch() {
        let window = HavocWindow::new();
        window.arm(10, 100);
        let late = sim(20, 5.0);
        assert!(window.open_at(late, late.sim_epoch_ns));
        assert!(!window.open_at(late, late.sim_epoch_ns + 100));
    }

    #[test]
    fn concurrent_arm_clear_and_read_never_observe_a_torn_span() {
        let window = Arc::new(HavocWindow::new());
        let writer = Arc::clone(&window);
        let handle = std::thread::spawn(move || {
            for i in 0..10_000 {
                writer.arm(i, i.saturating_add(1));
                writer.clear();
            }
        });
        let clock = sim(0, 1.0);
        for i in 0..10_000 {
            let _ = window.open_at(clock, clock.sim_epoch_ns.saturating_add(i));
        }
        handle.join().unwrap();
    }
}

/// A run over the BTCUSDT preset, for tests in this crate that need one to hang
/// connections off. Lives outside the test module so the sweeper's delivery
/// tests can reach it; there is nothing run-specific in what they assert.
#[cfg(test)]
pub(crate) fn test_run() -> Arc<Run> {
    tests::run(1_000, 400, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn run(started_ns: u64, warmup_ns: u64, run_duration_ns: Option<u64>) -> Arc<Run> {
        let profiles = Arc::new(source::InstrumentProfiles::from_profiles(vec![
            crate::config::profile_for_symbol("BTCUSDT").expect("BTCUSDT preset must resolve"),
        ]));
        let instrument = profiles
            .instrument_defs()
            .into_iter()
            .next()
            .expect("default instrument");
        Run::new(
            instrument,
            source::Rivers::new(
                source::TapeIdentity {
                    seeds: RunSeeds::from_run_seed(42),
                    regime: None,
                },
                profiles,
            ),
            std::collections::HashMap::new(),
            SimClock::identity(),
            started_ns,
            warmup_ns,
            run_duration_ns,
            RunSeeds::from_run_seed(42),
            8,
            1,
            mogwai_protocol::OmsType::Netting,
            200,
            mogwai_protocol::AccountId::parse(crate::config::DEFAULT_ACCOUNT_ID)
                .expect("the default account id is legal"),
            std::sync::mpsc::channel().0,
        )
    }

    /// A query reply carries the asking connection's orders and nobody else's.
    ///
    /// The engine answers from one book because there is one ledger, so without
    /// this scoping a passenger polling `QueryOrders` reads every other
    /// passenger's order history - including their terminal rows, which is why
    /// ownership outlives an order's end. Deleting the `retain` in
    /// `scope_query_rows` makes the count assertion fail at 2.
    #[test]
    fn a_query_reply_carries_only_the_asking_accounts_orders() {
        let run = run(1_000, 400, None);
        run.claim_order("V-mine".into(), "MOGWAI-001");
        run.claim_order("V-theirs".into(), "MOGWAI-002");

        let row = |venue_order_id: &str| mogwai_protocol::OrderStatusInfo {
            client_order_id: venue_order_id.into(),
            venue_order_id: venue_order_id.into(),
            symbol: Symbol::from("BTCUSDT"),
            position_id: None,
            side: mogwai_protocol::Side::Buy,
            order_type: mogwai_protocol::OrderType::Limit,
            time_in_force: mogwai_protocol::TimeInForce::Gtc,
            status: mogwai_protocol::WireOrderStatus::Filled,
            quantity: Decimal::ONE,
            filled_qty: Decimal::ONE,
            price: None,
            trigger_price: None,
            ts_triggered: None,
            reduce_only: false,
            post_only: false,
            ts_accepted: 1,
            ts_last: 1,
        };
        let mut events = vec![mogwai_protocol::ServerMessage::OrderStatusSnapshot(
            mogwai_protocol::OrderStatusSnapshot {
                request_id: "R-1".into(),
                orders: vec![row("V-mine"), row("V-theirs")],
                ts_event: 1,
            },
        )];
        run.scope_query_rows(&mut events, "MOGWAI-001");

        let mogwai_protocol::ServerMessage::OrderStatusSnapshot(snapshot) = &events[0] else {
            panic!("the snapshot survives scoping");
        };
        assert_eq!(snapshot.orders.len(), 1, "one account, one order");
        assert_eq!(snapshot.orders[0].venue_order_id, "V-mine");
    }

    #[test]
    fn the_history_floor_is_the_fixed_tape_origin() {
        let run = run(1_000, 400, None);
        assert_eq!(run.data_origin_ns(), source::TAPE_ORIGIN_NS);
        assert_eq!(run.started_ns, 1_000);
        assert_eq!(run.warmup_ns, 400);
    }

    #[test]
    fn the_deadline_is_measured_from_the_post_warmup_epoch() {
        // Decision 8: the deadline counts from `started_ns`, which is set after
        // warmup generation - NOT from boot. A run whose warmup is larger than
        // its duration must still get its whole declared duration.
        let bounded = run(1_000_000, 999_000, Some(30));
        assert_eq!(bounded.deadline_ns, Some(1_000_030));

        let indefinite = run(1_000, 0, None);
        assert_eq!(indefinite.deadline_ns, None);
    }
}
