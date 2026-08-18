// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! State owned by one venue process: one ledger, and keyed paced boats over
//! many rivers.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
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
    /// The rules this account is enforced under, and what enforcing them has
    /// cost so far.
    ///
    /// A separate lock from the engine because it is read on paths that must
    /// not take the engine lock - the order-entry gate asks only "is this
    /// account locked" - and because the two are updated at different moments:
    /// the engine books a fill, and the risk ledger judges the equity that fill
    /// produced.
    pub(crate) risk: Mutex<crate::risk::RiskLedger>,
    /// WHETHER ANYBODY IS READING THIS ACCOUNT, and since when.
    ///
    /// `None` while a connection is attached. `Some(wall_instant)` once the last
    /// one went away, which FREEZES the account: it is not swept, not marked,
    /// not funded and not judged against its policy until a socket returns.
    ///
    /// This is a deliberate departure from a real venue, where being away is no
    /// defence against liquidation. Mogwai exists to exercise a client's live
    /// path rather than to simulate an account nobody is trading, and the
    /// consequence to state in any claim is that a run spanning a disconnect has
    /// a GAP IN ITS RISK HISTORY.
    ///
    /// A WALL instant rather than a simulated one, and the reason is that there
    /// is no simulated clock while frozen: the boat that carried one wound down
    /// with the last socket. This is what the TTL is measured against.
    pub(crate) frozen_since: Mutex<Option<std::time::Instant>>,
    /// Which boats this account is currently riding, and HOW MANY CONNECTIONS
    /// ride each. One connection sits on one boat; the default account can
    /// have two unnamed sockets on two symbols and therefore two boats. The
    /// sweeper applies a boat's walk only to passengers seated on it, so two
    /// cadences on one river cannot double-fill one ledger.
    ///
    /// COUNTED RATHER THAN A SET, because two unnamed sockets may share one
    /// river at one speed and therefore one boat. A set would let the first of
    /// them to close vacate the seat out from under the second, which stops
    /// the sweeper applying that boat to a ledger somebody is still trading.
    ///
    /// A seat is released when the SESSION drops, not when the account
    /// freezes: a freeze needs every socket gone, and the two-socket shape
    /// above would otherwise leave a seat held by a connection that ended.
    /// The key carries no placement nonce, so a stale seat would be
    /// indistinguishable from a live one the moment any account boarded that
    /// river at that speed again.
    seated_on: Mutex<HashMap<crate::boatyard::BoatKey, usize>>,
    /// TRANSPORT havoc, per account rather than per venue.
    ///
    /// These corrupt what one connection RECEIVES rather than what the
    /// generator produces, so under the river-and-passenger model they ride the
    /// passenger by construction: the river is untouched and there is nothing to
    /// scope. They were run-wide, which meant one subagent arming a blackout
    /// blacked out every other subagent on the exchange.
    pub(crate) dark: HavocWindow,
    pub(crate) stall: HavocWindow,
    /// Per-account ACK and ACT latency, the rest of the transport-havoc family.
    ///
    /// Same reasoning as the windows above: these change when one connection
    /// hears about its own commands, so a scenario slowing one subagent's acks
    /// must not slow the batch. `delay_ms` holds outbound execution output;
    /// the act pair delays the venue ACTING and the ack pair delays it SAYING
    /// what it did.
    pub(crate) delay_ms: AtomicU64,
    pub(crate) submit_act_ms: AtomicU64,
    pub(crate) modify_act_ms: AtomicU64,
    pub(crate) cancel_act_ms: AtomicU64,
    pub(crate) submit_ack_ms: AtomicU64,
    pub(crate) modify_ack_ms: AtomicU64,
    pub(crate) cancel_ack_ms: AtomicU64,
}

impl Passenger {
    /// How long this account has been unattended, or `None` while a connection
    /// is reading it.
    pub(crate) fn frozen_for(&self) -> Option<std::time::Duration> {
        self.frozen_since
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .map(|since| since.elapsed())
    }

