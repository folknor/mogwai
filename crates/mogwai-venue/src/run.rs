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

/// A havoc window, armed at a wall instant for a simulated span, judged on
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
/// control - and two independent `AtomicU64`s are a torn read: a concurrent
/// reader can pair the new wall instant with the old span, and a clear can race
/// a re-arm and erase the new span. The single `AtomicU64` this replaces was
/// tear-free by construction, so an atomic pair would be a regression
/// introduced by the fix. No packed encoding either: two independent nanosecond
/// quantities do not fit one u64 without a range limit nobody can audit later.
///
/// Three socket-level forms of these rules were deliberately not built at the
/// piece-10 landing: a stall window lasting its declared span on a late boat, a
/// window armed before boarding still opening, and two boats swept at their own
/// cadence. Each rule is pinned at unit level below; the socket forms are
/// latency-bounded and would land as flakes rather than as gates. If one is ever
/// wanted, express it on the divergence window's own clock and never on wall
/// arrival order.
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

    /// Stores, never extends: the whole span is replaced under the lock, so
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

    /// Judged on the reader's own clock.
    ///
    /// The late-boarder rule: the opening instant is
    /// `sim.sim_ns(wall_armed_ns)`, whose pre-anchor branch returns the reader's
    /// own `sim_epoch_ns`. Projecting the
    /// arming instant through the clock of a boat anchored later than the arm
    /// would put the window in that boat's past, where it never opens - arm a
    /// blackout, connect 50 ms later, and the blackout silently does not
    /// happen. Such a reader instead treats its own epoch as the opening and
    /// consumes the full span unconditionally.
    pub(crate) fn open_at(&self, sim: SimClock, sim_at_ns: u64) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|span| {
                let opening = sim.window_opening(span.wall_armed_ns);
                sim_at_ns >= opening && sim_at_ns < opening.saturating_add(span.sim_span_ns)
            })
    }
}

/// One account: an id plus everything the venue holds under it.
///
/// The engine, the risk ledger, the freeze stamp and the boat counts all belong
/// here, and this outlives every passenger that speaks for the account. A
/// passenger is one socket riding one boat and dies with that socket; the money
/// and the book stay here.
///
/// The engine is per account, not per process. It was one per process, which is
/// right while a venue serves one run and wrong the moment an orchestrator
/// points fifty subagents at one exchange: every subagent's fills moved every
/// other subagent's net.
pub(crate) struct Account {
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
    /// Who is reading this account.
    ///
    /// Whether the account is frozen, which boats it rides, how many sockets
    /// are on their way in and which lanes a frame reaches are all derived from
    /// the connection records held here, so none of them can disagree with
    /// another. Four fields used to carry parts of that answer with the
    /// consistency rules in prose; see `crate::registry`.
    ///
    /// Held by the account so the ordinary questions still read as questions
    /// about an account. The registry is run-wide and shared.
    pub(crate) registry: Arc<crate::registry::ConnectionRegistry>,
    /// Transport havoc, per account rather than per venue.
    ///
    /// These corrupt what one connection receives rather than what the
    /// generator produces, so they are scoped to the account and blur each of
    /// its passengers alike: the river is untouched and there is nothing to
    /// scope on the water side. They were run-wide, which meant one subagent arming a blackout
    /// blacked out every other subagent on the exchange.
    pub(crate) dark: HavocWindow,
    pub(crate) stall: HavocWindow,
    /// Per-account ACK and ACT latency, the rest of the transport-havoc family.
    ///
    /// Same reasoning as the windows above: these change when one connection
    /// hears about its own commands, so a scenario slowing one subagent's acks
    /// must not slow the batch. `delay_ms` holds outbound execution output;
    /// the act pair delays the venue acting and the ack pair delays it saying
    /// what it did.
    pub(crate) delay_ms: AtomicU64,
    pub(crate) submit_act_ms: AtomicU64,
    pub(crate) modify_act_ms: AtomicU64,
    pub(crate) cancel_act_ms: AtomicU64,
    pub(crate) submit_ack_ms: AtomicU64,
    pub(crate) modify_ack_ms: AtomicU64,
    pub(crate) cancel_ack_ms: AtomicU64,
}

impl Account {
    /// How long this account has been unattended, or `None` while a connection
    /// is reading it.
    pub(crate) fn frozen_for(&self) -> Option<std::time::Duration> {
        self.registry.frozen_for(self.account_id.as_str())
    }

    /// Whether this account is currently unattended, and therefore not swept,
    /// marked, funded or judged.
    ///
    /// Derived from the connection table, so it cannot drift from it. An
    /// account is attended while any connection of it is reading, and also
    /// while a committed admission holds a continuity handoff - the eviction
    /// window in which the incumbent is gone and its successor has not begun
    /// reading is not an unattended account. See `crate::registry`.
    pub(crate) fn is_frozen(&self) -> bool {
        self.registry.is_frozen(self.account_id.as_str())
    }

    /// Whether this account is riding `key`, derived from its connections.
    pub(crate) fn is_seated_on(&self, key: &crate::boatyard::BoatKey) -> bool {
        self.registry.is_seated_on(self.account_id.as_str(), key)
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

    /// The six `CommandLatency` knobs in the order that control names them, so
    /// a walk over them cannot pair a value with the wrong knob.
    fn latency_knobs(&self) -> [&AtomicU64; 6] {
        [
            &self.submit_act_ms,
            &self.modify_act_ms,
            &self.cancel_act_ms,
            &self.submit_ack_ms,
            &self.modify_ack_ms,
            &self.cancel_ack_ms,
        ]
    }
}

impl std::fmt::Debug for Account {
    /// The account only. A ledger's contents are not something a diagnostic
    /// should splice into a log line, and the engine is behind an async mutex a
    /// formatter cannot take.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("account_id", &self.account_id.as_str())
            .finish_non_exhaustive()
    }
}

/// One arm from the control plane - a `POST /control/divergence`, whether it
/// named an account or not.
///
/// It exists so the arming site can state its effect ONCE. `Run::arm` both
/// records it and applies it to the ledgers that already exist, and every site
/// that mints a ledger replays the record onto it, so the two can only agree.
pub(crate) enum VenueArm {
    DelayAcks {
        ms: u64,
    },
    CommandLatency {
        submit_act_ms: u64,
        modify_act_ms: u64,
        cancel_act_ms: u64,
        submit_ack_ms: u64,
        modify_ack_ms: u64,
        cancel_ack_ms: u64,
    },
    GoDark {
        armed_ns: u64,
        span_ns: u64,
    },
    StallData {
        armed_ns: u64,
        span_ns: u64,
    },
    FeeSurcharge {
        mult: Decimal,
        armed_ns: u64,
        span_ns: u64,
    },
    Engine(mogwai_protocol::control::Divergence),
}

/// A folded-up run of arms, replayable onto a ledger that did not exist when
/// they were posted.
///
/// Every field is optional, and an unset one is not a zero. A record is replayed
/// onto a ledger that may already carry another record's effects - the venue-wide
/// record then the account's own - so "this run said nothing about ack delays"
/// has to be distinguishable from "this run armed a zero ack delay", or the
/// second replay silently disarms the first.
///
/// The arm variants are store-not-merge on the wire, and this mirrors that
/// exactly: one `CommandLatency` replaces all six values, one `GoDark` replaces
/// the whole span.
#[derive(Default)]
struct ArmRecord {
    dark: Option<ArmedSpan>,
    stall: Option<ArmedSpan>,
    delay_ms: Option<u64>,
    /// `submit`, `modify`, `cancel` act delays then the three ack delays, in the
    /// order `CommandLatency` names them.
    latency: Option<[u64; 6]>,
    /// `(mult, wall armed, sim span)`, as `Engine::arm_fee_surcharge` takes it.
    fee_surcharge: Option<(Decimal, u64, u64)>,
    /// Engine-armed divergences in arming order, capped and shed from the oldest
    /// end exactly as `Engine::arm` does, so replaying this onto a fresh ledger
    /// reproduces the queue an existing one holds.
    engine: Vec<mogwai_protocol::control::Divergence>,
}

impl ArmRecord {
    fn record(&mut self, arm: &VenueArm) {
        match arm {
            VenueArm::DelayAcks { ms } => self.delay_ms = Some(*ms),
            VenueArm::CommandLatency {
                submit_act_ms,
                modify_act_ms,
                cancel_act_ms,
                submit_ack_ms,
                modify_ack_ms,
                cancel_ack_ms,
            } => {
                self.latency = Some([
                    *submit_act_ms,
                    *modify_act_ms,
                    *cancel_act_ms,
                    *submit_ack_ms,
                    *modify_ack_ms,
                    *cancel_ack_ms,
                ]);
            }
            VenueArm::GoDark { armed_ns, span_ns } => {
                self.dark = Some(ArmedSpan {
                    wall_armed_ns: *armed_ns,
                    sim_span_ns: *span_ns,
                });
            }
            VenueArm::StallData { armed_ns, span_ns } => {
                self.stall = Some(ArmedSpan {
                    wall_armed_ns: *armed_ns,
                    sim_span_ns: *span_ns,
                });
            }
            VenueArm::FeeSurcharge {
                mult,
                armed_ns,
                span_ns,
            } => self.fee_surcharge = Some((*mult, *armed_ns, *span_ns)),
            VenueArm::Engine(div) => {
                if self.engine.len() >= mogwai_engine::MAX_ARMED_DIVERGENCES {
                    self.engine.remove(0);
                }
                self.engine.push(div.clone());
            }
        }
    }

    /// Replay the transport-side record onto an account being opened.
    fn open_transport(&self, account_state: &Account) {
        if let Some(span) = self.dark {
            account_state.dark.arm(span.wall_armed_ns, span.sim_span_ns);
        }
        if let Some(span) = self.stall {
            account_state
                .stall
                .arm(span.wall_armed_ns, span.sim_span_ns);
        }
        if let Some(ms) = self.delay_ms {
            account_state.delay_ms.store(ms, Ordering::Relaxed);
        }
        if let Some(latency) = self.latency {
            for (knob, ms) in account_state.latency_knobs().into_iter().zip(latency) {
                knob.store(ms, Ordering::Relaxed);
            }
        }
    }

    /// Replay the engine-side record onto a ledger being opened.
    fn open_engine(&self, engine: &mut Engine) {
        if let Some((mult, armed_ns, span_ns)) = self.fee_surcharge {
            engine.arm_fee_surcharge(mult, armed_ns, span_ns);
        }
        for div in &self.engine {
            engine.arm(div.clone());
        }
    }
}

/// Apply the transport half of one arm to an account that already exists.
///
/// It reads the arm's own fields and nothing else. An earlier draft read
/// `CommandLatency` out of the record it had just written, which was correct
/// only because the caller recorded first - exactly the ordering coupling a
/// later edit breaks without a test going red.
fn apply_transport_arm(arm: &VenueArm, account_state: &Account) {
    match arm {
        VenueArm::DelayAcks { ms } => account_state.delay_ms.store(*ms, Ordering::Relaxed),
        VenueArm::CommandLatency {
            submit_act_ms,
            modify_act_ms,
            cancel_act_ms,
            submit_ack_ms,
            modify_ack_ms,
            cancel_ack_ms,
        } => {
            let armed = [
                *submit_act_ms,
                *modify_act_ms,
                *cancel_act_ms,
                *submit_ack_ms,
                *modify_ack_ms,
                *cancel_ack_ms,
            ];
            for (knob, ms) in account_state.latency_knobs().into_iter().zip(armed) {
                knob.store(ms, Ordering::Relaxed);
            }
        }
        VenueArm::GoDark { armed_ns, span_ns } => account_state.dark.arm(*armed_ns, *span_ns),
        VenueArm::StallData { armed_ns, span_ns } => account_state.stall.arm(*armed_ns, *span_ns),
        VenueArm::FeeSurcharge { .. } | VenueArm::Engine(_) => {}
    }
}

/// An arm never waits for a connection. Whatever the control plane has posted
/// that a ledger minted later still owes.
///
/// The arms belong to the run, not to the ledgers that happen to exist. The
/// control plane is an operator surface, and an unqualified arm is a statement about the venue -
/// `docs/havoc.md` says so twice, once for the transport windows ("naming none
/// arms every account") and once for the late boarder ("a passenger that boards
/// after the arm receives the full declared span from its own boarding
/// instant"). The arming code walked the accounts that existed at the instant
/// of the request, so an operator who armed a `PartialFillNext` and then started
/// a subagent got a run that believed it was perturbed and was not - and the
/// eviction report's "every ledger holds the same arms and hits the cap
/// together" was false the moment one ledger was minted after an arm.
///
/// The two halves differ in lifetime, deliberately so. `all` is the venue's
/// standing state and is replayed onto every ledger this run ever mints.
/// `pending` holds an arm posted against a named account that does not exist
/// yet, and is consumed by that account's first mint - which is precisely the
/// promise a named arm makes ("the arm is standing when the consumer dials") and
/// nothing more. Recording rather than minting is what keeps the control plane
/// from deciding an account's terms: the consumer's own `POST /accounts` still
/// opens the ledger, with its own balances and policy, and finds the arm on it.
/// A previous draft minted the ledger here, which locked that consumer out with a
/// `409` and handed it default balances.
///
/// A ledger minted now must be indistinguishable from one minted at boot that
/// received every control request since - see `Run::arm` for the exact scope of
/// that claim. That is the whole reason this record exists, and it is why an
/// arm is only ever added here and never subtracted: there is no control that
/// retracts one. The control plane arms and does not disarm, so what a run
/// carries is the accumulation of what was posted to it.
///
/// An arm recorded here is therefore one-way within the run. Pre-boarding havoc
/// setup is run construction, not run state a harness manages: a setup that
/// fails partway leaves records behind, `ArmRecord::record` appends engine arms
/// rather than replacing them, so a retried setup accumulates one-shots instead
/// of reproducing itself, and the only rollback is discarding the run. The one
/// qualification, and it is a capacity rule rather than a control: `pending` is
/// bounded, so a record here can still be shed by arms posted for other names -
/// see `MAX_PENDING_ACCOUNT_ARMS`. A pending arm is retained, not guaranteed.
#[derive(Default)]
struct VenueArms {
    all: ArmRecord,
    /// `(account id, record)` in first-armed order, for accounts that had not
    /// been minted when the arm arrived. A `Vec` rather than a map because it
    /// is shed from the oldest end at `MAX_PENDING_ACCOUNT_ARMS` - an operator
    /// surface that allocates per distinct name needs a bound, and this is the
    /// same shape and direction as the engine's own armed-queue cap.
    pending: Vec<(String, ArmRecord)>,
}

/// How many not-yet-minted accounts the run will carry arms for. Operator scale
/// is a handful of subagents; the cap exists so a typo loop on
/// `POST /control/divergence` cannot grow the run without bound.
///
/// The shed here is deliberately silent, while the engine's armed-queue shed is
/// reported in the ack body, and the asymmetry was adjudicated rather than
/// overlooked. This cap counts arms outstanding and not arms posted:
/// `take_pending` consumes a record the moment its account first exists, on both
/// mint paths (`Run::passenger` and `Run::open_account`), and every consumer
/// posts its havoc knobs on connect, so records drain almost immediately.
/// Reaching 64 needs an operator to arm 65 distinct names that never connect,
/// which is a typo at scale rather than a usage pattern. The engine queue fills
/// instead from arms against ledgers that already exist, so it can hit its cap in
/// ordinary use, and a silent eviction there once cost a QA run a full
/// misdiagnosis.
const MAX_PENDING_ACCOUNT_ARMS: usize = 64;

impl VenueArms {
    /// Record an arm against a named account that does not exist yet.
    fn record_pending(&mut self, account_id: &str, arm: &VenueArm) {
        if let Some((_, record)) = self
            .pending
            .iter_mut()
            .find(|(name, _)| name.as_str() == account_id)
        {
            record.record(arm);
            return;
        }
        if self.pending.len() >= MAX_PENDING_ACCOUNT_ARMS {
            self.pending.remove(0);
        }
        let mut record = ArmRecord::default();
        record.record(arm);
        self.pending.push((account_id.to_owned(), record));
    }