    /// Whether this account is currently unattended, and therefore not swept,
    /// marked, funded or judged.
    pub(crate) fn is_frozen(&self) -> bool {
        self.frozen_since
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// Freeze the account, unless a connection is still reading it.
    ///
    /// Called when a lane is retired, so it must ask whether ANY lane is left
    /// rather than assume the one that went away was the last: an eviction
    /// retires the incumbent while the newcomer is already binding, and freezing
    /// an account somebody just claimed would leave it unswept until its next
    /// reconnect.
    fn freeze(&self) {
        let mut frozen = self
            .frozen_since
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if frozen.is_none() {
            *frozen = Some(std::time::Instant::now());
            self.seated_on
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clear();
            tracing::debug!(
                account = %self.account_id.as_str(),
                "account frozen: nobody is reading it, so it is not swept until a socket returns",
            );
        }
    }

    /// Take a seat on `key`, or name the seat this account already holds on
    /// that river at a different speed.
    ///
    /// THE ONLY WAY TO SIT, and unconditional on the freeze state on purpose.
    /// A frozen account has an empty map - the seats went with the sessions -
    /// so a reseat at a new speed passes the check anyway, and routing a
    /// frozen account past it instead would reopen the race this lock exists
    /// to close: an account is created frozen and only attaches once its
    /// socket reaches `resume`, so two first connections would both find
    /// themselves frozen and both sit unchecked.
    ///
    /// The check and the insert share the lock, so of two sockets racing two
    /// speeds exactly one wins.
    pub(crate) fn try_sit(
        &self,
        key: crate::boatyard::BoatKey,
    ) -> Result<(), crate::boatyard::BoatKey> {
        let mut seated = self
            .seated_on
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(other) = seated
            .keys()
            .find(|sitting| {
                sitting.river() == key.river() && sitting.speed_micros() != key.speed_micros()
            })
            .cloned()
        {
            return Err(other);
        }
        *seated.entry(key).or_insert(0) += 1;
        Ok(())
    }

    /// Give up one connection's seat on `key`.
    ///
    /// The seat itself is only vacated by the LAST rider, which is why the map
    /// counts. Tolerant of a key it does not hold: a freeze clears the map,
    /// and the freeze fires from lane release while the session that held the
    /// seat is still unwinding.
    pub(crate) fn unsit(&self, key: &crate::boatyard::BoatKey) {
        let mut seated = self
            .seated_on
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(riders) = seated.get_mut(key) {
            *riders -= 1;
            if *riders == 0 {
                seated.remove(key);
            }
        }
    }

    /// Whether this account is riding `key`.
    pub(crate) fn is_seated_on(&self, key: &crate::boatyard::BoatKey) -> bool {
        self.seated_on
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(key)
    }

    fn attach(&self) {
        *self
            .frozen_since
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
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

    /// Clear every transport arm this account carries.
    pub(crate) fn clear_transport_havoc(&self) {
        self.dark.clear();
        self.stall.clear();
        for knob in [
            &self.delay_ms,
            &self.submit_act_ms,
            &self.modify_act_ms,
            &self.cancel_act_ms,
            &self.submit_ack_ms,
            &self.modify_ack_ms,
            &self.cancel_ack_ms,
        ] {
            knob.store(0, Ordering::Relaxed);
        }
    }
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
    /// Whether a returning client is handed a CLEAN ledger instead of its own.
    /// See the config key of the same name; the readiness record reports it, so
    /// nobody has to infer which way a venue is set.
    reset_account_on_reconnect: bool,
    /// Risk policies the operator registered by name, which a client asks for
    /// instead of restating. Shadows a shipped name of the same spelling.
    account_policies: std::collections::HashMap<String, mogwai_protocol::risk::AccountPolicy>,
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
        reset_account_on_reconnect: bool,
        account_policies: std::collections::HashMap<String, mogwai_protocol::risk::AccountPolicy>,
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
            reset_account_on_reconnect,
            account_policies,
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
        let opening = opening_equity(&self.template.opening_balances);
        let seated = Arc::new(Passenger {
            account_id: account_id.clone(),
            engine: AsyncMutex::new(engine),
            // Unpoliced: an account nobody stated rules for is enforced against
            // nothing, which is what every client had before policies existed.
            risk: Mutex::new(crate::risk::RiskLedger::new(
                mogwai_protocol::risk::AccountPolicy::default(),
                opening,
                self.started_ns,
            )),
            // Born FROZEN, at the instant it was created. An account with no
            // connection is not being read whether it was just opened or just
            // abandoned, and starting it attached would make a POSTed account
            // nobody ever connects to immortal.
            frozen_since: Mutex::new(Some(std::time::Instant::now())),
            seated_on: Mutex::new(HashMap::new()),
            dark: HavocWindow::new(),
            stall: HavocWindow::new(),
            delay_ms: AtomicU64::new(0),
            submit_act_ms: AtomicU64::new(0),
            modify_act_ms: AtomicU64::new(0),
            cancel_act_ms: AtomicU64::new(0),
            submit_ack_ms: AtomicU64::new(0),
            modify_ack_ms: AtomicU64::new(0),
            cancel_ack_ms: AtomicU64::new(0),
        });
        passengers.insert(account_id.as_str().to_owned(), Arc::clone(&seated));
        seated
    }

    /// The passenger a connection that named no account is served under.
    pub(crate) fn default_passenger(&self) -> Arc<Passenger> {
        self.passenger(&self.default_account_id)
    }

    /// Seat a CONNECTION on an account: evict whoever holds it, then hand over
    /// the ledger or a clean one.
    ///
    /// The whole reconnection story in one call. A second socket presenting a
    /// seated id under a DIFFERENT session is indistinguishable from that client
    /// returning, so the venue does not try to tell them apart: the incumbent is
    /// closed and the newcomer gets the account. Whether it gets that account's
    /// HISTORY is the operator's `reset_account_on_reconnect` choice, reported
    /// in the readiness record so nobody has to guess which way a venue is set.
    ///
    /// A socket presenting the same session as a sitting one is the SAME CLIENT
    /// dialling again - a nautilus host's data and execution legs, which name
    /// one account by construction - so it neither evicts nor resets. Resetting
    /// there would discard the ledger the client's own first socket is trading
    /// on, which is the reset knob eating a live book rather than a stale one.
    pub(crate) fn seat(
        &self,
        account_id: &mogwai_protocol::AccountId,
        claimed: bool,
        session: Option<&str>,
    ) -> Arc<Passenger> {
        // ONLY A CLAIMED ACCOUNT EVICTS. Naming an id is a statement about
        // identity - "this ledger is mine, hand it over" - and eviction is the
        // answer to it. Naming NONE is not: it means the client has no opinion,
        // and the default account is a convenience for exactly that case.
        //
        // Evicting there broke the ordinary shape it exists to serve. A single
        // client opening two sockets on two symbols names no account on either,
        // so both land on the default and the second closed the first - which
        // is a client evicting itself.
        // Read BEFORE the eviction, which may retire lanes: this asks whether
        // the claiming client is already here, and a lane list the eviction has
        // already pruned could no longer answer it.
        let rejoining = claimed && self.has_client_on(account_id.as_str(), session);
        let displaced = if claimed {
            self.evict_account(account_id.as_str(), session)
        } else {
            0
        };
        if displaced > 0 {
            tracing::info!(
                account = %account_id.as_str(),
                displaced,
                reset = self.reset_account_on_reconnect,
                "a new connection claimed a seated account",
            );
        }
        if claimed && !rejoining && self.reset_account_on_reconnect {
            self.reopen(account_id);
        }
        self.passenger(account_id)
    }

    /// Collect every account that has been unattended longer than `ttl`.
    ///
    /// A frozen account is resumable state with no lifecycle of its own: nothing
    /// else would ever remove it, so a long-lived shared exchange would
    /// accumulate one ledger per id anybody ever presented, for the life of the
    /// process. The TTL is what bounds that, and it is deliberately the only
    /// thing that does - a frozen account is never liquidated, marked or judged
    /// on its way out, it is simply gone.
    ///
    /// Returns the ids collected, so the caller can say which.
    pub(crate) fn collect_expired_accounts(&self, ttl: std::time::Duration) -> Vec<String> {
        let expired: Vec<String> = self
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, passenger)| {
                passenger
                    .frozen_for()
                    .is_some_and(|unattended| unattended >= ttl)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for account_id in &expired {
            tracing::info!(
                account = %account_id,
                ttl_ms = ttl.as_millis(),
                "collecting an account nobody reclaimed",
            );
            self.discard_account(account_id);
        }
        expired
    }

    /// Discard an account's ledger so the next connection opens a clean one.
    /// Only `reset_account_on_reconnect` reaches this; the default is to keep
    /// what the account has.
    fn reopen(&self, account_id: &mogwai_protocol::AccountId) {
        self.discard_account(account_id.as_str());
    }

    /// Forget one account entirely: its ledger and the order claims attributing
    /// its frames. The claims must go with it, or a fresh ledger's frames would
    /// be attributed using a discarded one's history.
    fn discard_account(&self, account_id: &str) {
        self.passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(account_id);
        self.order_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, owner| owner != account_id);
    }