    /// Take `account_id`'s pending arms, if any. The caller replays `all` first
    /// and only then this, because a pending arm is by construction later in time
    /// than every standing one it can overlap.
    ///
    /// Taking rather than reading is what bounds `pending`, and it is the honest
    /// lifetime: the arm was a statement about the account's first ledger, and
    /// once that ledger exists it carries the arm the way any other armed ledger
    /// does.
    fn take_pending(&mut self, account_id: &str) -> Option<ArmRecord> {
        let at = self
            .pending
            .iter()
            .position(|(name, _)| name.as_str() == account_id)?;
        Some(self.pending.remove(at).1)
    }
}

/// The four venue settings used when an account's engine is built. Held on the
/// run because an account is created on demand, when a connection first names
/// one, and each new engine needs the same settings.
///
/// This is the venue-wide `[balances]` seed, which the account-policy design
/// retires in favour of a consumer-named opening balance and risk policy. Until
/// that lands it is the opening balance applied to each account rather than
/// the balance of one shared ledger, which is the same value doing a different
/// job.
/// Audited 2026-08-23 and found not to be a defect, twice over. Account opening
/// is not quadratic in symbol count: no mint loop installs more than one margin
/// policy, and `Engine::set_margin_policies` is the batch setter for whoever
/// writes one, with `Engine::set_margin_policy` a thin wrapper over it. And the
/// per-mint clone of `opening_balances` here, and the one in the `GET /account`
/// preview, is structural rather than waste: the engine mutates its own balance
/// map, so holding this behind an `Arc` would still clone into an owned map at
/// build time.
struct AccountOpeningTerms {
    opening_balances: std::collections::HashMap<String, Decimal>,
    fill_seed: u64,
    oms_type: mogwai_protocol::OmsType,
    fill_band_max_ticks: u32,
}

/// One connection's registration, from the instant its admission commits until
/// the socket is finished with it.
///
/// A guard object rather than a pair of calls, because the failure it closes is
/// the one where the release never runs: an upgrade abandoned after the 101
/// never reaches `handle_socket`, and a connection left registered forever
/// leaves its account never frozen, never TTL-collected and swept while riding
/// no boat.
///
/// Dropping it removes the connection record, which gives up its ride and its
/// lanes in the same transition and re-derives whether the account is still
/// attended. There is no way to register a connection without owning the guard
/// that removes it, and no way to remove it twice.
pub(crate) struct Attach {
    registry: Arc<crate::registry::ConnectionRegistry>,
    account_id: String,
    connection_id: u64,
}

impl Attach {
    /// This connection's identity, which is also the id its lanes carry, so an
    /// order accepted on them is attributable to the connection that submitted
    /// it.
    pub(crate) fn connection_id(&self) -> u64 {
        self.connection_id
    }
}

/// The account only: the run behind it holds every ledger on the venue, which
/// is not something a diagnostic should splice into a log line.
impl std::fmt::Debug for Attach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attach")
            .field("account_id", &self.account_id)
            .field("connection_id", &self.connection_id)
            .finish_non_exhaustive()
    }
}

impl Drop for Attach {
    fn drop(&mut self) {
        self.registry.release(&self.account_id, self.connection_id);
    }
}