    /// Resolve a risk policy the way a symbol resolves: total, three steps,
    /// step three never fails.
    ///
    /// 1. Knobs the client stated INLINE win.
    /// 2. Otherwise a policy the operator REGISTERED under that name, or one
    ///    this build ships under it. Registered shadows shipped, because the
    ///    whole reason registration exists is that shipped terms go stale.
    /// 3. Otherwise UNPOLICED, which is the default account's policy and what
    ///    every client had before policies existed.
    ///
    /// A name nobody has is an ERROR rather than a silent fall to step three:
    /// asking for `apex-50k` and quietly getting no rules at all would be a run
    /// that believes it is enforced and is not.
    pub(crate) fn resolve_policy(
        &self,
        named: Option<&str>,
        inline: mogwai_protocol::risk::AccountPolicy,
    ) -> Result<mogwai_protocol::risk::AccountPolicy, AccountRefusal> {
        if !inline.is_unpoliced() {
            return Ok(inline);
        }
        let Some(name) = named else {
            return Ok(inline);
        };
        self.account_policies
            .get(name)
            .cloned()
            .or_else(|| mogwai_protocol::risk::shipped_policy(name))
            .ok_or_else(|| AccountRefusal::UnknownPolicy {
                name: name.to_owned(),
            })
    }