pub(crate) struct Run {
    /// The shape this run placed its BOOT boat on, and the river a `/ws`
    /// upgrade binds when it names no symbol. Every configured shape has a
    /// river in `rivers`, is servable for history, and gets a boat of its own
    /// when an account boards it; this one is only distinguished by carrying
    /// a boat from boot, and therefore by never lagging the venue clock.
    pub(crate) default_symbol: Symbol,
    /// Every configured river, created on first use and keyed independently, so
    /// two symbols never serialize on each other's checkpoint chain.
    pub(crate) rivers: Arc<source::Rivers>,
    pub(crate) oms_type: mogwai_protocol::OmsType,
    /// Every account this venue has served, created on demand and keyed by
    /// account id. An id is the consumer's to choose and outlives its connection,
    /// so a returning socket finds its own ledger rather than a fresh one; that
    /// is what makes a reconnect a continuation instead of a new trader.
    accounts: Mutex<std::collections::HashMap<String, Arc<Account>>>,
    account_opening_terms: AccountOpeningTerms,
    /// Every venue-wide arm the control plane has posted. Read whenever a
    /// ledger is minted, so a late-connecting account carries what the operator
    /// armed. The lock order is `accounts` first and then this, and both `Run::account` and
    /// `arm_venue_wide` take them that way, which is what makes an arm racing a
    /// mint land in exactly one of the two paths rather than in neither.
    venue_arms: Mutex<VenueArms>,
    /// The account a connection that names none is served under. It exists for
    /// the ephemeral single-consumer venue, where making the one consumer name an
    /// id would be ceremony; it is NOT the venue's one account.
    default_account_id: mogwai_protocol::AccountId,
    /// Whether a returning consumer is handed a clean ledger instead of its own.
    /// See the config key of the same name; the readiness record reports it, so
    /// nobody has to infer which way a venue is set.
    reset_account_on_reconnect: bool,
    /// Risk policies the operator registered by name, which a consumer asks for
    /// instead of restating. Shadows a shipped name of the same spelling.
    account_policies: std::collections::HashMap<String, mogwai_protocol::risk::AccountPolicy>,
    pub(crate) seeds: RunSeeds,
    pub(crate) boatyard: Arc<Boatyard>,
    /// The venue clock, and not the now of any boated river. It is the venue's
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
    /// One receiver per live passenger, held from before the 101 until
    /// after that passenger's writer has flushed its close, so the process can
    /// tell whether anything is still being served.
    ///
    /// The accept loop is not the answer to that question, which is the whole
    /// reason this exists. `axum::serve`'s graceful shutdown tracks hyper
    /// connections, and an upgraded connection's hyper future resolves at the
    /// 101 rather than when the websocket ends - `serve/mod.rs` drops its
    /// `close_rx` the moment `serve_connection_with_upgrades` returns. So a
    /// venue that waited only on axum was racing its own passengers: the run's
    /// completion was published on a watch channel, the accept loop stopped, the
    /// serve future resolved, `main` returned, and the runtime dropped every
    /// passenger task mid-flight, taking the `RunComplete` frame and the WS 1000
    /// close with it. The peer then saw a reset rather than a completed run,
    /// intermittently and only on a loaded host, which is exactly the shape the
    /// two lifecycle leads reported.
    ///
    /// Nothing else may take a receiver. The count is the number of live
    /// passengers, and a holder that is not one makes `passengers_drained` a wait on
    /// something other than what it names.
    passengers_tx: watch::Sender<()>,
    /// The venue's terminal-fault channel, kept here as well as inside the
    /// boatyard so the control plane can reach it.
    ///
    /// A source that faults sends on its own clone; `FaultTape` sends on this
    /// one. The receiving end is one thread in `serve`, which does not care
    /// which clone a fault arrived on - so an injected fault and a generated one
    /// take the same teardown, which is the whole point of injecting it.
    fault_tx: std::sync::mpsc::Sender<mogwai_data::TickFault>,
    /// The first venue-owned materialization failure this run suffered, if any.
    ///
    /// Latched rather than reported per caller, and separate from the tape fault
    /// channel because it happens before there is a tape: placement walks a
    /// river to its origin before `Tape::start` installs a fault sender, so the
    /// channel that carries every other terminal condition does not exist yet.
    ///
    /// Only a venue fault latches. A river cap is reachable, intentional and
    /// documented as a bad request, so latching it would make ordinary capacity
    /// admission a kill switch for the whole venue.
    materialize_fault: Mutex<Option<String>>,
    /// Who is reading which account, and the lanes their output reaches.
    ///
    /// One structure rather than the four this replaced, so the answers cannot
    /// disagree: whether an account is frozen, which boats it rides, whether a
    /// socket is on its way in, and where a venue-originated frame goes are all
    /// derived from the same connection records. It also owns the admission
    /// reservation, which is what makes an upgrade and the eviction it performs
    /// one transaction. See `crate::registry`.
    pub(crate) registry: Arc<crate::registry::ConnectionRegistry>,
    /// Which connection submitted each live order, so a sweep-produced fill goes
    /// to the account that owns it and to nobody else.
    ///
    /// Keyed by `VenueOrderId` because that is what every sweep-produced frame
    /// and every query row carries; the value is the account id the order
    /// belongs to. Every live order is claimed: the command dispatcher claims
    /// a consumer's submissions at acceptance, and `claim_produced_orders`
    /// claims venue-originated orders (liquidations) for the account whose
    /// ledger produced them. An order absent from this table is therefore a
    /// BUG in whoever built the batch, and the fallback - delivered to every
    /// lane, visible to every query, with a warning naming the id - is the
    /// conservative direction to fail in while that bug lives: an account
    /// seeing a stray frame is a smaller wrong than one missing its own fill.
    ///
    /// By account, not by connection, and the difference is not cosmetic. A
    /// ledger belongs to an account, so two sockets presenting the same id are
    /// the same trader and must each see the whole account's orders; keying on
    /// the connection hid a consumer's own resting order from its own second
    /// socket. Different accounts is what invisibility is about.
    ///
    /// A claim survives its order's terminal state. Retiring on the ending frame
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
            fault_tx.clone(),
            started_ns,
        );
        let (complete_tx, _) = watch::channel(None);
        let (passengers_tx, _) = watch::channel(());
        // No engine is built here. A ledger belongs to an account, and no
        // account exists until a connection first names one, so the run carries
        // only what one is opened from.
        Arc::new(Self {
            accounts: Mutex::new(std::collections::HashMap::new()),
            account_opening_terms: AccountOpeningTerms {
                opening_balances: balances,
                fill_seed: seeds.fill,
                oms_type,
                fill_band_max_ticks,
            },
            venue_arms: Mutex::new(VenueArms::default()),
            default_account_id: account_id,
            reset_account_on_reconnect,
            account_policies,
            default_symbol: instrument.symbol,
            rivers,
            oms_type,
            seeds,
            boatyard,
            sim,
            started_ns,
            deadline_ns: run_duration_ns.map(|duration| started_ns.saturating_add(duration)),
            warmup_ns,
            complete_tx,
            passengers_tx,
            fault_tx,
            materialize_fault: Mutex::new(None),
            registry: crate::registry::ConnectionRegistry::new(),
            order_owners: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// An engine built from the account opening terms, for `account_id`,
    /// holding `balances`.
    ///
    /// The one place the opening terms are applied. Three callers build an
    /// engine - the mint on first sight, the consumer's own `open_account`, and
    /// the throwaway preview `unopened_ledger` - and each carried its own copy
    /// of `Engine::build` plus the two `set_*` calls. Those copies are the
    /// two-implementations-without-a-gate shape: the next setting added to the
    /// opening terms would be owed at three sites, nothing would notice two of
    /// them, and the preview would then answer for a ledger the venue would
    /// never actually open. Lifecycle still differs per caller - only this
    /// construction is shared.
    ///
    /// `balances` is a parameter rather than read from the opening terms
    /// because `open_account` is the consumer stating its own; the other two
    /// pass the configured opening balances.
    fn engine_from_account_opening_terms(
        &self,
        account_id: &mogwai_protocol::AccountId,
        balances: std::collections::HashMap<String, Decimal>,
    ) -> Engine {
        // The ledger starts empty of instruments. One becomes tradable when a
        // passenger binds a symbol or names it on an order, through
        // `ensure_instrument` - which is per account for the same reason the
        // engine is.
        let mut engine = Engine::build(EngineConfig {
            account_id: account_id.clone(),
            instruments: Vec::new(),
            balances,
            fill_seed: self.account_opening_terms.fill_seed,
        });
        engine.set_oms_type(self.account_opening_terms.oms_type);
        engine.set_liquidation_band_ticks(self.account_opening_terms.fill_band_max_ticks);
        engine
    }

    /// The account trading under `account_id`, created on first sight.
    ///
    /// Creation is the whole lifecycle: an account outlives the connection that
    /// named it, so a second connection presenting the same id gets the SAME
    /// ledger, with its positions and order history intact. That is what a
    /// reconnect is from the venue's side, and it is indistinguishable from a
    /// stranger claiming the id, which is why nothing here tries to tell them
    /// apart.
    pub(crate) fn account(&self, account_id: &mogwai_protocol::AccountId) -> Arc<Account> {
        let mut accounts = self
            .accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = accounts.get(account_id.as_str()) {
            return Arc::clone(existing);
        }
        let mut engine = self.engine_from_account_opening_terms(
            account_id,
            self.account_opening_terms.opening_balances.clone(),
        );
        // Opened with whatever the operator has armed - venue-wide, and against
        // this account by name before it existed - so this ledger is
        // indistinguishable from one that existed when the arm arrived. Held
        // across the account insert below, under the `accounts` lock this
        // call already holds, so an arm cannot interleave between the replay and
        // the moment `Run::arm` can see this account.
        let mut arms = self
            .venue_arms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = arms.take_pending(account_id.as_str());
        arms.all.open_engine(&mut engine);
        if let Some(pending) = &pending {
            pending.open_engine(&mut engine);
        }
        let minted_policy = self.minted_policy();
        let opening = opening_equity(&self.account_opening_terms.opening_balances, &minted_policy);
        let minted = Arc::new(Account {
            account_id: account_id.clone(),
            engine: AsyncMutex::new(engine),
            // `minted_policy` is unpoliced today: an account nobody stated
            // rules for is enforced against nothing, which is what every
            // consumer had before policies existed. It is asked for rather than
            // spelled inline so the upgrade path's calendar check can ask the
            // same question - see `Run::minted_policy`.
            risk: Mutex::new(crate::risk::RiskLedger::new(
                minted_policy,
                opening,
                self.started_ns,
            )),
            // Born frozen, because the registry has no connection for it yet.
            // An account with no connection is not being read whether it was
            // just opened or just abandoned, and starting it attended would
            // make a POSTed account nobody ever connects to immortal.
            registry: Arc::clone(&self.registry),
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
        // The registry learns the account here, at its mint, so every derived
        // answer about it has an entry to read rather than a missing one to
        // guess from.
        self.registry.ensure(account_id.as_str());
        arms.all.open_transport(&minted);
        if let Some(pending) = &pending {
            pending.open_transport(&minted);
        }
        drop(arms);
        accounts.insert(account_id.as_str().to_owned(), Arc::clone(&minted));
        minted
    }

    /// The ledger an account nobody has opened would open with, built here and
    /// retained nowhere.
    ///
    /// This is not a third mint site, and the difference is exactly that
    /// nothing survives the call: no account is inserted, no pending named
    /// arm is consumed, no passenger boards and no freeze clock starts. The
    /// construction is nevertheless shared with both real mint sites, through
    /// `engine_from_account_opening_terms`, because "not a mint" is a claim
    /// about lifecycle and says nothing about whether a preview is built the
    /// same way as the thing it previews. It exists so a READ
    /// can answer for an unknown account without allocating one -
    /// `GET /account?account=<anything>` is unauthenticated, and creating a
    /// ledger per id anybody names makes an endpoint that changes nothing into
    /// an unbounded allocator, bounded only when `account_ttl_ms > 0` and the
    /// default is to keep accounts forever.
    ///
    /// The arms are deliberately not replayed, which is what keeps this honest
    /// rather than a fourth thing to keep in sync. Replaying the venue record
    /// would consume this account's pending arm, so a read would disarm what an
    /// operator armed against an account that has not connected. Nothing the
    /// arms touch - a fee surcharge, an armed engine divergence, a transport
    /// window - is rendered in an `AccountState` or a `RiskState`, so the
    /// answer is identical either way. That last sentence is an assumption
    /// about the snapshot shape, stated here rather than left implicit: it
    /// holds for every field those two types carry today, and a field added to
    /// either that DOES render an armed effect makes a preview and a real
    /// ledger differ. Nothing detects that; this comment is the notice.
    pub(crate) fn unopened_ledger(
        &self,
        account_id: &mogwai_protocol::AccountId,
    ) -> (Engine, crate::risk::RiskLedger) {
        let engine = self.engine_from_account_opening_terms(
            account_id,
            self.account_opening_terms.opening_balances.clone(),
        );
        let ledger = crate::risk::RiskLedger::new(
            mogwai_protocol::risk::AccountPolicy::default(),
            opening_equity(
                &self.account_opening_terms.opening_balances,
                &mogwai_protocol::risk::AccountPolicy::default(),
            ),
            self.started_ns,
        );
        (engine, ledger)
    }

    /// Apply one control-plane arm, to the ledgers that exist AND to the ones
    /// that do not yet. Returns the divergence an engine queue shed to make
    /// room, if any.
    ///
    /// `account` is the request's optional `account` field. `None` is not "every
    /// ledger that happens to exist", it is the venue itself: the arm is recorded on
    /// the run and replayed onto every ledger minted from here on. `Some(name)`
    /// reaches exactly that account whether or not it has connected - live if it
    /// has, from the pending record if it has not.
    ///
    /// The record and the live application are one call, deliberately. Written
    /// as two - store it here, walk the accounts there - they drift, and the
    /// drift is invisible: the existing set behaves and the next account to
    /// connect does not. `VenueArms` says why this is owed at all.
    ///
    /// The record is taken while holding the account map, which is what
    /// resolves the race with a concurrent mint. `Run::account` and
    /// `Run::open_account` read the record under the same map lock, so an
    /// account is either already in the list this walks or was opened from a
    /// record that includes this arm - never neither.
    ///
    /// What "indistinguishable from an existing ledger" does not cover: the engine
    /// half is applied after both locks drop, because the engine sits behind an
    /// async mutex. Two engine arms posted concurrently can therefore land on
    /// two existing ledgers in opposite orders, and a ledger minted between them
    /// holds the record's order. The control plane is operator-driven and
    /// serialized in every scenario the venue is used from, so this is stated
    /// rather than closed - but it is a real limit on the claim, not a rounding
    /// error in it.
    pub(crate) async fn arm(
        &self,
        account: Option<&str>,
        arm: VenueArm,
    ) -> Option<mogwai_protocol::control::Divergence> {
        let existing: Vec<Arc<Account>> = {
            let accounts = self
                .accounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut arms = self
                .venue_arms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match account {
                None => {
                    arms.all.record(&arm);
                    for account_state in accounts.values() {
                        apply_transport_arm(&arm, account_state);
                    }
                    accounts.values().map(Arc::clone).collect()
                }
                Some(named) => match accounts.get(named) {
                    Some(account_state) => {
                        apply_transport_arm(&arm, account_state);
                        vec![Arc::clone(account_state)]
                    }
                    // Not minted here. Recording leaves the consumer's own
                    // `POST /accounts` free to open the ledger on its own terms
                    // and find the arm already on it; minting would answer that
                    // request `409 already open` and hand the account default
                    // balances and no policy.
                    None => {
                        arms.record_pending(named, &arm);
                        Vec::new()
                    }
                },
            }
        };
        let mut shed = None;
        match &arm {
            VenueArm::FeeSurcharge {
                mult,
                armed_ns,
                span_ns,
            } => {
                for account_state in existing {
                    account_state
                        .engine
                        .lock()
                        .await
                        .arm_fee_surcharge(*mult, *armed_ns, *span_ns);
                }
            }
            VenueArm::Engine(div) => {
                for account_state in existing {
                    let evicted = account_state.engine.lock().await.arm(div.clone());
                    shed = shed.or(evicted);
                }
            }
            // Transport arms are already applied, synchronously, above.
            VenueArm::DelayAcks { .. }
            | VenueArm::CommandLatency { .. }
            | VenueArm::GoDark { .. }
            | VenueArm::StallData { .. } => {}
        }
        shed
    }

    /// Claim an account for a connection: evict whoever holds it, then hand
    /// over the ledger or a clean one.
    ///
    /// The whole reconnection story in one call. A second socket presenting an
    /// id the venue already holds, under a different callsign, is
    /// indistinguishable from an incumbent
    /// returning, so the venue does not try to tell them apart: the incumbent is
    /// closed and the newcomer gets the account. Whether it gets that account's
    /// History is the operator's `reset_account_on_reconnect` choice, reported
    /// in the readiness record so nobody has to guess which way a venue is set.
    ///
    /// A socket presenting the same callsign as a sitting one carries the same identity
    /// dialling again - a nautilus host's data and execution legs, which name
    /// one account by construction - so it neither evicts nor resets. Resetting
    /// there would discard the ledger the first socket is trading
    /// on, which is the reset knob eating a live book rather than a stale one.
    ///
    /// `resetting` is passed in rather than re-derived, from
    /// `claim_discards_ledger`. `/ws` has to know the answer before it claims -
    /// the funding refusal is a statement about the ledger the connection will
    /// actually get - and computing it a second time here would evaluate the
    /// same predicate against a later state of the venue: `has_matching_identity_on` reads
    /// the lane table, and another matching lane can drop in the window
    /// between the two reads. The two values would then disagree about whether
    /// the ledger `/ws` funding-checked and cadence-checked is the ledger this
    /// call returns. One evaluation, one decision, one reader.
    pub(crate) fn claim_account(
        &self,
        account_id: &mogwai_protocol::AccountId,
        claimed: bool,
        callsign: Option<&str>,
        resetting: bool,
    ) -> Arc<Account> {
        // Only a claimed account evicts. Naming an id is a statement about
        // identity - "this ledger is mine, hand it over" - and eviction is the
        // answer to it. Naming NONE is not: it means the consumer has no opinion,
        // and the default account is a convenience for exactly that case.
        //
        // Evicting there broke the ordinary shape it exists to serve. A single
        // consumer opening two sockets on two symbols names no account on either,
        // so both land on the default and the second closed the first - which
        // is a consumer evicting itself.
        // Eviction is no longer performed here. It is selected by the admission
        // commit, in the same transaction that installs the newcomer, and
        // carried out by the caller once the registry lock is released - so a
        // refusal can no longer arrive after an incumbent has been closed. What
        // remains here is the ledger question.
        let _ = (claimed, callsign);
        if resetting {
            self.reopen(account_id);
        }
        self.account(account_id)
    }

    /// Whether claiming this account will discard its ledger.
    ///
    /// The one home of the reset rule, and asked exactly once per upgrade: `/ws` asks
    /// it before it refuses anything - the funding refusal is a statement about
    /// the ledger the connection will actually get, and under the reset knob
    /// that is a fresh one built from the account opening terms rather than
    /// whatever the account holds now - and then hands the answer to
    /// `claim_account`. It must be asked before any eviction, because
    /// `has_matching_identity_on` reads the lane table
    /// and an eviction prunes exactly the lanes that answer "is this identity
    /// already here".
    pub(crate) fn claim_discards_ledger(
        &self,
        account_id: &mogwai_protocol::AccountId,
        claimed: bool,
        callsign: Option<&str>,
    ) -> bool {
        claimed
            && self.reset_account_on_reconnect
            && !self.has_matching_identity_on(account_id.as_str(), callsign)
    }

    /// The account's ledger, if it already exists. Unlike `Run::account`, this
    /// never mints one, which is what lets the funding check ask about an
    /// account nobody has opened without opening it.
    ///
    /// This does not make a refused upgrade allocation-free, and saying so
    /// would be the over-claim this comment used to make. `/ws` calls
    /// `Run::account` on the non-resetting path, to record the ride before the
    /// eviction - and the cadence refusal comes after it, so a refused upgrade
    /// can still leave a fresh ledger behind. That is now the only such site:
    /// `GET /account` used to mint the same way on an unauthenticated read and
    /// resolves through this method instead, previewing an unopened ledger with
    /// `unopened_ledger` when there is nothing to find. Whether a refusal may
    /// allocate an account remains open; what this method promises is only that
    /// it, itself, does not mint.
    pub(crate) fn peek_account(
        &self,
        account_id: &mogwai_protocol::AccountId,
    ) -> Option<Arc<Account>> {
        self.accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account_id.as_str())
            .map(Arc::clone)
    }

    /// Whether the ledger this connection will be served on holds a balance
    /// line in `currency`.
    ///
    /// Presence, never sufficiency - see `Engine::is_funded_in`. Asked of the
    /// prospective ledger rather than of the current one, because a claim that
    /// resets replaces the account's balances with the opening terms' balances,
    /// and an account opened through `/account` with consumer-named balances
    /// can differ from those in exactly which currencies it carries.
    pub(crate) async fn funded_in(
        &self,
        account_id: &mogwai_protocol::AccountId,
        resetting: bool,
        currency: &str,
    ) -> bool {
        if resetting {
            return self
                .account_opening_terms
                .opening_balances
                .contains_key(currency);
        }
        match self.peek_account(account_id) {
            Some(account_state) => account_state.engine.lock().await.is_funded_in(currency),
            // An account nobody has opened yet is minted from the account
            // opening terms, so their balances are the honest answer and no
            // ledger is created to give it.
            None => self
                .account_opening_terms
                .opening_balances
                .contains_key(currency),
        }
    }

    /// The policy currency of the ledger this connection will be served on.
    ///
    /// This mirrors `funded_in`'s prospective-ledger semantics: a resetting or
    /// unopened claim reads the policy that will be minted, while a resuming
    /// claim reads the existing ledger. A currency on an unpoliced policy is
    /// descriptive only and must not turn boarding into enforcement.
    pub(crate) fn policy_currency(
        &self,
        account_id: &mogwai_protocol::AccountId,
        resetting: bool,
    ) -> Option<String> {
        if !resetting && let Some(account_state) = self.peek_account(account_id) {
            let ledger = account_state
                .risk
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            return ledger.policed_currency().map(str::to_owned);
        }
        // Resetting, or nothing opened yet: the policy this claim will mint.
        // Asked rather than hardcoded to `None`, so the day `minted_policy`
        // grows teeth this door follows it instead of being silently skipped.
        let policy = self.minted_policy();
        if policy.is_unpoliced() {
            None
        } else {
            policy.currency
        }
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
    /// "Unattended" is the same two-part question `freeze_if_unattended` asks,
    /// and it is asked here for the same reason: the freeze stamp alone is a
    /// statement about the past, and a socket that has attached to a
    /// long-frozen account has not cleared it yet.
    ///
    /// Returns the ids collected, so the caller can say which.
    pub(crate) fn collect_expired_accounts(&self, ttl: std::time::Duration) -> Vec<String> {
        let expired: Vec<String> = self
            .accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, account_state)| {
                // One predicate, because there is now only one place that knows
                // whether anybody is reading this account. `frozen_for` answers
                // `None` for an attended account, and a committed admission
                // that has not begun reading counts as attended - so a consumer
                // reclaiming a long-frozen ledger can no longer have it
                // collected out from under it between its admission and its
                // first read. That window used to be the whole upgrade wide,
                // and it was open because two predicates asked two structures.
                account_state
                    .frozen_for()
                    .is_some_and(|unattended| unattended >= ttl)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let mut collected = Vec::with_capacity(expired.len());
        for account_id in expired {
            // The filter's answer is already the past: an admission can reserve
            // or commit on this account between the read above and the removal,
            // and collecting it anyway would delete the connection record that
            // admission just committed - the finding-1 stranding through the
            // TTL door. `collect_account` re-derives the guard atomically and
            // says whether the collection happened; a refused one simply waits
            // out its next expiry, if it ever becomes unattended again.
            if self.collect_account(&account_id) {
                tracing::info!(
                    account = %account_id,
                    ttl_ms = ttl.as_millis(),
                    "collected an account nobody reclaimed",
                );
                collected.push(account_id);
            } else {
                tracing::info!(
                    account = %account_id,
                    "an expired account was reclaimed before it could be collected",
                );
            }
        }
        collected
    }

    /// Discard an account's ledger so the next connection opens a clean one.
    /// Only `reset_account_on_reconnect` reaches this; the default is to keep
    /// what the account has.
    fn reopen(&self, account_id: &mogwai_protocol::AccountId) {
        self.discard_account(account_id.as_str());
    }

    /// Discard one account's ledger and the order claims attributing its frames.
    /// The connection registry is deliberately left intact: reconnect reset runs
    /// inside the admission's reservation window, before the commit that
    /// installs the successor's ride and handoff, so the connection lifecycle is
    /// not this path's to touch.
    fn discard_account(&self, account_id: &str) {
        self.accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(account_id);
        self.order_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, owner| owner != account_id);
    }

    /// Collect an unattended account entirely, or refuse because an admission
    /// got in between.
    ///
    /// The sweep's expiry read is stale by the time it acts, so the registry
    /// re-derives "unattended, and nothing pending" under its own lock before
    /// the entry goes - see `ConnectionRegistry::collect`. The ledger is
    /// removed under the accounts lock taken before that registry call, in the
    /// same accounts-then-registry order the expiry filter already nests, so an
    /// admission reading the ledger through this map cannot interleave between
    /// the registry removal and the ledger removal: it either sees both or
    /// neither.
    fn collect_account(&self, account_id: &str) -> bool {
        {
            let mut accounts = self
                .accounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.registry.collect(account_id) {
                return false;
            }
            accounts.remove(account_id);
        }
        self.order_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, owner| owner != account_id);
        true
    }

    /// Resolve a risk policy the way a symbol resolves: total, three steps,
    /// step three never fails.
    ///
    /// 1. Knobs the consumer stated inline win.
    /// 2. Otherwise a policy the operator registered under that name, or one
    ///    this build ships under it. Registered shadows shipped, because the
    ///    whole reason registration exists is that shipped terms go stale.
    /// 3. Otherwise unpoliced, which is the default account's policy and what
    ///    every consumer had before policies existed.
    ///
    /// A name nobody has is an error rather than a silent fall to step three:
    /// asking for `apex-50k` and quietly getting no rules at all would be a run
    /// that believes it is enforced and is not.
    pub(crate) fn resolve_policy(
        &self,
        named: Option<&str>,
        inline: mogwai_protocol::risk::AccountPolicy,
    ) -> Result<mogwai_protocol::risk::AccountPolicy, AccountRefusal> {
        if !inline.is_unpoliced() || !inline.opening_balances.is_empty() {
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

    /// The account a connection naming none claims.
    pub(crate) fn default_account_id(&self) -> mogwai_protocol::AccountId {
        self.default_account_id.clone()
    }

    /// The risk policy a fresh mint installs on this venue's accounts.
    ///
    /// One question, one answer, asked by `Run::account` when it mints and by
    /// `Run::daily_reset_minute` when it has to say what a resetting claim will
    /// be served. It is unpoliced today because `AccountOpeningTerms` carries
    /// no policy - so a reset ledger has no daily loss limit and therefore no
    /// reset minute to validate against a symbol's calendar. That was the
    /// argument for the resetting path skipping the calendar refusal outright;
    /// the argument is now a call instead, so the day the opening terms gain a
    /// policy the refusal follows it rather than being silently skipped.
    fn minted_policy(&self) -> mogwai_protocol::risk::AccountPolicy {
        mogwai_protocol::risk::AccountPolicy::default()
    }

    /// The UTC minute at which this connection's ledger resets its daily loss
    /// limit, if it polices one.
    ///
    /// A resetting claim is not served the ledger the account holds now, so it
    /// is asked of the ledger it will actually get: the one `Run::account`
    /// mints, whose policy is `minted_policy`. Answering `None` unconditionally
    /// there was correct only for as long as that policy stayed unpoliced, and
    /// nothing said so.
    pub(crate) fn daily_reset_minute(
        &self,
        account_id: &mogwai_protocol::AccountId,
        resetting: bool,
    ) -> Option<u32> {
        if resetting {
            return crate::risk::daily_reset_minute_of(&self.minted_policy());
        }
        let account = self
            .accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account_id.as_str())
            .cloned()?;
        account
            .risk
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .daily_reset_minute()
    }

    /// Open an account with a consumer-named opening balance, before anything
    /// trades on it.
    ///
    /// This is step one of the three-step account resolution, the one where the
    /// consumer states its own terms. Steps two and three - a named policy preset,
    /// and the default account preset - are what a connection gets when it never
    /// calls this, which is why calling it is optional and its absence is not an
    /// error anywhere.
    ///
    /// Refused if the account is already open, rather than resetting it. An
    /// account outlives the connection that named it, so re-opening one is
    /// ambiguous between "I am starting a fresh experiment" and "I reconnected
    /// and re-sent my config", and the second reading would silently wipe a
    /// live position book. A consumer that wants a clean ledger names a different
    /// id, which costs it nothing.
    ///
    /// This is the second mint site and it owes the venue arms too. The consumer
    /// states its balances and its policy; it does not get to state whether the
    /// operator's havoc reaches it. Replaying the record here is what makes
    /// `VenueArms`'s claim - a ledger minted now is indistinguishable from one
    /// minted at boot that received every control request since - true on both
    /// paths rather than only on the one that was written first.
    pub(crate) fn open_account(
        &self,
        account_id: &mogwai_protocol::AccountId,
        balances: std::collections::HashMap<String, Decimal>,
        policy: mogwai_protocol::risk::AccountPolicy,
    ) -> Result<(), AccountRefusal> {
        let mut accounts = self
            .accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if accounts.contains_key(account_id.as_str()) {
            return Err(AccountRefusal::AlreadyOpen);
        }
        // A policed account may hold only the currency its thresholds are
        // stated in, and that starts at the opening funding. The policy
        // validator says the same thing about `opening_balances`; this is the
        // other door into the same ledger, the request's own explicit
        // `balances`, and it must be shut too or the refusal is decorative.
        // Funding a USD policy with USD and EUR would anchor the account at a
        // number no observation can reach - the first mark reads as a loss of
        // the whole foreign balance and can liquidate immediately - and there
        // is no rate surface to convert with, so the configuration is refused
        // rather than coerced.
        if let Some(policy_currency) = policy.currency.as_ref().filter(|_| !policy.is_unpoliced())
            && let Some(other) = balances
                .keys()
                .find(|currency| currency.as_str() != policy_currency.as_str())
        {
            return Err(AccountRefusal::ForeignOpeningBalance {
                currency: other.clone(),
                policy_currency: policy_currency.clone(),
            });
        }
        let opening = opening_equity(&balances, &policy);
        let mut engine = self.engine_from_account_opening_terms(account_id, balances);
        // Under the `accounts` lock this call already holds, in the same lock
        // order `Run::arm` takes, so an arm racing this open lands in exactly
        // one of the two paths.
        let mut arms = self
            .venue_arms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = arms.take_pending(account_id.as_str());
        arms.all.open_engine(&mut engine);
        if let Some(pending) = &pending {
            pending.open_engine(&mut engine);
        }
        let opened = Arc::new(Account {
            account_id: account_id.clone(),
            engine: AsyncMutex::new(engine),
            // Anchored at the opening balance rather than at the first
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
            registry: Arc::clone(&self.registry),
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
        self.registry.ensure(account_id.as_str());
        arms.all.open_transport(&opened);
        if let Some(pending) = &pending {
            pending.open_transport(&opened);
        }
        drop(arms);
        accounts.insert(account_id.as_str().to_owned(), opened);
        Ok(())
    }

    /// The account whose book holds `client_order_id` as a resting order, and
    /// the symbol it rests on.
    ///
    /// `account` is the request's own `account` field, and naming it is how a
    /// caller resolves the ambiguity rather than losing to it. Client order ids
    /// are consumer-chosen, so they are unique within one trader's book and not
    /// across a venue serving fifty of them: two subagents that both number
    /// their orders from one collide, and an unqualified search returns
    /// whichever account the map iterated first. That is a scenario control
    /// cancelling a stranger's resting order - silently, since a silent cancel
    /// emits no lifecycle event by design, so the victim learns of it only by
    /// querying. With an account named, exactly one book is searched and a miss
    /// is a miss.
    ///
    /// `None` still searches every book, because that is what a control request
    /// naming no account means everywhere else on this plane - the venue - and
    /// on the single-account venue an operator usually drives, it is the only
    /// book there is.
    pub(crate) async fn account_holding(
        &self,
        account: Option<&str>,
        client_order_id: &str,
    ) -> Option<(Arc<Account>, Symbol)> {
        let candidates = match account {
            Some(named) => self
                .accounts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(named)
                .map(Arc::clone)
                .into_iter()
                .collect(),
            None => self.accounts(),
        };
        for account_state in candidates {
            let symbol = account_state
                .engine
                .lock()
                .await
                .open_order_symbol(client_order_id);
            if let Some(symbol) = symbol {
                return Some((account_state, symbol));
            }
        }
        None
    }

    /// Every account this venue has served, for the venue-wide walks: the fill
    /// sweeper, and the control plane's reach into every ledger.
    pub(crate) fn accounts(&self) -> Vec<Arc<Account>> {
        self.accounts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(Arc::clone)
            .collect()
    }

    /// Make `symbol` tradable on one account's ledger: register the def and
    /// install the margin policy and fee schedule from its profile. Called when
    /// a socket binds a symbol and before an order for it is admitted. An `Err`
    /// means no profile resolves it, and the caller lets the engine produce its
    /// own unknown-instrument rejection rather than inventing a second wording.
    ///
    /// Per account because the engine is: two traders on one venue each keep
    /// their own registered instruments, margin policy and fee schedule, and
    /// nothing one of them binds is visible in the other's ledger.
    ///
    /// This is the one path from a profile to engine policy - `Run::new` no
    /// longer has a copy - and the installs are guarded on the registration
    /// having been new, so re-binding a symbol a consumer is already trading never
    /// resets its configuration.
    pub(crate) async fn ensure_instrument(
        &self,
        account_state: &Account,
        symbol: &str,
    ) -> Result<Arc<crate::source::InstrumentProfile>, crate::source::ResolveRefusal> {
        let profile = self.rivers.resolve_profile(symbol)?;
        self.register_instrument(account_state, &profile).await;
        Ok(profile)
    }

    /// The second half of `ensure_instrument`: install an already-resolved
    /// shape on one ledger.
    ///
    /// Split out because the two halves refuse differently and `/ws` needs them
    /// at different moments. Resolution is the only fallible part and is a
    /// property of the venue, so it is decided before a connection claims an
    /// account and can therefore refuse without having evicted anybody;
    /// registration is a property of the ledger, cannot fail, and has to happen
    /// on whichever account the claim actually returned, which under the reset
    /// knob is not the one that existed when the shape was resolved.
    pub(crate) async fn register_instrument(
        &self,
        account_state: &Account,
        profile: &Arc<crate::source::InstrumentProfile>,
    ) {
        let mut engine = account_state.engine.lock().await;
        if !engine.ensure_instrument(profile.def.clone()) {
            return;
        }
        if let Some(margin) = profile.margin.clone() {
            engine.set_margin_policy(
                Arc::clone(&profile.def.symbol),
                mogwai_engine::MarginPolicy {
                    initial_per_contract: margin.initial_per_contract,
                    maintenance_per_contract: margin.maintenance_per_contract,
                    breach_action: match margin.breach_action {
                        crate::config::MarginBreachAction::Refuse => {
                            mogwai_engine::MarginBreachAction::Refuse
                        }
                        crate::config::MarginBreachAction::Liquidate => {
                            mogwai_engine::MarginBreachAction::Liquidate
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
    }

    /// Enrol one connection's lanes for venue-originated output. The returned
    /// id is what `release_lanes` retires, so a reconnecting consumer cannot
    /// retire the lanes of the connection that replaced it.
    ///
    /// The id belongs to the lanes themselves, minted when they were constructed rather than
    /// here. That is what lets `process_order_cmd` - which is handed the lanes
    /// and nothing else naming the connection - record who owns an order it just
    /// saw accepted.
    pub(crate) fn bind_lanes(
        &self,
        lanes: ExecLanes,
        account_id: &str,
        _callsign: Option<&str>,
    ) -> u64 {
        let id = lanes.id();
        // The reading boundary. This is where the connection stops being a
        // committed admission holding a continuity handoff and starts being an
        // attended reader, so it is also where the handoff is retired.
        self.registry.begin_reading(account_id, id, lanes);
        id
    }

    /// Retire one connection. The account's order claims survive: an account
    /// outlives every connection that ever presented it, so a returning socket
    /// must still find its own orders attributed to it.
    ///
    /// One transition rather than the several this replaced. The connection
    /// record carries its ride and its lanes, so removing it gives all of them
    /// up at once and attendance is re-derived from what is left. That is what
    /// retired the tolerance the old shape needed: a ride used to be released
    /// separately from a lane and from an attach count, at different moments,
    /// so each release had to accept finding nothing.
    pub(crate) fn release_lanes(&self, account_id: &str, id: u64) {
        self.registry.release(account_id, id);
    }

    /// Phase one of an admission: take exclusive authority over this account so
    /// the fallible work can happen without holding any lock.
    ///
    /// Nothing is claimed and nobody is evicted here. That is the whole point of
    /// the split: an upgrade that goes on to fail placement, or has its future
    /// cancelled, costs the incumbent nothing, where a check-then-claim shape
    /// had to evict before it knew whether the newcomer could be served.
    /// `observed_incarnation` comes from [`Run::ledger_incarnation`], sampled
    /// before the caller read this account's ledger. See the ledger identity
    /// boundary in `crate::registry`.
    pub(crate) fn reserve_admission(
        &self,
        account_id: &str,
        seat: crate::registry::Seat,
        resetting: bool,
        observed_incarnation: u64,
    ) -> Result<crate::registry::Reservation, crate::registry::AdmissionRefusal> {
        self.registry
            .reserve(account_id, seat, resetting, observed_incarnation)
    }

    /// The identity of the ledger this account holds right now, to be sampled
    /// before any check is taken against that ledger and handed back to
    /// [`Run::reserve_admission`].
    pub(crate) fn ledger_incarnation(&self, account_id: &mogwai_protocol::AccountId) -> u64 {
        self.registry.incarnation(account_id.as_str())
    }

    /// Phase three: install the connection, and hand back both the guard that
    /// removes it and whatever it displaced.
    ///
    /// The displaced lanes are returned rather than closed here, because closing
    /// one sends on a channel: doing that under the registry lock would make
    /// every consumer's teardown cost a registry acquisition. The caller closes
    /// them once this has returned.
    ///
    /// `None` carries the registry's own invariant failure through unchanged.
    /// It cannot happen while the reservation is live, and the caller answers it
    /// rather than being handed a guard for a connection that was never
    /// installed - see `ConnectionRegistry::commit`.
    pub(crate) fn commit_admission(
        &self,
        reservation: &mut crate::registry::Reservation,
        callsign: Option<&str>,
        ride: Option<crate::boatyard::BoatKey>,
        claimed: bool,
    ) -> Option<(Attach, crate::registry::Committed)> {
        let committed = self.registry.commit(reservation, callsign, ride, claimed)?;
        let attach = Attach {
            registry: Arc::clone(&self.registry),
            account_id: reservation.account_id().to_owned(),
            connection_id: committed.connection_id,
        };
        Some((attach, committed))
    }

    /// Attach an account to the river a socket has just bound, and put it in a
    /// state that river can actually serve.
    ///
    /// Three things happen, and each closes a way an account could otherwise
    /// hold something nobody is reading:
    ///
    /// 1. The account is unfrozen, which is what the sweeper reads.
    /// 2. What a returning account holds off this river is retired - resting
    ///    orders cancelled, positions closed at their last mark. A returning
    ///    socket may name a different symbol than the frozen account was
    ///    trading, and carrying that position forward would leave the account
    ///    holding something the new connection can neither see nor close.
    /// 3. Every surviving order's scan frontier is re-based onto this boat's
    ///    clock. A frozen order's frontier is wherever the departed boat got to,
    ///    which sits in the new boat's future - so without this the order is
    ///    wedged until the new cursor catches up, which is as long as the
    ///    previous connection ran. The span while nobody was reading was never
    ///    watched and no fill is owed for it, which is the same statement the
    ///    freeze makes.
    ///
    /// Steps 2 and 3 run only for a returning account, and that is not a
    /// shortcut. A consumer that opens two sockets on two symbols and names no
    /// account lands both on the default account, which is a supported shape;
    /// retiring on every bind would make the second socket close the first
    /// socket's book. Neither step has anything to do with a live account
    /// anyway: nothing is stranded and no clock has been left behind.
    ///
    /// Returns the events the retirement produced, for the caller to deliver.
    ///
    /// `returning` is supplied by the admission that produced this passenger
    /// rather than asked here, and it has to be: committing an admission is what
    /// makes an account attended, so by the time this runs the account is never
    /// frozen no matter how long it had been sitting. The commit samples it at
    /// the one instant that can answer it. See
    /// `crate::registry::Committed::resumed_from_freeze`.
    pub(crate) async fn resume(
        &self,
        account_state: &Account,
        symbol: &mogwai_protocol::Symbol,
        now_ns: u64,
        returning: bool,
    ) -> Vec<mogwai_protocol::VenueMessage> {
        if !returning {
            // Not a first bind, necessarily. An eviction-reconnect lands here
            // too: the newcomer is counted onto the account before the incumbent
            // is closed, so the account never goes unattended and `returning` is
            // false even though a cursor may have been torn down underneath it.
            // Step 3 is therefore owed on this path as well, and asked as the
            // state itself rather than through the freeze - see
            // `Engine::rebase_future_scans`.
            let mut engine = account_state.engine.lock().await;
            let rebased = engine.rebase_future_scans(now_ns);
            if rebased > 0 {
                tracing::debug!(
                    account = account_state.account_id.as_str(),
                    rebased,
                    now_ns,
                    "re-based scan frontiers that led the binding cursor; the book was marked on a \
                     cursor that is gone"
                );
            }
            return Vec::new();
        }
        let mut engine = account_state.engine.lock().await;
        let events = engine.retire_off_river(symbol, now_ns);
        engine.rebase_scans(now_ns);
        events
    }

    /// Whether a socket with this identity is already bound to `account_id`. A
    /// socket that named no callsign is never "already here": silence is not a
    /// claim to be the incumbent, which is the same reading the admission commit
    /// takes of it.
    pub(crate) fn has_matching_identity_on(
        &self,
        account_id: &str,
        callsign: Option<&str>,
    ) -> bool {
        let Some(callsign) = callsign else {
            return false;
        };
        self.registry.has_callsign_on(account_id, callsign)
    }

    /// Close every connection already trading `account_id` under a different
    /// callsign than the newcomer's, because a different identity has claimed it.
    ///
    /// Connections presenting different identities do not coexist on one
    /// account. That would leave one ledger read and written from unrelated
    /// places, so the venue evicts rather than admitting the claimant alongside
    /// the incumbent.
    ///
    /// Several sockets may present the same identity. A nautilus host dials
    /// `/ws` twice, once for market data and once for execution, and both legs
    /// name the same account by construction. Evicting on the bare id would make
    /// the second dial evict the first. A callsign is stable across related
    /// sockets and their redials, and fresh in a restarted process: sockets
    /// presenting the same one coexist, and a different one evicts.
    ///
    /// An absent callsign always evicts, which keeps the pre-callsign contract
    /// exactly: a socket that says nothing about its identity has made no claim
    /// to be the incumbent, and reading silence as "same identity" would let a
    /// stranger quietly share a ledger. So the coexistence is opt-in and the
    /// safe reading is the default.
    ///
    /// The evicted socket is closed normally, not faulted: from the venue's side
    /// a second connection presenting an id is indistinguishable from an
    /// incumbent reconnecting, and handing the ledger over is what makes a
    /// reconnect work. A consumer must not treat it as a reason to redial, or it
    /// would evict whatever evicted it.
    ///
    /// Close the connections an admission displaced, and report how many.
    ///
    /// Which connections those are is decided inside the commit, under the
    /// registry lock and in the same transaction that installed their
    /// replacement. This runs after that lock is released, and only sends the
    /// closes: a send can block, and doing it under the registry mutex would
    /// make every consumer's teardown cost a registry acquisition.
    ///
    /// The records are already gone from the registry by the time this runs, so
    /// the newcomer never sees the old lanes in `bound_lanes` even for the
    /// instant it takes the incumbent to notice its close. That ordering used to
    /// be a second retain pass over a lane table that the eviction had to
    /// remember to perform.
    pub(crate) fn close_displaced(
        &self,
        account_id: &str,
        displaced: &[crate::registry::BoundLane],
    ) -> usize {
        for bound in displaced {
            drop(
                bound
                    .lanes
                    .send_close(crate::admission::CloseSpec::evicted(account_id)),
            );
        }
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
    /// Only the command dispatcher calls this; the sweep's half is
    /// [`Run::claim_produced_orders`], which covers the orders the venue
    /// originates.
    pub(crate) fn track_ownership(&self, events: &[mogwai_protocol::VenueMessage], owner: &str) {
        for event in events {
            if let mogwai_protocol::VenueMessage::OrderAccepted { venue_order_id, .. } = event {
                self.claim_order(venue_order_id.clone(), owner);
            }
        }
    }

    /// Claim, for `owner`, every order-scoped frame in a batch that account's
    /// own ledger just produced and that no connection has already claimed.
    ///
    /// This is what attributes a venue-originated order - a risk or margin
    /// liquidation the venue mints under the reserved id prefixes - to the
    /// account whose ledger it acts on. Such an order has no submitting
    /// connection, so before this existed its frames fell to the broadcast
    /// fallback and every account saw them, which is the one hole the
    /// invisibility property had. The account is knowable, and it is knowable
    /// here, at production time: every engine pass is per account, so a
    /// frame in the pass's batch is about that account's book by
    /// construction. Claiming at production keeps delivery a pure function of
    /// the batch - the ambient account context is used where it is truthful
    /// (booking) and never at delivery, which is the rule the `AccountState`
    /// broadcast defect taught.
    ///
    /// `or_insert`, never overwrite: a consumer-submitted order in the same
    /// batch is already claimed by the dispatcher, and its claim is the same
    /// account's anyway. Claims retire with the account (`discard_account`),
    /// exactly like a dispatcher's claims, so a frozen-and-resumed account
    /// keeps attribution over its whole book.
    pub(crate) fn claim_produced_orders(
        &self,
        events: &[mogwai_protocol::VenueMessage],
        owner: &str,
    ) {
        let mut owners = self
            .order_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for event in events {
            if let Audience::Order(id) = audience(event) {
                owners.entry(id.clone()).or_insert_with(|| owner.to_owned());
            }
        }
    }

    /// Drop the rows of a query reply that belong to another connection.
    ///
    /// The engine answers `QueryOrders` and `QueryFills` from one book, because
    /// there is one ledger; scoping happens here, where the connection is known.
    /// A row for an unclaimed order stays, on the same rule the delivery filter
    /// uses - better a stray row than a missing one - and, like that filter's
    /// broadcast arm, it is a defensive fallback rather than a class of order:
    /// venue-originated liquidations are claimed at production, so an unclaimed
    /// row means a production site failed to claim.
    pub(crate) fn scope_query_rows(
        &self,
        events: &mut [mogwai_protocol::VenueMessage],
        owner: &str,
    ) {
        use mogwai_protocol::VenueMessage as M;
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
    pub(crate) fn bound_lanes(&self) -> Vec<crate::registry::BoundLane> {
        self.registry.bound_lanes()
    }

    /// Record a venue-owned materialization failure, first one wins.
    ///
    /// First rather than latest because it is the one that describes what went
    /// wrong: every later placement on a venue whose generator has already
    /// failed is a consequence rather than a cause.
    ///
    /// Also sends on the fault channel, so a venue that can no longer produce
    /// water it promised takes the same terminal path as a tape that faulted
    /// mid-run. A run that kept serving its other rivers would be a venue
    /// selectively trustworthy about which of its own promises it kept, which is
    /// worse for a forward test than ending.
    pub(crate) fn latch_materialize_fault(&self, symbol: &str, detail: &str) {
        let mut latched = self
            .materialize_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if latched.is_some() {
            return;
        }
        *latched = Some(format!("{symbol}: {detail}"));
        tracing::error!(
            symbol,
            detail,
            "a river the config validated could not be materialized; the venue is faulted",
        );
        drop(latched);
        // A closed channel is a venue already tearing down for some other
        // reason, which is not a second failure to report.
        if self
            .fault_tx
            .send(mogwai_data::TickFault::Materialize)
            .is_err()
        {
            tracing::debug!("the venue was already tearing down when a river failed to appear");
        }
    }

    /// The latched materialization fault, for `/health`.
    pub(crate) fn materialize_fault(&self) -> Option<String> {
        self.materialize_fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Earliest sim instant the tape can serve.
    pub(crate) fn data_origin_ns(&self) -> u64 {
        source::TAPE_ORIGIN_NS
    }

    /// Announces the one planned terminal transition.  Receivers get the
    /// simulated instant and elapsed duration before the listener is drained.
    pub(crate) fn complete(&self, sim_now_ns: u64, elapsed_ns: u64) {
        self.complete_tx
            .send_replace(Some((sim_now_ns, elapsed_ns)));
    }

    pub(crate) fn completion(&self) -> watch::Receiver<Option<(u64, u64)>> {
        self.complete_tx.subscribe()
    }

    /// Fault the venue terminally, at the operator's request.
    ///
    /// Reports whether the fault was delivered. A closed channel means the
    /// receiving thread in `serve` is already gone, which is what a venue
    /// tearing down for some other reason looks like from here - so a second
    /// `FaultTape` arriving during teardown is a no-op rather than a panic, and
    /// the caller can answer honestly instead of claiming to have killed a venue
    /// that was already dying.
    pub(crate) fn fault_venue(&self) -> bool {
        // The diagnostic is emitted here because an injected fault reaches the
        // channel directly and never passes the tape worker, which is what logs
        // a source fault. Without this line the two causes would share an exit
        // code and nothing else: a run that died on command would look, in the
        // log, exactly like one that died silently.
        //
        // Its own message rather than the worker's "tape source faulted". No
        // tape source faulted - the operator asked - and a shared substring
        // between the two would stop either being a discriminator for whichever
        // one a reader is trying to confirm.
        tracing::error!("venue faulted on operator request; exiting nonzero");
        self.fault_tx.send(mogwai_data::TickFault::Injected).is_ok()
    }

    /// A live-passenger token. Taken by `ws::ws_upgrade` ahead of the 101 being
    /// returned, carried on the `Passenger`, and dropped only after that
    /// passenger's writer has flushed - so the token's lifetime strictly
    /// contains the passenger's, and strictly contains the hyper connection's,
    /// which ends at the 101. Taking it inside the spawned handler instead
    /// would leave a window in which the connection is upgraded and this count
    /// has not yet risen. See `passengers_tx`.
    pub(crate) fn passenger_guard(&self) -> watch::Receiver<()> {
        self.passengers_tx.subscribe()
    }

    /// Resolves once no passenger is live. Immediately, when none is.
    ///
    /// Awaited on the planned-completion shutdown only, and that limit is
    /// deliberate rather than an oversight. A planned completion is a promise
    /// the venue made about its declared duration - every open socket is told,
    /// and each passenger ends itself the moment it hears, so waiting here is
    /// bounded by the passengers' own teardown and is the difference between an
    /// announced run and a reset one. A signal is not that promise: the
    /// launcher ended the run, `RunComplete` is deliberately not published, and
    /// nothing tells a passenger to stop, so waiting would idle out the whole
    /// shutdown grace on any venue with a socket attached and turn a clean stop
    /// into a bailed one. The signal path therefore keeps its abrupt teardown,
    /// which is what `sigterm_closes_without_announcing_run_complete` observes.
    pub(crate) async fn passengers_drained(&self) {
        self.passengers_tx.closed().await;
    }
}

/// Equity at open, before anything is marked: the funded balances summed.
/// Positions cannot exist yet, so there is no unrealized half.
/// The equity a fresh ledger is anchored at, in the currency its rules are
/// stated in.
///
/// The policy is a parameter rather than the balances alone because summing
/// across currencies is the one thing this must never do. Every rule downstream
/// values the policy currency and nothing else, and the venue owns no rate
/// surface - so an anchor that counted a EUR balance toward a USD policy would
/// open the account above any equity it can ever observe, and the first honest
/// reading would look like a loss of exactly the foreign balance. That is a
/// liquidation with no cause. `open_account` refuses such a configuration by
/// name; this signature is what keeps the refusal from being the only thing
/// standing between the two, since a caller here cannot reach the balances
/// without stating the policy that gives them meaning.
///
/// An unpoliced policy names no currency and enforces nothing, so its anchor is
/// read by no rule; the sum is retained there because it is the number the
/// account's own opening report has always carried.
fn opening_equity(
    balances: &std::collections::HashMap<String, Decimal>,
    policy: &mogwai_protocol::risk::AccountPolicy,
) -> Decimal {
    if let Some(currency) = policy.currency.as_ref().filter(|_| !policy.is_unpoliced()) {
        return balances.get(currency.as_str()).copied().unwrap_or_default();
    }
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
    /// Opening funding in a currency the policy's rules cannot value. Refused
    /// rather than converted at parity: see `open_account`.
    ForeignOpeningBalance {
        currency: String,
        policy_currency: String,
    },
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
            Self::ForeignOpeningBalance {
                currency,
                policy_currency,
            } => write!(
                f,
                "this account opens with {currency} under a policy stated in {policy_currency}; a \
                 policed account may hold only its policy currency, because equity is computed in \
                 that currency alone and the venue has no exchange rate - counting the two \
                 together would anchor the account above any equity it can observe and liquidate \
                 it on its first mark"
            ),
        }
    }
}

/// One live connection: its identity, the account it trades under, and its
/// outbound machinery. The account rides along because delivery is attributed
/// by account - two sockets on one account are one trader and both hear about
/// its fills - while retirement is per connection.
#[derive(Clone)]
/// Who a swept frame is for. Every `VenueMessage` variant is classified here,
/// exhaustively and deliberately: [`audience`] carries no catch-all, so a new
/// variant does not compile until someone decides who it belongs to. That is
/// the whole point. Delivery used to attribute by two chained lookups whose
/// shared default was "unrecognized means everyone", and `AccountState` rode
/// that default silently - the sweep runs one engine pass per account, so an
/// N-account venue handed every socket N snapshots per pass, N-1 of them
/// somebody else's balances and positions. A consumer had no way to tell: the
/// snapshot names its account, but a consumer promised one ledger per run has no
/// reason to check, and the known nautilus adapter deliberately does not.
/// Sizing off a sibling's equity was the consequence, and it moves capital.
/// The next ledger-owned frame variant would have joined the broadcast set the
/// same silent way; now it is a compile error instead.
pub(crate) enum Audience<'a> {
    /// Genuinely about the venue - a completion, a fault, a feed gap, market
    /// data - and belongs to every connection.
    Venue,
    /// Owned by the account the frame itself names. Attribution by the frame's
    /// own field rather than by which account the sweep was iterating,
    /// deliberately: delivery is a pure function of the batch, and a frame that
    /// can say who it belongs to must not be attributed by ambient context
    /// that a later refactor can silently change.
    Account(&'a mogwai_protocol::AccountId),
    /// Owned by whoever submitted the named order; delivery resolves the owner
    /// through the run's ownership table. A venue-originated order, such as a
    /// risk or margin liquidation, has no submitter but is claimed for the
    /// account whose ledger produced it, at production time, by
    /// [`Run::claim_produced_orders`] - so every order-scoped frame that reaches
    /// delivery is expected to resolve. An order the table does not know is
    /// therefore a claim some production site failed to make, not a class of
    /// order; delivery falls back to everyone, which is the conservative
    /// direction of the failure rather than the intended path.
    Order(&'a mogwai_protocol::VenueOrderId),
    /// Order-scoped, but the venue never recognized the order, so there is no
    /// id to resolve an owner from: a submit rejection, or a modify/cancel
    /// rejection naming an unknown id. Goes to everyone - right rather than
    /// merely conservative, because the only connection that could care is the
    /// one that asked, and it is in that set.
    Unattributable,
    /// Belongs to the connection that issued the request - a query reply, an
    /// admission refusal, a protocol error - which the swept-delivery path
    /// cannot know. These frames are delivered on the issuing lane at the
    /// point of refusal or reply and MUST NOT enter a swept batch; delivery
    /// drops one that does, loudly, because broadcasting it would leak one
    /// consumer's orders, fills or refusals to every other.
    Requester,
}

/// Classify one frame for swept delivery. Exhaustive over `VenueMessage` with
/// no catch-all - see [`Audience`] for why, and keep it that way.
pub(crate) fn audience(event: &mogwai_protocol::VenueMessage) -> Audience<'_> {
    use mogwai_protocol::VenueMessage as M;
    match event {
        M::RunComplete { .. }
        | M::Heartbeat { .. }
        | M::FeedLagged { .. }
        | M::HavocDiagnostic { .. }
        | M::Trade(_)
        | M::Quote(_)
        | M::FundingRate { .. } => Audience::Venue,
        M::AccountState(state) => Audience::Account(&state.account_id),
        M::OrderAccepted { venue_order_id, .. }
        | M::OrderTriggered { venue_order_id, .. }
        | M::OrderCanceled { venue_order_id, .. }
        | M::OrderExpired { venue_order_id, .. }
        | M::OrderUpdated { venue_order_id, .. } => Audience::Order(venue_order_id),
        M::OrderModifyRejected { venue_order_id, .. }
        | M::OrderCancelRejected { venue_order_id, .. } => match venue_order_id {
            Some(id) => Audience::Order(id),
            None => Audience::Unattributable,
        },
        M::OrderFilled(fill) => Audience::Order(&fill.venue_order_id),
        M::OrderRejected { .. } => Audience::Unattributable,
        M::AdmissionRejected { .. }
        | M::OrderStatusSnapshot(_)
        | M::FillSnapshot(_)
        | M::ProtocolError { .. }
        // One socket's own deadline, and true of no other passenger - so it is
        // never `Venue`, even though it sits beside `RunComplete` in the terminal
        // vocabulary. Its producer writes it straight to the socket that owns
        // it and it never enters a swept batch; classifying it venue-wide would
        // make this classifier lie today and broadcast one passenger's
        // completion to the whole run if it ever reached the sweep.
        // A page answers the socket that asked for it, and only that one. It
        // carries the requester's own river, which after the fork need not be
        // the river any other passenger on the same label is reading, so
        // broadcasting it would hand one consumer another's water as well as
        // another's correlation id.
        | M::HistoryPage { .. }
        | M::HistoryRejected { .. }
        | M::PassengerDurationComplete { .. } => Audience::Requester,
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

    /// A zero span is the only route off a window: the
    /// control plane has no clear, so `GoDark { ms: 0 }` and `StallData
    /// { ms: 0 }` are what `control.rs` documents as lifting an armed window.
    /// The property that makes that honest is that a zero span is open for no
    /// clock at all - `open_at` wants `sim_at_ns` both at or after the opening
    /// and strictly before `opening + span`, which no instant satisfies when
    /// the span is zero. A late boarder is covered too: it opens at its own
    /// epoch and still consumes a span of zero.
    #[test]
    fn a_zero_span_window_is_open_for_no_clock() {
        let window = HavocWindow::new();
        window.arm(10, 100);
        window.arm(20, 0);
        assert!(!window.open_at(sim(0, 1.0), 1_010));
        assert!(!window.open_at(sim(0, 1.0), 1_020));
        let late = sim(2_000, 5.0);
        assert!(!window.open_at(late, late.sim_epoch_ns));
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
    fn concurrent_arm_and_read_never_observe_a_torn_span() {
        let window = Arc::new(HavocWindow::new());
        let writer = Arc::clone(&window);
        let handle = std::thread::spawn(move || {
            for i in 0..10_000 {
                writer.arm(i, i.saturating_add(1));
                // The zero-span lift, which is a write like any other arm -
                // that is the point here, since the two writes alternate and a
                // reader must see one whole span or the other.
                writer.arm(i, 0);
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

/// Admit a connection through the real reserve-and-commit path, for tests in
/// sibling modules. Out here for the same reason `test_run` is.
#[cfg(test)]
pub(crate) fn admit_for_test(
    run: &Arc<Run>,
    account_id: &mogwai_protocol::AccountId,
    callsign: Option<&str>,
) -> (Attach, crate::registry::Committed) {
    tests::admit(run, account_id, callsign, None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn run(started_ns: u64, warmup_ns: u64, run_duration_ns: Option<u64>) -> Arc<Run> {
        run_with_reset(started_ns, warmup_ns, run_duration_ns, false)
    }

    fn run_with_reset(
        started_ns: u64,
        warmup_ns: u64,
        run_duration_ns: Option<u64>,
        reset_account_on_reconnect: bool,
    ) -> Arc<Run> {
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
                started_ns,
                profiles,
            ),
            std::collections::HashMap::new(),
            SimClock::identity(),
            started_ns,
            warmup_ns,
            run_duration_ns,
            RunSeeds::from_run_seed(42),
            8,
            mogwai_protocol::OmsType::Netting,
            200,
            mogwai_protocol::AccountId::parse(crate::config::DEFAULT_ACCOUNT_ID)
                .expect("the default account id is legal"),
            reset_account_on_reconnect,
            std::collections::HashMap::new(),
            std::sync::mpsc::channel().0,
        )
    }

    /// The reconnect reset replaces the ledger and nothing else. It used to
    /// call `registry.forget`, which removed the connection record the
    /// admission had just committed - so the connection bound no lanes, rode no
    /// boat, and its live account read as unattended to the TTL collector.
    ///
    /// Sequenced exactly as `/ws` sequences it: sample the identity, reserve,
    /// replace the ledger inside the reservation's exclusive window, then
    /// commit. That ordering is what makes the incarnation a boundary, so a
    /// test that ran the reset after the commit would be pinning a shape the
    /// handler no longer has.
    #[test]
    fn reconnect_reset_keeps_the_committed_connection_ride_and_lanes() {
        let run = run_with_reset(1_000, 400, None, true);
        let account = mogwai_protocol::AccountId::parse("RESET-001").unwrap();
        let old = run.account(&account);
        let key = boat_key(&run, "BTCUSDT", 1.0);
        let observed = run.ledger_incarnation(&account);
        let resetting = run.claim_discards_ledger(&account, true, Some("new"));
        assert!(
            resetting,
            "the configured reconnect path must actually reset"
        );

        let mut reservation = run
            .reserve_admission(
                account.as_str(),
                crate::registry::Seat {
                    river: key.river().clone(),
                    speed_micros: key.speed_micros(),
                    bounds: key.bounds(),
                },
                resetting,
                observed,
            )
            .unwrap();
        let fresh = run.claim_account(&account, true, Some("new"), resetting);
        assert!(!Arc::ptr_eq(&old, &fresh), "the ledger itself was replaced");
        let (attach, committed) = run
            .commit_admission(&mut reservation, Some("new"), Some(key.clone()), true)
            .expect("a live reservation commits");

        let (lanes, _rx) = ExecLanes::detached_as(committed.connection_id);
        run.bind_lanes(lanes, account.as_str(), Some("new"));
        assert!(
            run.bound_lanes()
                .iter()
                .any(|bound| bound.account_id == account.as_str()),
            "the reset did not erase the connection the admission committed"
        );
        assert!(
            fresh.is_seated_on(&key),
            "the committed ride survived reset"
        );
        assert!(!fresh.is_frozen(), "the live reset connection is attended");
        assert_ne!(
            run.ledger_incarnation(&account),
            observed,
            "the committed reset advanced the identity the replacement is paired with"
        );
        drop(attach);
    }

    /// Pins [`audience`]'s verdicts on every discriminating case, so a
    /// transposition fails by name. The completeness half is the compiler's:
    /// `audience` carries no catch-all, so a new `VenueMessage` variant does
    /// not build until it is classified, and this test cannot and does not
    /// try to hold that.
    #[test]
    fn every_swept_frame_class_is_attributed_deliberately() {
        use mogwai_protocol::VenueMessage as M;
        let order = |venue_order_id: &str| M::OrderAccepted {
            client_order_id: "C-1".to_string(),
            venue_order_id: venue_order_id.to_string(),
            ts_event: 1,
        };
        let account = mogwai_protocol::AccountId::parse("ACCT-X").expect("legal id");

        assert!(matches!(
            audience(&M::RunComplete {
                sim_now_ns: 1,
                elapsed_ns: 1
            }),
            Audience::Venue
        ));
        assert!(matches!(
            audience(&M::FeedLagged {
                episode: 1,
                skipped: 1,
                skipped_total: 1,
                after_ts_event: Some(1),
                resumed_ts_event: Some(2),
            }),
            Audience::Venue
        ));
        assert!(matches!(
            audience(&M::AccountState(mogwai_protocol::AccountState {
                account_id: account.clone(),
                balances: vec![],
                positions: vec![],
                margins: vec![],
                risk: None,
                ts_event: 1,
            })),
            Audience::Account(id) if id == &account
        ));
        assert!(matches!(
            audience(&order("V-1")),
            Audience::Order(id) if id.as_str() == "V-1"
        ));
        assert!(matches!(
            audience(&M::OrderCancelRejected {
                client_order_id: "C-1".to_string(),
                venue_order_id: Some("V-2".to_string()),
                reason: "illegal".to_string(),
                ts_event: 1,
            }),
            Audience::Order(id) if id.as_str() == "V-2"
        ));
        assert!(matches!(
            audience(&M::OrderCancelRejected {
                client_order_id: "C-1".to_string(),
                venue_order_id: None,
                reason: "unknown order".to_string(),
                ts_event: 1,
            }),
            Audience::Unattributable
        ));
        assert!(matches!(
            audience(&M::OrderRejected {
                client_order_id: "C-1".to_string(),
                reason: "no".to_string(),
                ts_event: 1,
            }),
            Audience::Unattributable
        ));
        assert!(matches!(
            audience(&M::OrderStatusSnapshot(
                mogwai_protocol::OrderStatusSnapshot {
                    request_id: "R-1".to_string(),
                    orders: vec![],
                    ts_event: 1,
                }
            )),
            Audience::Requester
        ));
        assert!(matches!(
            audience(&M::ProtocolError {
                reason: "bad frame".to_string(),
                ts_event: 1,
            }),
            Audience::Requester
        ));
    }

    /// The planned-completion shutdown waits on a live passenger and not merely
    /// on the accept loop.
    ///
    /// The differential is the point, and both halves are asserted because the
    /// second alone passes against the broken shape too. `passengers_drained`
    /// must not resolve while a guard is held - that is the whole defect: the
    /// venue's serve future can complete while a passenger is still
    /// mid-announcement, because axum stops tracking an upgraded connection at
    /// the 101 - and it must resolve promptly once the last guard drops, or a
    /// clean completion would idle out the shutdown grace and bail.
    ///
    /// A wall timeout is the observable here rather than a bet, because the
    /// negative half of a "does not resolve" claim has no positive event to
    /// wait for. 200 ms against an operation that is a single atomic wake is
    /// three orders of magnitude of margin, and losing it means the guard was
    /// ignored outright rather than that the host was slow.
    #[tokio::test]
    async fn a_live_passenger_guard_holds_the_shutdown_open() {
        let run = run(1_000, 400, None);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                run.passengers_drained()
            )
            .await
            .is_ok(),
            "with no passenger live the venue owes nobody an announcement and must not wait"
        );

        let passenger = run.passenger_guard();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                run.passengers_drained()
            )
            .await
            .is_err(),
            "a live passenger still owes its RunComplete and its close; exiting here is what \
             reset the peer instead of announcing to it"
        );

        drop(passenger);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                run.passengers_drained()
            )
            .await
            .is_ok(),
            "the last passenger ended, so the wait must release rather than run out the grace"
        );
    }

    /// A query reply carries the asking connection's orders and nobody else's.
    ///
    /// The engine answers from one book because there is one ledger, so without
    /// this scoping an account polling `QueryOrders` reads every other
    /// account's order history - including their terminal rows, which is why
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
        let mut events = vec![mogwai_protocol::VenueMessage::OrderStatusSnapshot(
            mogwai_protocol::OrderStatusSnapshot {
                request_id: "R-1".into(),
                orders: vec![row("V-mine"), row("V-theirs")],
                ts_event: 1,
            },
        )];
        run.scope_query_rows(&mut events, "MOGWAI-001");

        let mogwai_protocol::VenueMessage::OrderStatusSnapshot(snapshot) = &events[0] else {
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
        let account_state = run.account(&account);
        assert!(
            account_state.is_frozen(),
            "an account nobody has connected to is unattended like any other"
        );

        let (_first_attach, first) = admit(&run, &account, Some("leg"), None);
        let (first_lanes, _first_rx) =
            crate::admission::ExecLanes::detached_as(first.connection_id);
        let first_id = run.bind_lanes(first_lanes, account.as_str(), None);
        assert!(!account_state.is_frozen());

        // A second connection on the same account: retiring the first must not
        // freeze an account somebody is still reading. This is the eviction
        // shape, where the incumbent is retired while the newcomer binds.
        let (_second_attach, second) = admit(&run, &account, Some("leg"), None);
        let (second_lanes, _second_rx) =
            crate::admission::ExecLanes::detached_as(second.connection_id);
        let second_id = run.bind_lanes(second_lanes, account.as_str(), None);
        run.release_lanes(account.as_str(), first_id);
        assert!(
            !account_state.is_frozen(),
            "a connection is still reading this account"
        );

        run.release_lanes(account.as_str(), second_id);
        assert!(account_state.is_frozen(), "the last one left");
    }

    /// An evicted incumbent still owes the freeze. The account must end up
    /// frozen: collectable, because `collect_expired_accounts` filters on
    /// `frozen_for()`, and out of the sweep, because a swept account riding no
    /// boat has its resting orders cancelled.
    #[test]
    fn an_evicted_account_freezes_when_the_incumbent_finishes_tearing_down() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("FREEZE-002").unwrap();
        let account_state = run.account(&account);
        let (incumbent_attach, incumbent) = admit(&run, &account, Some("alpha"), None);
        let (lanes, _rx) = crate::admission::ExecLanes::detached_as(incumbent.connection_id);
        let incumbent_id = run.bind_lanes(lanes, account.as_str(), Some("alpha"));

        // A stranger claims the account and then goes away without ever
        // binding - the upgrade abandoned after the 101 - so the incumbent's own
        // teardown is the last thing that touches the account.
        let (stranger_attach, stranger) = admit(&run, &account, Some("beta"), None);
        assert_eq!(
            run.close_displaced(account.as_str(), &stranger.displaced),
            1,
            "a different callsign displaces the incumbent"
        );
        run.release_lanes(account.as_str(), incumbent_id);
        drop(incumbent_attach);
        drop(stranger_attach);

        assert!(
            account_state.is_frozen(),
            "nothing is reading this account, so it must be frozen: collectable, and out of the \
             sweep"
        );
        assert!(
            account_state.frozen_for().is_some(),
            "and the TTL reaper reads exactly that"
        );
    }

    /// The continuity lease, which is the subtlety the registry turns on. An
    /// account is not frozen out from under a committed socket that has not
    /// begun reading yet. That window is real - it spans the 101 and everything
    /// `handle_socket` does before `bind_lanes` - and freezing inside it would
    /// retire a book and re-base every scan frontier because a socket arrived,
    /// nondeterministically, while leaving a live connection trading a ledger
    /// the sweeper skips.
    #[test]
    fn a_committed_socket_holds_the_account_open_before_it_binds_a_lane() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("FREEZE-003").unwrap();
        let account_state = run.account(&account);
        let (incumbent_attach, incumbent) = admit(&run, &account, Some("alpha"), None);
        let (lanes, _rx) = crate::admission::ExecLanes::detached_as(incumbent.connection_id);
        let incumbent_id = run.bind_lanes(lanes, account.as_str(), Some("alpha"));

        // The newcomer commits before the incumbent is closed, exactly as the
        // upgrade does it, and holds a handoff until it reads.
        let (newcomer_attach, newcomer) = admit(&run, &account, Some("beta"), None);
        assert_eq!(
            run.close_displaced(account.as_str(), &newcomer.displaced),
            1
        );
        run.release_lanes(account.as_str(), incumbent_id);
        drop(incumbent_attach);
        assert!(
            !account_state.is_frozen(),
            "the newcomer is on its way in, so the account is still being read"
        );

        drop(newcomer_attach);
        assert!(
            account_state.is_frozen(),
            "and when that newcomer goes without ever binding, the account freezes"
        );
    }

    /// Claim an account the way `/ws` does: derive `resetting` once,
    /// before anything is evicted, and hand it to `claim_account`. A test that spelled
    /// the predicate out for itself would be pinning the rule against its own
    /// copy of it rather than against the venue's.
    fn claim_account(
        run: &Arc<Run>,
        account_id: &mogwai_protocol::AccountId,
        callsign: Option<&str>,
    ) -> Arc<Account> {
        let resetting = run.claim_discards_ledger(account_id, true, callsign);
        run.claim_account(account_id, true, callsign, resetting)
    }

    fn boat_key(run: &Run, symbol: &str, speed: f64) -> crate::boatyard::BoatKey {
        let profile = run.rivers.resolve_profile(symbol).expect("a served symbol");
        crate::boatyard::BoatKey::new(run.rivers.resolve_key(&profile, None), speed)
            .expect("a legal speed")
    }

    /// Admit a connection the way the upgrade does: reserve, then commit.
    ///
    /// Tests go through the real admission rather than installing a connection
    /// by hand, because the properties they are about - when an account is
    /// attended, what an admission displaces, whether a ride is held - are all
    /// decided inside it.
    pub(super) fn admit(
        run: &Arc<Run>,
        account_id: &mogwai_protocol::AccountId,
        callsign: Option<&str>,
        ride: Option<crate::boatyard::BoatKey>,
    ) -> (Attach, crate::registry::Committed) {
        admit_at(run, account_id, callsign, ride, 1.0)
    }

    /// `admit`, naming the cadence, for the tests about one ledger carrying one.
    fn admit_at(
        run: &Arc<Run>,
        account_id: &mogwai_protocol::AccountId,
        callsign: Option<&str>,
        ride: Option<crate::boatyard::BoatKey>,
        speed: f64,
    ) -> (Attach, crate::registry::Committed) {
        let seat = crate::registry::Seat {
            river: ride
                .as_ref()
                .map_or_else(|| run.rivers.test_key("BTCUSDT"), |key| key.river().clone()),
            speed_micros: crate::boatyard::quantize_speed(speed).expect("a legal speed"),
            bounds: ride.as_ref().and_then(crate::boatyard::BoatKey::bounds),
        };
        let mut reservation = run
            .reserve_admission(
                account_id.as_str(),
                seat,
                false,
                run.ledger_incarnation(account_id),
            )
            .expect("nothing else is being admitted");
        run.commit_admission(&mut reservation, callsign, ride, true)
            .expect("a live reservation commits")
    }

    /// Two passengers of one ledger may share one boat, and the first to leave
    /// must not take the ride away from the second, or the sweeper stops
    /// applying that boat to a ledger somebody is still trading.
    ///
    /// Nothing counts here. The ride is a field on each connection record, so
    /// two records naming one boat is what sharing is, and removing one record
    /// cannot affect the other.
    #[test]
    fn a_ride_shared_by_two_passengers_survives_the_first_leaving() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("SEAT-001").unwrap();
        let account_state = run.account(&account);
        let key = boat_key(&run, "BTCUSDT", 2.0);

        let (first, _) = admit_at(&run, &account, Some("leg"), Some(key.clone()), 2.0);
        let (second, _) = admit_at(&run, &account, Some("leg"), Some(key.clone()), 2.0);
        drop(first);
        assert!(
            account_state.is_seated_on(&key),
            "one passenger left, one is still riding"
        );
        drop(second);
        assert!(
            !account_state.is_seated_on(&key),
            "the last passenger left the boat"
        );
    }

    /// One ledger, one cadence, and the rule is account-wide rather than per
    /// river: a ledger judged on two clocks would let a fill at one simulated
    /// instant fund an order judged at another.
    ///
    /// It holds while the account is frozen, which is the state every account
    /// is created in, so two first connections racing two speeds both meet it
    /// and exactly one may win.
    #[test]
    fn an_account_may_not_ride_one_river_at_two_cadences() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("SEAT-002").unwrap();
        let account_state = run.account(&account);
        assert!(account_state.is_frozen(), "a fresh account is unattended");

        let key = boat_key(&run, "BTCUSDT", 2.0);
        let (_first, _) = admit_at(&run, &account, Some("leg"), Some(key.clone()), 2.0);
        let refused = run
            .reserve_admission(
                account.as_str(),
                crate::registry::Seat {
                    river: key.river().clone(),
                    speed_micros: crate::boatyard::quantize_speed(3.0).unwrap(),
                    bounds: key.bounds(),
                },
                false,
                run.ledger_incarnation(&account),
            )
            .expect_err("a second cadence on one river is refused");
        let crate::registry::AdmissionRefusal::CadenceConflict(held) = refused else {
            panic!("the refusal names the conflict, not a busy admission");
        };
        assert_eq!(held.speed(), 2.0, "the refusal names the sitting cadence");
    }

    /// A ride ends when its connection does, not at the freeze - an account
    /// riding two rivers that loses one socket never freezes. A stale ride would
    /// be indistinguishable from a live one and would hand this ledger a boat it
    /// never boarded.
    #[test]
    fn leaving_one_river_frees_that_ride_while_another_is_still_ridden() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("SEAT-003").unwrap();
        let account_state = run.account(&account);
        let slow = boat_key(&run, "BTCUSDT", 2.0);

        let (rider, _) = admit_at(&run, &account, Some("leg"), Some(slow.clone()), 2.0);
        drop(rider);
        assert!(
            !account_state.is_seated_on(&slow),
            "the ride ended with the connection, no freeze required"
        );
    }

    /// An eviction-reconnect carries a scan frontier off a cursor that is gone.
    ///
    /// `resume` re-bases every surviving order's frontier onto the binding
    /// boat's clock, and it does so only for a returning account - one it found
    /// frozen. Eviction deliberately defeats that test: `ws_upgrade` counts the
    /// newcomer onto the account before closing the incumbent, so the account
    /// never goes unattended and `resume` sees `returning == false`.
    ///
    /// For the case that ordering was aimed at - a consumer reconnecting on the
    /// same river - nothing is lost, because the newcomer boards the same boat
    /// and the cursor never rewound. The gap is the cross-river claim: the
    /// newcomer boards a different `BoatKey`, the incumbent's ticket drops, and
    /// the departed river's boat is torn down with its worker. A boat placed
    /// over that river again starts at the yard's origin - `BoatKey` carries no
    /// placement nonce, so it is the same key and `is_seated_on` still passes -
    /// while the order's frontier sits wherever the first cursor reached. The
    /// sweeper then judges that order only on ticks after a bound in the new
    /// cursor's future, so it rests unscanned until the new cursor has covered
    /// the whole of the first session again.
    ///
    /// Closed by asking the state itself rather than repairing the proxy: a
    /// frontier that leads the binding cursor is never legitimate, whatever put
    /// it there, so `resume` re-bases exactly those on every bind. The
    /// alternative was a placement nonce on `Boat`, which would let the freeze
    /// proxy be repaired but needs a new identity to carry.
    ///
    /// Pinned here rather than on a socket because the symptom is a stall whose
    /// length is the previous session's, and an absence assertion over a `/ws`
    /// drain would be satisfied for free by a truncated one. The frontier is
    /// the thing itself.
    #[tokio::test]
    async fn an_eviction_reconnect_rebases_a_frontier_from_the_departed_cursor() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("FRONTIER-001").unwrap();
        let symbol = Symbol::from("BTCUSDT");
        let key = boat_key(&run, "BTCUSDT", 1.0);

        let account_state = claim_account(&run, &account, Some("alpha"));
        let (incumbent_attach, incumbent) = admit(&run, &account, Some("alpha"), Some(key.clone()));
        let (lanes, _rx) = ExecLanes::detached_as(incumbent.connection_id);
        let incumbent_id = run.bind_lanes(lanes, account.as_str(), Some("alpha"));

        // A limit far below the tape, so it rests rather than filling: the
        // frontier only exists on an order the sweeper still has to judge.
        // Accepted at 5_000 on the incumbent's cursor, which is what puts the
        // frontier there.
        let accepted_ns = 5_000;
        run.ensure_instrument(&account_state, "BTCUSDT")
            .await
            .expect("the boot river's own symbol resolves");
        {
            let mut engine = account_state.engine.lock().await;
            let events = engine.process(
                mogwai_protocol::Command::SubmitOrder(mogwai_protocol::SubmitOrder {
                    client_order_id: "FRONTIER-ORDER".into(),
                    symbol: Symbol::clone(&symbol),
                    position_id: None,
                    side: mogwai_protocol::Side::Buy,
                    order_type: mogwai_protocol::OrderType::Limit,
                    quantity: Decimal::ONE,
                    price: Some(Decimal::ONE),
                    trigger_price: None,
                    trail_offset: None,
                    limit_offset: None,
                    reduce_only: false,
                    post_only: false,
                    time_in_force: mogwai_protocol::TimeInForce::Gtc,
                    expire_time: None,
                    link: None,
                }),
                accepted_ns,
            );
            // Asserted, not assumed: a refused order rests nothing, and every
            // assertion below would then hold vacuously over an empty book.
            assert!(
                events.iter().any(|event| matches!(
                    event,
                    mogwai_protocol::VenueMessage::OrderAccepted { .. }
                )),
                "the limit was not accepted, so this run says nothing about frontiers: {events:?}"
            );
            let open = engine.open_orders();
            assert_eq!(open.len(), 1, "the limit did not rest: {open:?}");
            assert_eq!(
                open[0].scanned_ns, accepted_ns,
                "an accepted order's frontier starts at its acceptance"
            );
        }

        // The newcomer is counted on before the eviction, which is the ordering
        // `ws_upgrade` takes and the reason the freeze never fires.
        let (newcomer_attach, newcomer) = admit(&run, &account, Some("beta"), None);
        assert_eq!(
            run.close_displaced(account.as_str(), &newcomer.displaced),
            1,
            "the incumbent is displaced by a claim under a different callsign"
        );
        // The incumbent's teardown. Removing its connection record gives up its
        // lane and its ride in one transition, which is what takes this account
        // off that boat.
        run.release_lanes(account.as_str(), incumbent_id);
        drop(incumbent_attach);
        assert!(
            !account_state.is_frozen(),
            "the newcomer was already counted on, so the account never went unattended - which is \
             precisely what makes `resume` treat this as a first bind"
        );

        // The newcomer binds a river of its own, on a cursor at the origin.
        let resumed_ns = 1_000;
        let events = run
            .resume(
                &account_state,
                &symbol,
                resumed_ns,
                newcomer.resumed_from_freeze,
            )
            .await;
        assert!(
            events.is_empty(),
            "an unfrozen account is not retired, which is the behaviour under test rather than a \
             surprise: {events:?}"
        );

        let engine = account_state.engine.lock().await;
        let frontier = engine.open_orders()[0].scanned_ns;
        assert_eq!(
            frontier, resumed_ns,
            "the frontier still names the departed cursor's clock, so every scan window this order \
             is judged on is empty until the binding cursor has covered the whole first session \
             again - silently, and looking exactly like an order nothing has hit yet"
        );
        assert!(
            frontier <= resumed_ns,
            "a frontier may trail the cursor serving it and must never lead it"
        );
        drop(newcomer_attach);
    }

    /// A consumer is not a socket. The nautilus host that drives this venue holds
    /// two sockets on one ledger - data and execution - and both name the same
    /// account, so eviction keyed on the bare id made the host's second dial
    /// disconnect its own first. Sockets presenting one callsign coexist; a
    /// different callsign takes over. The venue decides on the callsign it was
    /// shown and never on the consumer behind it, which it cannot see.
    #[test]
    fn one_callsigns_sockets_share_a_ledger_and_a_stranger_evicts_them_all() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("CLAUDETTE-07").unwrap();

        claim_account(&run, &account, Some("worker-1"));
        let (_data_attach, data) = admit(&run, &account, Some("worker-1"), None);
        let (data_lanes, _data_rx) = crate::admission::ExecLanes::detached_as(data.connection_id);
        run.bind_lanes(data_lanes, account.as_str(), Some("worker-1"));

        let (_exec_attach, exec) = admit(&run, &account, Some("worker-1"), None);
        assert_eq!(
            run.close_displaced(account.as_str(), &exec.displaced),
            0,
            "the host's second leg presents the same callsign, not a claim"
        );
        let (exec_lanes, _exec_rx) = crate::admission::ExecLanes::detached_as(exec.connection_id);
        run.bind_lanes(exec_lanes, account.as_str(), Some("worker-1"));
        assert_eq!(
            run.bound_lanes()
                .iter()
                .filter(|bound| bound.account_id == "CLAUDETTE-07")
                .count(),
            2,
            "both legs are reading the ledger they were configured for"
        );

        // A restarted worker presents a genuinely new callsign, and it takes the
        // whole ledger: both stale sockets go, which is the reconnection story
        // the eviction exists for.
        let (_new_attach, restarted) = admit(&run, &account, Some("worker-2"), None);
        assert_eq!(
            run.close_displaced(account.as_str(), &restarted.displaced),
            2,
            "a different callsign displaces every socket of the old one"
        );
        assert!(
            run.bound_lanes()
                .iter()
                .all(|bound| bound.account_id != "CLAUDETTE-07"),
        );
    }

    /// A ledger minted after an unqualified arm carries it. This is the whole of
    /// finding 5: the control plane walked the accounts that existed at the
    /// instant of the request, so an operator who armed a `PartialFillNext` and
    /// then started a subagent got a run that believed it was perturbed and was
    /// not - while `docs/havoc.md` and `arm_divergence`'s own comment both said
    /// the arm reaches a passenger that boards later.
    ///
    /// Three observables, one per storage class, because the three are applied
    /// by three different lines and any of them can be dropped alone.
    ///
    /// The engine queue is observed through the cap, which is the only reader
    /// `mogwai-engine` exposes: fill the venue record to
    /// `MAX_ARMED_DIVERGENCES` with no ledger open at all, mint one, and arm once
    /// more. A ledger that replayed the record is at the cap and sheds its
    /// oldest entry, so the arm reports one; a ledger opened empty has room and
    /// reports nothing. That also pins the mirroring the eviction report's
    /// wording rests on - "every ledger holds the same arms and hits the cap
    /// together" - rather than only the fact of a replay.
    ///
    /// The fee surcharge is observed through a real fill on the late ledger,
    /// which keeps the engine's private window private while pinning the value a
    /// consumer actually receives.
    #[tokio::test]
    async fn a_ledger_minted_after_a_venue_wide_arm_carries_it() {
        let run = run(1_000, 400, None);
        fill_venue_record(&run).await;

        // The subagent that started after the operator armed.
        let late = run.account(&mogwai_protocol::AccountId::parse("LATE-001").unwrap());

        assert!(
            late.dark.open_at(SimClock::identity(), 1_000),
            "a blackout armed venue-wide before this account existed must still black it out"
        );
        assert_eq!(
            late.delay_ms.load(Ordering::Relaxed),
            37,
            "an ack delay armed venue-wide must reach a ledger minted afterwards"
        );
        let shed = run
            .arm(
                None,
                VenueArm::Engine(mogwai_protocol::control::Divergence::DuplicateNextFill),
            )
            .await;
        assert!(
            shed.is_some(),
            "the minted ledger must open holding the venue's armed queue, at the cap with the \
             ledgers that already existed"
        );
        assert_eq!(
            commission_on_one_fill(&run, &late, "LATE-FEE").await,
            Decimal::from(3),
            "a surcharge armed venue-wide must price a fill on a ledger minted afterwards"
        );
    }

    /// The second mint site owes the same replay. `POST /accounts` builds its own
    /// ledger from consumer-named balances, and the cold review found it opening
    /// that ledger with every havoc field at zero - so a subagent that starts by
    /// POSTing its account escaped the operator's arms entirely, which is
    /// verbatim the scenario finding 5 names.
    ///
    /// The three observables and the cap trick are `a_ledger_minted_after_a_
    /// venue_wide_arm_carries_it`'s; what is new here is only the path.
    #[tokio::test]
    async fn an_account_opened_by_its_consumer_carries_the_venue_wide_arms() {
        let run = run(1_000, 400, None);
        fill_venue_record(&run).await;

        let posted = mogwai_protocol::AccountId::parse("POST-001").unwrap();
        run.open_account(
            &posted,
            std::collections::HashMap::from([("USD".to_owned(), Decimal::from(50_000))]),
            mogwai_protocol::risk::AccountPolicy::default(),
        )
        .expect("a fresh id opens");
        let opened = run.peek_account(&posted).expect("just opened");

        assert!(
            opened.dark.open_at(SimClock::identity(), 1_000),
            "a blackout armed venue-wide must reach an account its consumer opened afterwards"
        );
        assert_eq!(
            opened.delay_ms.load(Ordering::Relaxed),
            37,
            "an ack delay armed venue-wide must reach a consumer-opened account too"
        );
        let shed = run
            .arm(
                None,
                VenueArm::Engine(mogwai_protocol::control::Divergence::DuplicateNextFill),
            )
            .await;
        assert!(
            shed.is_some(),
            "a consumer-opened ledger must hold the venue's armed queue, at the cap with the \
             ledgers that already existed"
        );
    }

    /// The other door into the same ledger. `AccountPolicy::validate` refuses a
    /// policy whose own `opening_balances` leave the policy currency, but a
    /// request may state `balances` explicitly and those take precedence, so
    /// the refusal has to live where the two converge or it guards only half
    /// the path. A USD policy funded 50,000 USD plus 50,000 EUR would anchor at
    /// 100,000 under a parity sum, then read its first honest observation -
    /// 50,000, the USD leg alone - as a 50,000 loss and liquidate an account
    /// that has not traded. The venue holds no rate surface to value the EUR
    /// with, so the configuration is refused by name.
    #[tokio::test]
    async fn a_policed_account_may_not_open_outside_its_policy_currency() {
        let run = run(1_000, 400, None);
        let policy = mogwai_protocol::risk::AccountPolicy {
            currency: Some("USD".to_owned()),
            overall_drawdown: Some(mogwai_protocol::risk::OverallDrawdown {
                amount: Decimal::from(5_000),
                on_breach: mogwai_protocol::risk::BreachAction::Terminate,
            }),
            ..Default::default()
        };
        let mixed = mogwai_protocol::AccountId::parse("MIXED-001").unwrap();
        let refusal = run
            .open_account(
                &mixed,
                std::collections::HashMap::from([
                    ("USD".to_owned(), Decimal::from(50_000)),
                    ("EUR".to_owned(), Decimal::from(50_000)),
                ]),
                policy.clone(),
            )
            .expect_err("a policed account may not open in a currency its rules cannot value");
        assert!(
            matches!(refusal, AccountRefusal::ForeignOpeningBalance { .. }),
            "the refusal must name the currency mismatch, got {refusal:?}"
        );
        assert!(
            run.peek_account(&mixed).is_none(),
            "a refused open must leave no ledger behind"
        );

        // The same policy in its own currency opens, and anchors at exactly the
        // funding rather than at anything summed.
        let clean = mogwai_protocol::AccountId::parse("CLEAN-001").unwrap();
        run.open_account(
            &clean,
            std::collections::HashMap::from([("USD".to_owned(), Decimal::from(50_000))]),
            policy,
        )
        .expect("opening in the policy currency alone is the supported shape");
    }

    /// The boarding door reads the ledger this claim will be served on, which
    /// is not always the ledger that exists. Three readings, one per branch:
    /// a resuming claim reads the live ledger, a resetting claim reads the
    /// policy about to be minted rather than the doomed one, and an id nobody
    /// opened reads the minted policy too. A currency on an unpoliced policy
    /// is descriptive and must answer `None` on every one of them, or the door
    /// refuses a bind while enforcing nothing.
    #[tokio::test]
    async fn the_boarding_policy_currency_follows_the_ledger_the_claim_will_get() {
        let run = run(1_000, 400, None);
        let policed = mogwai_protocol::AccountId::parse("POLICED-01").unwrap();
        run.open_account(
            &policed,
            std::collections::HashMap::from([("USD".to_owned(), Decimal::from(50_000))]),
            mogwai_protocol::risk::AccountPolicy {
                currency: Some("USD".to_owned()),
                trailing_drawdown: Some(mogwai_protocol::risk::TrailingDrawdown {
                    amount: Decimal::from(5_000),
                    basis: mogwai_protocol::risk::TrailingBasis::default(),
                    lock_at_equity: None,
                    on_breach: mogwai_protocol::risk::BreachAction::Terminate,
                }),
                ..Default::default()
            },
        )
        .expect("a policed account in its own currency opens");
        assert_eq!(
            run.policy_currency(&policed, false).as_deref(),
            Some("USD"),
            "a resuming claim is enforced under the ledger it resumes"
        );
        assert_eq!(
            run.policy_currency(&policed, true),
            None,
            "a resetting claim is enforced under the policy it mints, which is unpoliced"
        );
        assert_eq!(
            run.policy_currency(
                &mogwai_protocol::AccountId::parse("NOBODY-01").unwrap(),
                false
            ),
            None,
            "an id nobody opened is enforced under the minted policy too"
        );

        let stated = mogwai_protocol::AccountId::parse("STATED-01").unwrap();
        run.open_account(
            &stated,
            std::collections::HashMap::from([("USD".to_owned(), Decimal::from(50_000))]),
            mogwai_protocol::risk::AccountPolicy {
                currency: Some("USD".to_owned()),
                ..Default::default()
            },
        )
        .expect("a currency with no rule is a valid unpoliced policy");
        assert_eq!(
            run.policy_currency(&stated, false),
            None,
            "a currency on an unpoliced policy must not turn boarding into enforcement"
        );
    }

    /// An arm against an account that has not connected must not cost that
    /// consumer its own account. The arm is recorded, not minted: the consumer's
    /// `POST /accounts` still succeeds, still gets the balances it asked for, and
    /// finds the arm standing on the ledger it opened.
    ///
    /// Minting at the control plane instead - which an earlier draft of this
    /// round did - answers that request `409 already open` and hands the account
    /// default balances and no policy, so the fix for a silent no-op became a
    /// refusal on a legitimate consumer path.
    ///
    /// Honest about what bites: the second assertion bites on a dropped record.
    /// The `open_account` call succeeding is a guard, not a proven-biting
    /// assertion - reproducing the regression it forbids means putting a mint
    /// back into `Run::arm`, and there is no perturbation of the shipped code
    /// that produces it.
    #[tokio::test]
    async fn a_named_arm_before_a_consumer_opens_its_account_does_not_lock_it_out() {
        let run = run(1_000, 400, None);
        run.arm(
            Some("SUB-01"),
            VenueArm::GoDark {
                armed_ns: 1_000,
                span_ns: 5_000,
            },
        )
        .await;

        let named = mogwai_protocol::AccountId::parse("SUB-01").unwrap();
        run.open_account(
            &named,
            std::collections::HashMap::from([("USD".to_owned(), Decimal::from(50_000))]),
            mogwai_protocol::risk::AccountPolicy::default(),
        )
        .expect("an arm against an account is not an account");
        let opened = run.peek_account(&named).expect("just opened");
        assert!(
            opened.dark.open_at(SimClock::identity(), 1_000),
            "the arm posted before the consumer existed must be standing on the ledger it opened"
        );
    }

    /// The same record reaches an account that arrives on a socket instead, and
    /// it reaches only that account: a named arm is not a venue-wide one.
    #[tokio::test]
    async fn a_named_arm_reaches_the_account_that_connects_later_and_no_other() {
        let run = run(1_000, 400, None);
        run.arm(
            Some("SUB-02"),
            VenueArm::GoDark {
                armed_ns: 1_000,
                span_ns: 5_000,
            },
        )
        .await;

        let stranger = run.account(&mogwai_protocol::AccountId::parse("SUB-03").unwrap());
        assert!(
            !stranger.dark.open_at(SimClock::identity(), 1_000),
            "an arm naming one account must not black out another"
        );
        let named = run.account(&mogwai_protocol::AccountId::parse("SUB-02").unwrap());
        assert!(
            named.dark.open_at(SimClock::identity(), 1_000),
            "an arm naming an account that had not connected must be standing when it does"
        );
    }

    /// A pending arm is retained, not guaranteed, and this is the limit that
    /// makes the difference matter. There is no control that retracts an arm,
    /// so a pending record is one-way as far as the operator is concerned - but
    /// `pending` is capacity-bounded and sheds from the oldest end, which means
    /// arms posted for entirely unrelated names can drop one. An operator
    /// reading only "there is no clear" would conclude the arm is certain to be
    /// standing when its account arrives, and it is not.
    ///
    /// The account under test is armed first so it is the oldest, and the cap's
    /// worth of other names is then posted; the shed takes the front on the
    /// entry that overflows.
    #[tokio::test]
    async fn a_pending_arm_is_shed_by_arms_posted_for_other_accounts() {
        let run = run(1_000, 400, None);
        run.arm(
            Some("SHED-000"),
            VenueArm::GoDark {
                armed_ns: 1_000,
                span_ns: 5_000,
            },
        )
        .await;
        for i in 1..=MAX_PENDING_ACCOUNT_ARMS {
            run.arm(
                Some(&format!("SHED-{i:03}")),
                VenueArm::GoDark {
                    armed_ns: 1_000,
                    span_ns: 5_000,
                },
            )
            .await;
        }

        let shed = run.account(&mogwai_protocol::AccountId::parse("SHED-000").unwrap());
        assert!(
            !shed.dark.open_at(SimClock::identity(), 1_000),
            "the oldest pending arm is shed at the cap, so the account it named opens clean"
        );
        // The distinguishing half: the cap sheds the oldest rather than
        // refusing the newest, so the arm that caused the overflow is standing.
        let kept = run.account(
            &mogwai_protocol::AccountId::parse(&format!("SHED-{MAX_PENDING_ACCOUNT_ARMS:03}"))
                .unwrap(),
        );
        assert!(
            kept.dark.open_at(SimClock::identity(), 1_000),
            "the arm that overflowed the cap is the one retained"
        );
    }

    /// A venue-wide arm of each storage class, with the engine queue filled to
    /// its cap so a replay onto a freshly minted ledger is observable through
    /// the shed report. Shared by the two mint-site tests so neither can be
    /// checking a different set of arms than the other.
    async fn fill_venue_record(run: &Run) {
        run.arm(
            None,
            VenueArm::GoDark {
                armed_ns: 1_000,
                span_ns: 5_000,
            },
        )
        .await;
        run.arm(None, VenueArm::DelayAcks { ms: 37 }).await;
        run.arm(
            None,
            VenueArm::FeeSurcharge {
                mult: Decimal::from(3),
                armed_ns: 1_000,
                span_ns: 5_000,
            },
        )
        .await;
        for _ in 0..mogwai_engine::MAX_ARMED_DIVERGENCES {
            let shed = run
                .arm(
                    None,
                    VenueArm::Engine(mogwai_protocol::control::Divergence::DuplicateNextFill),
                )
                .await;
            assert!(shed.is_none(), "the record fills before it sheds");
        }
    }

    /// Book one taker fill on `account` and report the commission it paid.
    ///
    /// The fee schedule is one currency unit per contract, so the commission
    /// reads back as the armed surcharge multiplier itself. Pricing a real fill
    /// is the only observation of an armed fee window that does not reach into
    /// the engine's private state, which is why every surcharge-routing test
    /// here goes through this rather than inspecting a field.
    async fn commission_on_one_fill(run: &Run, account: &Account, tag: &str) -> Decimal {
        run.ensure_instrument(account, "BTCUSDT")
            .await
            .expect("the ledger can trade the boot symbol");
        let mut engine = account.engine.lock().await;
        engine.set_fee_schedule(
            "BTCUSDT".into(),
            mogwai_engine::FeeSchedule {
                maker: mogwai_engine::FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
                taker: mogwai_engine::FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
            },
        );
        let clock = SimClock {
            sim_epoch_ns: 10_000,
            wall_anchor_ns: 2_000,
            speed: 1.0,
        };
        let events = engine.process_with_market_on_clock(
            mogwai_protocol::Command::SubmitOrder(mogwai_protocol::SubmitOrder {
                client_order_id: tag.into(),
                symbol: "BTCUSDT".into(),
                position_id: None,
                side: mogwai_protocol::Side::Buy,
                order_type: mogwai_protocol::OrderType::Market,
                quantity: Decimal::ONE,
                price: Some(Decimal::from(100)),
                trigger_price: None,
                trail_offset: None,
                limit_offset: None,
                reduce_only: false,
                post_only: false,
                time_in_force: mogwai_protocol::TimeInForce::Gtc,
                expire_time: None,
                link: None,
            }),
            clock.sim_epoch_ns,
            Some(mogwai_engine::MarketReading::flat(
                Decimal::from(100),
                clock.sim_epoch_ns,
                0,
            )),
            clock,
        );
        events
            .iter()
            .find_map(|event| match event {
                mogwai_protocol::VenueMessage::OrderFilled(fill) => Some(fill.commission),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the probe order must fill: {events:?}"))
    }

    /// The account-side half of named routing: an engine one-shot and a fee
    /// surcharge posted against one name reach that ledger and no other.
    ///
    /// `arm_divergence` passed `None` for both whatever the request named, so an
    /// operator perturbing one subagent perturbed the whole batch and the wire
    /// gave no way to say otherwise. The transport half is covered by
    /// `a_named_arm_reaches_the_account_that_connects_later_and_no_other`, and it
    /// cannot stand in for this one: transport arms are applied synchronously by
    /// `apply_transport_arm` while these two are applied by the engine-side match
    /// below it, so either line can be dropped alone.
    ///
    /// Both ledgers are minted after the arms, which puts the pending record on
    /// the path too - the named one must open holding what was posted for it and
    /// the stranger must open clean.
    ///
    /// The engine queue is read through the cap, the only reader
    /// `mogwai-engine` exposes: fill the named record to
    /// `MAX_ARMED_DIVERGENCES` with no ledger open, mint both, and arm once more
    /// against each. The named ledger replayed the record and sheds; the
    /// stranger's has room and sheds nothing.
    #[tokio::test]
    async fn a_named_account_side_arm_reaches_that_ledger_and_no_other() {
        let run = run(1_000, 400, None);
        run.arm(
            Some("SCOPED-01"),
            VenueArm::FeeSurcharge {
                mult: Decimal::from(3),
                armed_ns: 1_000,
                span_ns: 5_000,
            },
        )
        .await;
        for _ in 0..mogwai_engine::MAX_ARMED_DIVERGENCES {
            let shed = run
                .arm(
                    Some("SCOPED-01"),
                    VenueArm::Engine(mogwai_protocol::control::Divergence::DuplicateNextFill),
                )
                .await;
            assert!(shed.is_none(), "a pending record fills before it sheds");
        }

        let scoped = run.account(&mogwai_protocol::AccountId::parse("SCOPED-01").unwrap());
        let stranger = run.account(&mogwai_protocol::AccountId::parse("STRANGER-01").unwrap());

        assert!(
            run.arm(
                Some("SCOPED-01"),
                VenueArm::Engine(mogwai_protocol::control::Divergence::DuplicateNextFill),
            )
            .await
            .is_some(),
            "the named ledger must open holding the queue armed against its name"
        );
        assert!(
            run.arm(
                Some("STRANGER-01"),
                VenueArm::Engine(mogwai_protocol::control::Divergence::DuplicateNextFill),
            )
            .await
            .is_none(),
            "an engine arm naming one account must leave another account's queue empty"
        );

        assert_eq!(
            commission_on_one_fill(&run, &scoped, "SCOPED-FEE").await,
            Decimal::from(3),
            "a surcharge naming an account must be standing on the ledger it named"
        );
        assert_eq!(
            commission_on_one_fill(&run, &stranger, "STRANGER-FEE").await,
            Decimal::ONE,
            "and must not reach a ledger the request did not name"
        );
    }

    /// An absent callsign keeps the pre-callsign contract exactly: silence is not
    /// a claim to be the incumbent, so it always evicts and is always evicted.
    #[test]
    fn a_socket_naming_no_callsign_evicts_and_is_evicted() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("QUIET-001").unwrap();
        let (_first_attach, first) = admit(&run, &account, None, None);
        let (first_lanes, _first_rx) =
            crate::admission::ExecLanes::detached_as(first.connection_id);
        run.bind_lanes(first_lanes, account.as_str(), None);

        let (_quiet_attach, quiet) = admit(&run, &account, None, None);
        assert_eq!(
            run.close_displaced(account.as_str(), &quiet.displaced),
            1,
            "an unidentified newcomer takes the ledger, as it always did"
        );
        let (second_lanes, _second_rx) =
            crate::admission::ExecLanes::detached_as(quiet.connection_id);
        run.bind_lanes(second_lanes, account.as_str(), Some("worker-1"));

        let (_third_attach, third) = admit(&run, &account, None, None);
        assert_eq!(
            run.close_displaced(account.as_str(), &third.displaced),
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
        run.account(&abandoned);
        let held_account = run.account(&held);
        let (_held_attach, held_conn) = admit(&run, &held, None, None);
        let (lanes, _rx) = crate::admission::ExecLanes::detached_as(held_conn.connection_id);
        run.bind_lanes(lanes, held.as_str(), None);
        let _ = &held_account;

        // A TTL nothing has outlived yet spares even the unattended account,
        // which is what makes the collection about the SPAN rather than about
        // being unattended at all.
        assert!(
            run.collect_expired_accounts(std::time::Duration::from_secs(3_600))
                .is_empty(),
            "an account frozen a moment ago has not outlived an hour"
        );

        // A zero TTL collects everything already unattended, which is what makes
        // the rest of this about the attachment rather than about a sleep.
        let collected = run.collect_expired_accounts(std::time::Duration::ZERO);
        assert_eq!(
            collected,
            vec!["GONE-001".to_string()],
            "only the unattended account is collected: {collected:?}"
        );
        assert!(
            !run.accounts()
                .iter()
                .any(|account_state| account_state.account_id.as_str() == "GONE-001"),
            "and it is gone from the registry"
        );
        assert!(
            run.accounts()
                .iter()
                .any(|account_state| account_state.account_id.as_str() == "HELD-001"),
            "the attached account survives"
        );
    }

    /// A socket reclaiming a long-frozen ledger must not have it collected out
    /// from under it, which the freeze stamp alone cannot express.
    ///
    /// A returning `/ws` upgrade is counted onto its account before the 101 and
    /// only clears the freeze once its handler reaches `resume` - the 101, a
    /// task spawn and an instrument registration later. Collecting on the stamp
    /// alone therefore discarded the account inside that window, and the consumer
    /// that came back for its book was silently minted a fresh one. Both
    /// assertions are here because the second alone passes against the broken
    /// shape: what proves the fix is the account surviving while attached.
    #[test]
    fn an_attached_socket_spares_the_account_it_is_reclaiming() {
        let run = run(1_000, 400, None);
        let returning = mogwai_protocol::AccountId::parse("BACK-001").unwrap();
        let account_state = run.account(&returning);
        assert!(
            account_state.is_frozen(),
            "an account nobody has connected to is unattended, and this test is about that state"
        );

        let (attach, _committed) = admit(&run, &returning, None, None);
        assert!(
            run.collect_expired_accounts(std::time::Duration::ZERO)
                .is_empty(),
            "a socket is on its way in: the ledger it is reclaiming must still be there when it \
             arrives"
        );

        drop(attach);
        assert_eq!(
            run.collect_expired_accounts(std::time::Duration::ZERO),
            vec!["BACK-001".to_string()],
            "and once nothing is on it, the TTL collects it as it always did"
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
        // warmup generation - never from boot. A run whose warmup is larger than
        // its duration must still get its whole declared duration.
        let bounded = run(1_000_000, 999_000, Some(30));
        assert_eq!(bounded.deadline_ns, Some(1_000_030));

        let indefinite = run(1_000, 0, None);
        assert_eq!(indefinite.deadline_ns, None);
    }
}