    /// The account a connection naming none is seated on.
    pub(crate) fn default_account_id(&self) -> mogwai_protocol::AccountId {
        self.default_account_id.clone()
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
        policy: mogwai_protocol::risk::AccountPolicy,
    ) -> Result<(), AccountRefusal> {
        let mut passengers = self
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if passengers.contains_key(account_id.as_str()) {
            return Err(AccountRefusal::AlreadyOpen);
        }
        let opening = opening_equity(&balances);
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
                // Anchored at the OPENING balance rather than at the first
                // observed equity: a trailing floor is stated relative to what
                // the account started with, and anchoring it at the first mark
                // would silently forgive whatever was lost before it.
                risk: Mutex::new(crate::risk::RiskLedger::new(
                    policy,
                    opening,
                    self.started_ns,
                )),
                // A POSTed account nobody has connected to yet is unattended
                // like any other, which is also what makes it collectable: an
                // account opened and then abandoned must not outlive the TTL
                // just because no socket ever reached it.
                frozen_since: Mutex::new(Some(std::time::Instant::now())),
                seated_on: Mutex::new(HashMap::new()),
                dark: HavocWindow::new(),
                stall: HavocWindow::new(),
                delay_ms: AtomicU64::new(0),
                submit_act_ms: AtomicU64::new(0),
                modify_act_ms: AtomicU64::new(0),
                cancel_act_ms: AtomicU64::new(0),
                submit_ack_ms: AtomicU64::new(0),
                modify_ack_ms: AtomicU64::new(0),
                cancel_ack_ms: AtomicU64::new(0),
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
                    basis: match margin.basis {
                        crate::config::MarginBasis::PerContract => {
                            mogwai_engine::MarginBasis::PerContract
                        }
                        crate::config::MarginBasis::Notional => {
                            mogwai_engine::MarginBasis::Notional
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
    pub(crate) fn bind_lanes(
        &self,
        lanes: ExecLanes,
        account_id: &str,
        session: Option<&str>,
    ) -> u64 {
        let id = lanes.id();
        self.locked_lanes().push(BoundLane {
            id,
            account_id: account_id.to_owned(),
            session: session.map(str::to_owned),
            lanes,
        });
        id
    }

    /// Retire one connection. The ACCOUNT'S order claims survive: an account
    /// outlives every connection that ever presented it, so a returning socket
    /// must still find its own orders attributed to it.
    pub(crate) fn release_lanes(&self, id: u64) {
        let account = {
            let mut lanes = self.locked_lanes();
            let account = lanes
                .iter()
                .find(|bound| bound.id == id)
                .map(|bound| bound.account_id.clone());
            lanes.retain(|bound| bound.id != id);
            // Only when nothing is left reading it. An eviction retires the
            // incumbent while the newcomer is binding, and freezing an account
            // somebody has just claimed would leave it unswept for the life of
            // that connection.
            account.filter(|account| !lanes.iter().any(|bound| &bound.account_id == account))
        };
        let Some(account) = account else {
            return;
        };
        if let Some(passenger) = self
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account.as_str())
        {
            passenger.freeze();
        }
    }

    /// Attach an account to the river a socket has just bound, and put it in a
    /// state that river can actually serve.
    ///
    /// THREE THINGS HAPPEN, and each closes a way an account could otherwise
    /// hold something nobody is reading:
    ///
    /// 1. The account is UNFROZEN, which is what the sweeper reads.
    /// 2. What a RETURNING account holds off this river is retired - resting
    ///    orders cancelled, positions closed at their last mark. A returning
    ///    socket may name a different symbol than the frozen account was
    ///    trading, and carrying that position forward would leave the account
    ///    holding something the new session can neither see nor close.
    /// 3. Every surviving order's scan frontier is RE-BASED onto this boat's
    ///    clock. A frozen order's frontier is wherever the departed boat got to,
    ///    which sits in the NEW boat's future - so without this the order is
    ///    wedged until the new cursor catches up, which is as long as the
    ///    previous session ran. The span while nobody was reading was never
    ///    watched and no fill is owed for it, which is the same statement the
    ///    freeze makes.
    ///
    /// STEPS 2 AND 3 RUN ONLY FOR A RETURNING ACCOUNT, and that is not a
    /// shortcut. A client that opens two sockets on two symbols and names no
    /// account lands both on the DEFAULT account, which is a supported shape;
    /// retiring on every bind would make the second socket close the first
    /// socket's book. Neither step has anything to do with a live account
    /// anyway: nothing is stranded and no clock has been left behind.
    ///
    /// Returns the events the retirement produced, for the caller to deliver.
    pub(crate) async fn resume(
        &self,
        passenger: &Passenger,
        symbol: &mogwai_protocol::Symbol,
        now_ns: u64,
    ) -> Vec<mogwai_protocol::ServerMessage> {
        let returning = passenger.is_frozen();
        passenger.attach();
        if !returning {
            return Vec::new();
        }
        let mut engine = passenger.engine.lock().await;
        let events = engine.retire_off_river(symbol, now_ns);
        engine.rebase_scans(now_ns);
        events
    }

    /// Close every connection already trading `account_id` under a DIFFERENT
    /// session than the newcomer's, because a newer client has claimed it.
    ///
    /// AN ACCOUNT IS ON AT MOST ONE CLIENT AT A TIME. Two clients on one id
    /// would be one ledger read and written from two places, with a trailing
    /// drawdown computed across two instruments, so the venue evicts rather than
    /// admitting the second alongside the first.
    ///
    /// A CLIENT IS NOT A SOCKET, which is what `session` exists to say. One
    /// client routinely holds several sockets on one ledger - a nautilus host
    /// dials `/ws` twice, once for market data and once for execution, and both
    /// legs name the same account by construction - and evicting on the bare id
    /// would make that client evict itself on its second dial. A session id is
    /// the client's own, stable across its sockets and across their redials, and
    /// fresh in a restarted process: sockets presenting the SAME one are one
    /// client and coexist, and a different one is a new client and evicts.
    ///
    /// AN ABSENT SESSION ALWAYS EVICTS, which keeps the pre-session contract
    /// exactly: a client that says nothing about its identity has made no claim
    /// to be the incumbent, and reading silence as "same client" would let a
    /// stranger quietly share a ledger. So the coexistence is opt-in and the
    /// safe reading is the default.
    ///
    /// The evicted socket is closed NORMALLY, not faulted: from the venue's side
    /// a second connection presenting an id is indistinguishable from that
    /// client reconnecting, and handing the ledger over is what makes a
    /// reconnect work. A consumer must not treat it as a reason to redial, or it
    /// would evict whatever evicted it.
    ///
    /// Returns how many were displaced, so the caller can say so.
    /// Whether a socket of THIS client is already bound to `account_id`. A
    /// client that named no session is never "already here": silence is not a
    /// claim to be the incumbent, which is the same reading
    /// [`Run::evict_account`] takes of it.
    pub(crate) fn has_client_on(&self, account_id: &str, session: Option<&str>) -> bool {
        let Some(session) = session else {
            return false;
        };
        self.locked_lanes().iter().any(|bound| {
            bound.account_id == account_id && bound.session.as_deref() == Some(session)
        })
    }

    pub(crate) fn evict_account(&self, account_id: &str, session: Option<&str>) -> usize {
        let same_client = |bound: &BoundLane| {
            session.is_some_and(|session| bound.session.as_deref() == Some(session))
        };
        let displaced: Vec<BoundLane> = self
            .locked_lanes()
            .iter()
            .filter(|bound| bound.account_id == account_id && !same_client(bound))
            .cloned()
            .collect();
        for bound in &displaced {
            drop(
                bound
                    .lanes
                    .send_close(crate::admission::CloseSpec::evicted(format!(
                        "another connection claimed account {account_id}; a ledger is never read \
                     from two clients at once"
                    ))),
            );
        }
        // Retired here rather than left to each socket's own teardown: the new
        // connection must not see the old lanes in `bound_lanes` even for the
        // instant it takes them to notice the close, or its first batch would
        // be delivered to a socket that is on its way out.
        self.locked_lanes()
            .retain(|bound| bound.account_id != account_id || same_client(bound));
        displaced.len()
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
}

/// Equity at open, before anything is marked: the funded balances summed.
/// Positions cannot exist yet, so there is no unrealized half.
fn opening_equity(balances: &std::collections::HashMap<String, Decimal>) -> Decimal {
    balances.values().fold(Decimal::ZERO, |sum, total| {
        sum.checked_add(*total).unwrap_or(sum)
    })
}

/// Why an account could not be opened on the terms asked for.
#[derive(Debug)]
pub(crate) enum AccountRefusal {
    /// Something already trades under this id. Never a reset: see
    /// `Run::open_account`.
    AlreadyOpen,
    /// A policy name neither the operator nor this build has. Refused rather
    /// than falling through to unpoliced, which would be a run that believes it
    /// is enforced and is not.
    UnknownPolicy { name: String },
}

impl std::fmt::Display for AccountRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyOpen => f.write_str(
                "this account is already open; an account outlives its connections, so it is \
                 never re-opened with new terms - name a different account id for a fresh ledger",
            ),
            Self::UnknownPolicy { name } => write!(
                f,
                "no account policy is registered or shipped under {name}; shipped names are {}, \
                 and an operator registers more under [account_policies] in the venue config",
                mogwai_protocol::risk::SHIPPED_POLICIES.join(", ")
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
    /// The CLIENT this socket belongs to, as the client named itself on the
    /// upgrade. `None` for a socket that named none, which is every socket
    /// predating the carrier and every one that has no opinion. See
    /// [`Run::evict_account`] for what it buys and why absent means "evict".
    pub(crate) session: Option<String>,
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
        | M::OrderExpired { venue_order_id, .. }
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
            false,
            std::collections::HashMap::new(),
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

    /// The freeze, stated as behaviour rather than as an accident of nobody
    /// having a boat: an account is frozen the moment its last connection goes,
    /// and attached again when one returns.
    #[test]
    fn an_account_freezes_when_its_last_connection_goes_and_thaws_when_one_returns() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("FREEZE-001").unwrap();
        let passenger = run.passenger(&account);
        assert!(
            passenger.is_frozen(),
            "an account nobody has connected to is unattended like any other"
        );

        let (first, _first_rx) = crate::admission::ExecLanes::detached();
        let first_id = run.bind_lanes(first, account.as_str(), None);
        passenger.attach();
        assert!(!passenger.is_frozen());

        // A SECOND connection on the same account: retiring the first must not
        // freeze an account somebody is still reading. This is the eviction
        // shape, where the incumbent is retired while the newcomer binds.
        let (second, _second_rx) = crate::admission::ExecLanes::detached();
        let second_id = run.bind_lanes(second, account.as_str(), None);
        run.release_lanes(first_id);
        assert!(
            !passenger.is_frozen(),
            "a connection is still reading this account"
        );

        run.release_lanes(second_id);
        assert!(passenger.is_frozen(), "the last one left");
    }

    fn boat_key(run: &Run, symbol: &str, speed: f64) -> crate::boatyard::BoatKey {
        let profile = run.rivers.resolve_profile(symbol).expect("a served symbol");
        crate::boatyard::BoatKey::new(run.rivers.resolve_key(&profile), speed)
            .expect("a legal speed")
    }

    /// Two sockets may share one boat - the default account, one symbol, one
    /// speed - so the seat is COUNTED. The first to leave must not vacate a
    /// seat the second is still riding, or the sweeper stops applying that
    /// boat to a ledger somebody is still trading.
    #[test]
    fn a_seat_shared_by_two_connections_survives_the_first_leaving() {
        let run = run(1_000, 400, None);
        let passenger = run.passenger(&mogwai_protocol::AccountId::parse("SEAT-001").unwrap());
        let key = boat_key(&run, "BTCUSDT", 2.0);

        passenger.try_sit(key.clone()).expect("the first sits");
        passenger
            .try_sit(key.clone())
            .expect("the second shares it");
        passenger.unsit(&key);
        assert!(
            passenger.is_seated_on(&key),
            "one connection left, one is still riding"
        );
        passenger.unsit(&key);
        assert!(!passenger.is_seated_on(&key), "the last rider vacated it");
    }

    /// The one-cadence rule holds while the account is FROZEN, which is the
    /// state every account is created in and stays in until its first socket
    /// reaches `resume`. Two first connections racing two speeds therefore
    /// both meet this check, and exactly one may win it.
    #[test]
    fn a_frozen_account_may_not_sit_on_two_cadences_of_one_river() {
        let run = run(1_000, 400, None);
        let passenger = run.passenger(&mogwai_protocol::AccountId::parse("SEAT-002").unwrap());
        assert!(passenger.is_frozen(), "a fresh account is unattended");

        passenger
            .try_sit(boat_key(&run, "BTCUSDT", 2.0))
            .expect("the first cadence sits");
        let refused = passenger
            .try_sit(boat_key(&run, "BTCUSDT", 3.0))
            .expect_err("a second cadence on one ledger is refused");
        assert_eq!(
            refused.speed(),
            2.0,
            "the refusal names the sitting cadence"
        );
    }

    /// A seat is released by the SESSION, not by the freeze - an account
    /// riding two rivers that loses one socket never freezes. The key carries
    /// no placement nonce, so a held seat would be indistinguishable from a
    /// live one and would both refuse a legitimate new cadence and hand this
    /// ledger a boat it never boarded.
    #[test]
    fn leaving_one_river_frees_that_seat_while_another_is_still_ridden() {
        let run = run(1_000, 400, None);
        let passenger = run.passenger(&mogwai_protocol::AccountId::parse("SEAT-003").unwrap());
        let slow = boat_key(&run, "BTCUSDT", 2.0);

        passenger.try_sit(slow.clone()).expect("board the slow one");
        passenger.unsit(&slow);
        assert!(
            !passenger.is_seated_on(&slow),
            "the seat went with the session, no freeze required"
        );
        passenger
            .try_sit(boat_key(&run, "BTCUSDT", 3.0))
            .expect("a new cadence on a river nobody is riding is not a conflict");
    }

    /// A CLIENT IS NOT A SOCKET. The nautilus host that drives this venue holds
    /// two sockets on one ledger - data and execution - and both name the same
    /// account, so eviction keyed on the bare id made the host's second dial
    /// disconnect its own first. Sockets presenting one session are one client
    /// and coexist; a different session is a different client and takes over.
    #[test]
    fn one_clients_sockets_share_a_ledger_and_a_stranger_evicts_them_all() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("CLAUDETTE-07").unwrap();

        let (data, _data_rx) = crate::admission::ExecLanes::detached();
        let (exec, _exec_rx) = crate::admission::ExecLanes::detached();
        run.seat(&account, true, Some("worker-1"));
        run.bind_lanes(data, account.as_str(), Some("worker-1"));
        assert_eq!(
            run.evict_account(account.as_str(), Some("worker-1")),
            0,
            "the host's second leg is the same client, not a claimant"
        );
        run.seat(&account, true, Some("worker-1"));
        run.bind_lanes(exec, account.as_str(), Some("worker-1"));
        assert_eq!(
            run.bound_lanes()
                .iter()
                .filter(|bound| bound.account_id == "CLAUDETTE-07")
                .count(),
            2,
            "both legs are reading the ledger they were configured for"
        );

        // A RESTARTED worker is a genuinely new client, and it takes the whole
        // ledger: both stale sockets go, which is the reconnection story the
        // eviction exists for.
        assert_eq!(
            run.evict_account(account.as_str(), Some("worker-2")),
            2,
            "a different client displaces every socket of the old one"
        );
        assert!(
            run.bound_lanes()
                .iter()
                .all(|bound| bound.account_id != "CLAUDETTE-07"),
        );
    }

    /// An absent session keeps the pre-session contract exactly: silence is not
    /// a claim to be the incumbent, so it always evicts and is always evicted.
    #[test]
    fn a_socket_naming_no_session_evicts_and_is_evicted() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("QUIET-001").unwrap();
        let (first, _first_rx) = crate::admission::ExecLanes::detached();
        let (second, _second_rx) = crate::admission::ExecLanes::detached();

        run.bind_lanes(first, account.as_str(), None);
        assert_eq!(
            run.evict_account(account.as_str(), None),
            1,
            "an unidentified newcomer takes the ledger, as it always did"
        );
        run.bind_lanes(second, account.as_str(), Some("worker-1"));
        assert_eq!(
            run.evict_account(account.as_str(), None),
            1,
            "and an unidentified newcomer displaces an identified incumbent too"
        );
    }

    /// The TTL, which is the only thing that ever removes an account: a frozen
    /// ledger is otherwise state with no lifecycle.
    #[test]
    fn the_ttl_collects_an_account_nobody_reclaimed_and_spares_an_attached_one() {
        let run = run(1_000, 400, None);
        let abandoned = mogwai_protocol::AccountId::parse("GONE-001").unwrap();
        let held = mogwai_protocol::AccountId::parse("HELD-001").unwrap();
        run.passenger(&abandoned);
        let held_passenger = run.passenger(&held);
        let (lanes, _rx) = crate::admission::ExecLanes::detached();
        run.bind_lanes(lanes, held.as_str(), None);
        held_passenger.attach();

        // A TTL nothing has outlived yet spares even the unattended account,
        // which is what makes the collection about the SPAN rather than about
        // being unattended at all.
        assert!(
            run.collect_expired_accounts(std::time::Duration::from_secs(3_600))
                .is_empty(),
            "an account frozen a moment ago has not outlived an hour"
        );

        // A zero TTL collects everything already unattended, which is what makes
        // the rest of this about the ATTACHMENT rather than about a sleep.
        let collected = run.collect_expired_accounts(std::time::Duration::ZERO);
        assert_eq!(
            collected,
            vec!["GONE-001".to_string()],
            "only the unattended account is collected: {collected:?}"
        );
        assert!(
            !run.passengers()
                .iter()
                .any(|passenger| passenger.account_id.as_str() == "GONE-001"),
            "and it is gone from the registry"
        );
        assert!(
            run.passengers()
                .iter()
                .any(|passenger| passenger.account_id.as_str() == "HELD-001"),
            "the attached account survives"
        );
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
