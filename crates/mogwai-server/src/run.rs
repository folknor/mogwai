// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! State owned by one venue process: one ledger, and keyed paced boats over
//! many rivers.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
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
    /// HOW MANY SOCKETS HAVE BEEN ADMITTED TO THIS ACCOUNT AND HAVE NOT YET
    /// FINISHED TEARING DOWN, counted from the instant the upgrade is decided
    /// rather than from the instant a lane is bound.
    ///
    /// The lane table alone cannot answer "is anybody reading this account",
    /// and the gap is not theoretical. An eviction retires the incumbent's lane
    /// EAGERLY, and the newcomer binds its own only once `handle_socket` runs -
    /// which is after the 101 and never at all if the client abandons the
    /// upgrade. Between those two instants the account has no lane, and a freeze
    /// decided on the lane table alone would either fire on a live account or,
    /// as it did, never fire at all and leave the ledger un-frozen, un-swept-out
    /// and un-collectable for the life of the process.
    ///
    /// Raised by `Run::admit` before the upgrade completes and lowered by
    /// `SocketSession`'s `Drop`, which runs on the abandoned-upgrade path too.
    /// So it counts ADMISSIONS, and every admission has exactly one departure.
    admitted: AtomicUsize,
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

    /// How many admitted sockets are still on this account. See `admitted`.
    fn admissions(&self) -> usize {
        self.admitted.load(Ordering::Acquire)
    }

    /// Count one socket in, before its upgrade completes.
    fn admit(&self) {
        self.admitted.fetch_add(1, Ordering::AcqRel);
    }

    /// Count one socket out. Called from `SocketSession`'s `Drop`, so it runs
    /// for an abandoned upgrade as well as for a socket that traded.
    fn depart(&self) {
        // SATURATING RATHER THAN WRAPPING, which is why this is a loop and not
        // a `fetch_sub`: an unbalanced departure would wrap the count to
        // `usize::MAX` and leave the account looking permanently attended,
        // which is the exact failure this counter exists to close.
        //
        // AND IT ASSERTS THE INVARIANT IT PROTECTS, because a saturating loop
        // alone silently absorbs the bug it fears: production keeps the safe
        // floor, and a debug build says which account went unbalanced. It is a
        // `debug_assert` deliberately: the workspace runs its tests in both
        // profiles, and a hard assert here would turn an unbalanced departure
        // into a release-mode abort of the serving path.
        let mut live = self.admitted.load(Ordering::Acquire);
        debug_assert!(
            live > 0,
            "an admission departed twice on account {account}: every admission has exactly one \
             departure",
            account = self.account_id.as_str()
        );
        while live > 0 {
            match self.admitted.compare_exchange_weak(
                live,
                live - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(seen) => live = seen,
            }
        }
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

    /// Clear every transport arm this account carries.
    pub(crate) fn clear_transport_havoc(&self) {
        self.dark.clear();
        self.stall.clear();
        self.delay_ms.store(0, Ordering::Relaxed);
        for knob in self.latency_knobs() {
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
/// EVERY FIELD IS OPTIONAL AND AN UNSET ONE IS NOT A ZERO. A record is replayed
/// onto a ledger that may already carry another record's effects - the venue-wide
/// record then the account's own - so "this run said nothing about ack delays"
/// has to be distinguishable from "this run armed a zero ack delay", or the
/// second replay silently disarms the first.
///
/// The arm variants are STORE-not-merge on the wire, and this mirrors that
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
    /// Engine-armed divergences in arming order, capped and shed from the OLDEST
    /// end exactly as `Engine::arm` does, so replaying this onto a fresh ledger
    /// reproduces the queue a seated one holds.
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

    /// Lift what `ClearDivergences` lifts. The engine queue is deliberately left
    /// standing; see `VenueArms`.
    fn clear_transport_and_fee(&mut self) {
        self.dark = None;
        self.stall = None;
        self.delay_ms = None;
        self.latency = None;
        self.fee_surcharge = None;
    }

    /// Replay the transport-side record onto a passenger being opened.
    fn open_transport(&self, passenger: &Passenger) {
        if let Some(span) = self.dark {
            passenger.dark.arm(span.wall_armed_ns, span.sim_span_ns);
        }
        if let Some(span) = self.stall {
            passenger.stall.arm(span.wall_armed_ns, span.sim_span_ns);
        }
        if let Some(ms) = self.delay_ms {
            passenger.delay_ms.store(ms, Ordering::Relaxed);
        }
        if let Some(latency) = self.latency {
            for (knob, ms) in passenger.latency_knobs().into_iter().zip(latency) {
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

/// Apply the transport half of ONE arm to a passenger that already exists.
///
/// It reads the ARM'S OWN FIELDS and nothing else. An earlier draft read
/// `CommandLatency` out of the record it had just written, which was correct
/// only because the caller recorded first - exactly the ordering coupling a
/// later edit breaks without a test going red.
fn apply_transport_arm(arm: &VenueArm, passenger: &Passenger) {
    match arm {
        VenueArm::DelayAcks { ms } => passenger.delay_ms.store(*ms, Ordering::Relaxed),
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
            for (knob, ms) in passenger.latency_knobs().into_iter().zip(armed) {
                knob.store(ms, Ordering::Relaxed);
            }
        }
        VenueArm::GoDark { armed_ns, span_ns } => passenger.dark.arm(*armed_ns, *span_ns),
        VenueArm::StallData { armed_ns, span_ns } => passenger.stall.arm(*armed_ns, *span_ns),
        VenueArm::FeeSurcharge { .. } | VenueArm::Engine(_) => {}
    }
}

/// AN ARM DOES NOT WAIT FOR A CONNECTION. Whatever the control plane has posted
/// that a ledger minted later still owes.
///
/// THE ARMS BELONG TO THE RUN, NOT TO THE SEATED SET. The control plane is an
/// operator surface, and an unqualified arm is a statement about the venue -
/// `docs/havoc.md` says so twice, once for the transport windows ("naming none
/// arms every account") and once for the late boarder ("a passenger that boards
/// after the arm receives the full declared span from its own boarding
/// instant"). The arming code walked the passengers that existed at the instant
/// of the request, so an operator who armed a `PartialFillNext` and then started
/// a subagent got a run that believed it was perturbed and was not - and the
/// eviction report's "every ledger holds the same arms and hits the cap
/// together" was false the moment one ledger was minted after an arm.
///
/// THE TWO HALVES DIFFER IN LIFETIME, AND DELIBERATELY. `all` is the venue's
/// standing state and is replayed onto EVERY ledger this run ever mints.
/// `pending` holds an arm posted against a NAMED account that does not exist
/// yet, and is consumed by that account's first mint - which is precisely the
/// promise a named arm makes ("the arm is standing when the client dials") and
/// nothing more. Recording rather than minting is what keeps the control plane
/// from deciding an account's terms: the client's own `POST /account` still
/// opens the ledger, with its own balances and policy, and finds the arm on it.
/// A previous draft minted the ledger here, which locked that client out with a
/// `409` and handed it default balances.
///
/// WHAT A CLEAR TOUCHES AND WHAT IT DOES NOT is mirrored here, because a ledger
/// minted now must be indistinguishable from one minted at boot that received
/// every control request since - see `Run::arm` for the exact scope of that
/// claim. `ClearDivergences` lifts the transport windows and the fee surcharge
/// off every seated ledger and does NOT drain the engine's armed queue, so
/// `all` does the same. `pending` is dropped WHOLE by a clear, engine entries
/// included: those arms were never applied to anything, so there is no seated
/// ledger for them to stay consistent with.
#[derive(Default)]
struct VenueArms {
    all: ArmRecord,
    /// `(account id, record)` in first-armed order, for accounts that had not
    /// been minted when the arm arrived. A `Vec` rather than a map because it
    /// is shed from the OLDEST end at `MAX_PENDING_ACCOUNT_ARMS` - an operator
    /// surface that allocates per distinct name needs a bound, and this is the
    /// same shape and direction as the engine's own armed-queue cap.
    pending: Vec<(String, ArmRecord)>,
}

/// How many not-yet-minted accounts the run will carry arms for. Operator scale
/// is a handful of subagents; the cap exists so a typo loop on
/// `POST /control/divergence` cannot grow the run without bound.
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

    /// Take `account_id`'s pending arms, if any. The caller replays `all` FIRST
    /// and then this, because a pending arm is by construction later in time
    /// than every standing one it can overlap.
    ///
    /// TAKING RATHER THAN READING is what bounds `pending`, and it is the honest
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

/// One socket's claim to be reading an account, from before the 101 until the
/// socket is finished with it.
///
/// A GUARD OBJECT rather than a pair of calls, because the failure it closes is
/// the one where the release never runs: an upgrade abandoned after the 101
/// never reaches `handle_socket`, and an account left counted-in forever is
/// never frozen, never TTL-collected and swept while holding no seat.
///
/// MINTED ONLY BY `Run::admit`, which raises the count and constructs this in
/// one expression. `Run::depart` is private to this module, so there is no way
/// to raise the count without owning the guard that lowers it, and no way to
/// lower it twice.
pub(crate) struct Admission {
    run: Arc<Run>,
    passenger: Arc<Passenger>,
}

/// The account only: the run behind it holds every ledger on the venue, which
/// is not something a diagnostic should splice into a log line.
impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admission")
            .field("account_id", &self.passenger.account_id.as_str())
            .finish_non_exhaustive()
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.run.depart(&self.passenger);
    }
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
    /// Every venue-wide arm the control plane has posted. Read whenever a
    /// ledger is minted, so a late-connecting account carries what the operator
    /// armed. LOCK ORDER: `passengers` FIRST, then this - both `passenger` and
    /// `arm_venue_wide` take them that way, which is what makes an arm racing a
    /// mint land in exactly one of the two paths rather than in neither.
    venue_arms: Mutex<VenueArms>,
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
    /// One receiver per LIVE WEBSOCKET SESSION, held from BEFORE THE 101 until
    /// after that session's writer has flushed its close, so the process can
    /// tell whether anything is still being served.
    ///
    /// THE ACCEPT LOOP IS NOT THE ANSWER TO THAT QUESTION, which is the whole
    /// reason this exists. `axum::serve`'s graceful shutdown tracks HYPER
    /// CONNECTIONS, and an upgraded connection's hyper future resolves at the
    /// 101 rather than when the websocket ends - `serve/mod.rs` drops its
    /// `close_rx` the moment `serve_connection_with_upgrades` returns. So a
    /// venue that waited only on axum was racing its own sessions: the run's
    /// completion was published on a watch channel, the accept loop stopped, the
    /// serve future resolved, `main` returned, and the runtime dropped every
    /// session task mid-flight - taking the `RunComplete` frame and the WS 1000
    /// close with it. The peer then saw a reset rather than a completed run,
    /// intermittently and only on a loaded host, which is exactly the shape the
    /// two lifecycle leads reported.
    ///
    /// Nothing else may take a receiver. The count is the number of live
    /// sessions, and a holder that is not one makes `sessions_drained` a wait on
    /// something other than what it names.
    sessions_tx: watch::Sender<()>,
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
        let (sessions_tx, _) = watch::channel(());
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
            venue_arms: Mutex::new(VenueArms::default()),
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
            sessions_tx,
            lanes: Mutex::new(Vec::new()),
            order_owners: Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// An engine built from the venue template, for `account_id`, holding
    /// `balances`.
    ///
    /// THE ONE PLACE THE TEMPLATE IS APPLIED. Three callers build an engine -
    /// the mint on first sight, the client's own `open_account`, and the
    /// throwaway preview `unopened_ledger` - and each carried its own copy of
    /// `Engine::build` plus the two `set_*` calls. Those copies are the
    /// two-implementations-without-a-gate shape: the next setting added to the
    /// template would be owed at three sites, nothing would notice two of them,
    /// and the preview would then answer for a ledger the venue would never
    /// actually open. Lifecycle still differs per caller - only this
    /// construction is shared.
    ///
    /// `balances` is a parameter rather than read from the template because
    /// `open_account` is the client stating its own; the other two pass the
    /// template's.
    fn template_engine(
        &self,
        account_id: &mogwai_protocol::AccountId,
        balances: std::collections::HashMap<String, Decimal>,
    ) -> Engine {
        // The ledger starts EMPTY of instruments. One becomes tradable when a
        // passenger binds a symbol or names it on an order, through
        // `ensure_instrument` - which is per passenger for the same reason the
        // engine is.
        let mut engine = Engine::build(EngineConfig {
            account_id: account_id.clone(),
            instruments: Vec::new(),
            balances,
            fill_seed: self.template.fill_seed,
        });
        engine.set_oms_type(self.template.oms_type);
        engine.set_liquidation_band_ticks(self.template.fill_band_max_ticks);
        engine
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
        let mut engine = self.template_engine(account_id, self.template.opening_balances.clone());
        // Opened with whatever the operator has armed - venue-wide, and against
        // this account by name before it existed - so this ledger is
        // indistinguishable from one that was seated when the arm arrived. Held
        // across the passenger insert below, under the `passengers` lock this
        // call already holds, so an arm cannot interleave between the replay and
        // the moment `Run::arm` can see this passenger.
        let mut arms = self
            .venue_arms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = arms.take_pending(account_id.as_str());
        arms.all.open_engine(&mut engine);
        if let Some(pending) = &pending {
            pending.open_engine(&mut engine);
        }
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
            admitted: AtomicUsize::new(0),
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
        arms.all.open_transport(&seated);
        if let Some(pending) = &pending {
            pending.open_transport(&seated);
        }
        drop(arms);
        passengers.insert(account_id.as_str().to_owned(), Arc::clone(&seated));
        seated
    }

    /// The ledger an account nobody has opened WOULD open with, built here and
    /// retained nowhere.
    ///
    /// THIS IS NOT A THIRD MINT SITE, and the difference is exactly that
    /// nothing survives the call: no passenger is inserted, no pending named
    /// arm is consumed, no seat and no freeze clock start. The CONSTRUCTION is
    /// nevertheless shared with both real mint sites, through
    /// `template_engine`, because "not a mint" is a claim about lifecycle and
    /// says nothing about whether a preview is built the same way as the thing
    /// it previews. It exists so a READ
    /// can answer for an unknown account without allocating one -
    /// `GET /account?account=<anything>` is unauthenticated, and creating a
    /// ledger per id anybody names makes an endpoint that changes nothing into
    /// an unbounded allocator, bounded only when `account_ttl_ms > 0` and the
    /// default is to keep accounts forever.
    ///
    /// THE ARMS ARE DELIBERATELY NOT REPLAYED, which is what keeps this honest
    /// rather than a fourth thing to keep in sync. Replaying the venue record
    /// would CONSUME this account's pending arm, so a read would disarm what an
    /// operator armed against an account that has not connected. Nothing the
    /// arms touch - a fee surcharge, an armed engine divergence, a transport
    /// window - is rendered in an `AccountState` or a `RiskState`, so the
    /// answer is identical either way. THAT LAST SENTENCE IS AN ASSUMPTION
    /// ABOUT THE SNAPSHOT SHAPE, stated here rather than left implicit: it
    /// holds for every field those two types carry today, and a field added to
    /// either that DOES render an armed effect makes a preview and a real
    /// ledger differ. Nothing detects that; this comment is the notice.
    pub(crate) fn unopened_ledger(
        &self,
        account_id: &mogwai_protocol::AccountId,
    ) -> (Engine, crate::risk::RiskLedger) {
        let engine = self.template_engine(account_id, self.template.opening_balances.clone());
        let ledger = crate::risk::RiskLedger::new(
            mogwai_protocol::risk::AccountPolicy::default(),
            opening_equity(&self.template.opening_balances),
            self.started_ns,
        );
        (engine, ledger)
    }

    /// Apply one control-plane arm, to the ledgers that exist AND to the ones
    /// that do not yet. Returns the divergence an engine queue shed to make
    /// room, if any.
    ///
    /// `account` is the request's optional `account` field. `None` is not "every
    /// ledger that happens to exist", it is THE VENUE: the arm is recorded on
    /// the run and replayed onto every ledger minted from here on. `Some(name)`
    /// reaches exactly that account whether or not it has connected - live if it
    /// has, from the pending record if it has not.
    ///
    /// THE RECORD AND THE LIVE APPLICATION ARE ONE CALL, deliberately. Written
    /// as two - store it here, walk the passengers there - they drift, and the
    /// drift is invisible: the seated set behaves and the next account to
    /// connect does not. `VenueArms` says why this is owed at all.
    ///
    /// The record is taken while HOLDING THE PASSENGER MAP, which is what
    /// resolves the race with a concurrent mint. `Run::passenger` and
    /// `Run::open_account` read the record under the same map lock, so a
    /// passenger is either already in the list this walks or was opened from a
    /// record that includes this arm - never neither.
    ///
    /// WHAT "INDISTINGUISHABLE FROM A SEATED LEDGER" DOES NOT COVER: the engine
    /// half is applied after both locks drop, because the engine sits behind an
    /// async mutex. Two engine arms posted CONCURRENTLY can therefore land on
    /// two seated ledgers in opposite orders, and a ledger minted between them
    /// holds the record's order. The control plane is operator-driven and
    /// serialized in every scenario the venue is used from, so this is stated
    /// rather than closed - but it is a real limit on the claim, not a rounding
    /// error in it.
    pub(crate) async fn arm(
        &self,
        account: Option<&str>,
        arm: VenueArm,
    ) -> Option<mogwai_protocol::control::Divergence> {
        let seated: Vec<Arc<Passenger>> = {
            let passengers = self
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut arms = self
                .venue_arms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match account {
                None => {
                    arms.all.record(&arm);
                    for passenger in passengers.values() {
                        apply_transport_arm(&arm, passenger);
                    }
                    passengers.values().map(Arc::clone).collect()
                }
                Some(named) => match passengers.get(named) {
                    Some(passenger) => {
                        apply_transport_arm(&arm, passenger);
                        vec![Arc::clone(passenger)]
                    }
                    // NOT MINTED HERE. Recording leaves the client's own
                    // `POST /account` free to open the ledger on its own terms
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
                for passenger in seated {
                    passenger
                        .engine
                        .lock()
                        .await
                        .arm_fee_surcharge(*mult, *armed_ns, *span_ns);
                }
            }
            VenueArm::Engine(div) => {
                for passenger in seated {
                    let evicted = passenger.engine.lock().await.arm(div.clone());
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

    /// `ClearDivergences`: lift the transport windows and the fee surcharge off
    /// every seated ledger AND off the venue record, so an account connecting
    /// after a clear is not re-armed from a stale record.
    ///
    /// The engine's armed queue is untouched on the venue record and on every
    /// seated ledger. That is not an oversight - a clear has never drained it -
    /// and mirroring the omission is what keeps a minted ledger identical to a
    /// seated one. The PENDING per-account records are dropped whole, engine
    /// entries included: nothing was ever applied from them, so there is no
    /// seated ledger for them to stay consistent with.
    pub(crate) async fn clear_venue_arms(&self) {
        let seated: Vec<Arc<Passenger>> = {
            let passengers = self
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut arms = self
                .venue_arms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            arms.all.clear_transport_and_fee();
            arms.pending.clear();
            passengers.values().map(Arc::clone).collect()
        };
        for passenger in seated {
            // EVERY account's transport arms, whatever the request named: a
            // clear is an operator saying "stop everything".
            passenger.clear_transport_havoc();
            passenger.engine.lock().await.clear_fee_surcharge();
        }
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
    ///
    /// `resetting` IS PASSED IN RATHER THAN RE-DERIVED, from
    /// `seat_discards_ledger`. `/ws` has to know the answer BEFORE it seats -
    /// the funding refusal is a statement about the ledger the connection will
    /// actually get - and computing it a second time here would evaluate the
    /// same predicate against a LATER state of the venue: `has_client_on` reads
    /// the lane table, and this client's other lane can drop in the window
    /// between the two reads. The two values would then disagree about whether
    /// the ledger `/ws` funding-checked and cadence-checked is the ledger this
    /// call returns. One evaluation, one decision, one reader.
    pub(crate) fn seat(
        &self,
        account_id: &mogwai_protocol::AccountId,
        claimed: bool,
        session: Option<&str>,
        resetting: bool,
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
        if resetting {
            self.reopen(account_id);
        }
        self.passenger(account_id)
    }

    /// Whether seating this connection will DISCARD the account's ledger.
    ///
    /// THE ONE HOME OF THE RESET RULE, and asked ONCE per upgrade: `/ws` asks
    /// it before it refuses anything - the funding refusal is a statement about
    /// the ledger the connection will actually get, and under the reset knob
    /// that is a fresh one built from the venue template rather than whatever
    /// the account holds now - and then hands the answer to `seat`. It must be
    /// asked before any eviction, because `has_client_on` reads the lane table
    /// and an eviction prunes exactly the lanes that answer "is the claiming
    /// client already here".
    pub(crate) fn seat_discards_ledger(
        &self,
        account_id: &mogwai_protocol::AccountId,
        claimed: bool,
        session: Option<&str>,
    ) -> bool {
        claimed
            && self.reset_account_on_reconnect
            && !self.has_client_on(account_id.as_str(), session)
    }

    /// The account's ledger IF IT ALREADY EXISTS. Unlike `passenger`, this
    /// never mints one, which is what lets the funding check ask about an
    /// account nobody has opened without opening it.
    ///
    /// THIS DOES NOT MAKE A REFUSED UPGRADE ALLOCATION-FREE, and saying so
    /// would be the over-claim this comment used to make. `/ws` calls
    /// `Run::passenger` on the non-resetting path - to take the seat before the
    /// eviction - and the cadence refusal comes after it, so a refused upgrade
    /// can still leave a fresh ledger behind. That is now the ONLY such site:
    /// `GET /account` used to mint the same way on an unauthenticated read and
    /// resolves through this method instead, previewing an unopened ledger with
    /// `unopened_ledger` when there is nothing to find. Whether a REFUSAL may
    /// allocate an account remains open; what this method promises is only that
    /// IT does not mint.
    pub(crate) fn peek_passenger(
        &self,
        account_id: &mogwai_protocol::AccountId,
    ) -> Option<Arc<Passenger>> {
        self.passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account_id.as_str())
            .map(Arc::clone)
    }

    /// Whether the ledger this connection WILL be served on holds a balance
    /// line in `currency`.
    ///
    /// PRESENCE, never sufficiency - see `Engine::is_funded_in`. Asked of the
    /// prospective ledger rather than of the current one, because a seat that
    /// resets replaces the account's balances with the venue template's, and an
    /// account opened through `/account` with client-named balances can differ
    /// from that template in exactly which currencies it carries.
    pub(crate) async fn funded_in(
        &self,
        account_id: &mogwai_protocol::AccountId,
        resetting: bool,
        currency: &str,
    ) -> bool {
        if resetting {
            return self.template.opening_balances.contains_key(currency);
        }
        match self.peek_passenger(account_id) {
            Some(passenger) => passenger.engine.lock().await.is_funded_in(currency),
            // An account nobody has opened yet is minted from the template, so
            // the template is the honest answer and no ledger is created to
            // give it.
            None => self.template.opening_balances.contains_key(currency),
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
    /// UNATTENDED IS THE SAME TWO-PART QUESTION `freeze_if_unattended` ASKS,
    /// and it is asked here for the same reason: the freeze stamp alone is a
    /// statement about the past, and a socket that has been ADMITTED onto a
    /// long-frozen account has not cleared it yet.
    ///
    /// Returns the ids collected, so the caller can say which.
    pub(crate) fn collect_expired_accounts(&self, ttl: std::time::Duration) -> Vec<String> {
        let expired: Vec<String> = self
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(_, passenger)| {
                // ADMISSION IS THE SECOND HALF OF "NOBODY IS READING THIS", and
                // asking only the freeze clock left the two predicates able to
                // disagree. `freeze_if_unattended` refuses to freeze an account
                // an admitted socket is still on; this asked a STALE freeze
                // stamp, which a returning socket does not clear until its
                // handler reaches `resume` - so a client reclaiming a
                // long-frozen ledger could have it collected out from under it
                // between the admission and the attach, and would silently be
                // handed a fresh one instead of the book it came back for. The
                // window is the whole upgrade, which is the 101, a task spawn
                // and an instrument registration wide.
                passenger.admissions() == 0
                    && passenger
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
    ///
    /// THIS IS THE SECOND MINT SITE AND IT OWES THE VENUE ARMS TOO. The client
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
        let mut passengers = self
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if passengers.contains_key(account_id.as_str()) {
            return Err(AccountRefusal::AlreadyOpen);
        }
        let opening = opening_equity(&balances);
        let mut engine = self.template_engine(account_id, balances);
        // Under the `passengers` lock this call already holds, in the same lock
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
        let opened = Arc::new(Passenger {
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
            admitted: AtomicUsize::new(0),
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
        arms.all.open_transport(&opened);
        if let Some(pending) = &pending {
            pending.open_transport(&opened);
        }
        drop(arms);
        passengers.insert(account_id.as_str().to_owned(), opened);
        Ok(())
    }

    /// The passenger whose book holds `client_order_id` as a RESTING order, and
    /// the symbol it rests on.
    ///
    /// `account` IS THE REQUEST'S OWN `account` FIELD, and naming it is how a
    /// caller resolves the ambiguity rather than losing to it. Client order ids
    /// are CLIENT-CHOSEN, so they are unique within one trader's book and not
    /// across a venue serving fifty of them: two subagents that both number
    /// their orders from one collide, and an unqualified search returns
    /// whichever passenger the map iterated first. That is a scenario control
    /// cancelling a stranger's resting order - silently, since a silent cancel
    /// emits no lifecycle event by design, so the victim learns of it only by
    /// querying. With an account named, exactly one book is searched and a miss
    /// is a miss.
    ///
    /// `None` still searches every book, because that is what a control request
    /// naming no account means everywhere else on this plane - the venue - and
    /// on the single-account venue an operator usually drives, it is the only
    /// book there is.
    pub(crate) async fn passenger_holding(
        &self,
        account: Option<&str>,
        client_order_id: &str,
    ) -> Option<(Arc<Passenger>, Symbol)> {
        let candidates = match account {
            Some(named) => self
                .passengers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(named)
                .map(Arc::clone)
                .into_iter()
                .collect(),
            None => self.passengers(),
        };
        for passenger in candidates {
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
        self.register_instrument(passenger, &profile).await;
        Ok(profile)
    }

    /// The second half of `ensure_instrument`: install an ALREADY-RESOLVED
    /// shape on one ledger.
    ///
    /// Split out because the two halves refuse differently and `/ws` needs them
    /// at different moments. Resolution is the only fallible part and is a
    /// property of the VENUE, so it is decided before a connection claims an
    /// account and can therefore refuse without having evicted anybody;
    /// registration is a property of the LEDGER, cannot fail, and has to happen
    /// on whichever passenger the seat actually produced - which under the reset
    /// knob is not the one that existed when the shape was resolved.
    pub(crate) async fn register_instrument(
        &self,
        passenger: &Passenger,
        profile: &Arc<crate::source::InstrumentProfile>,
    ) {
        let mut engine = passenger.engine.lock().await;
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
            account
        };
        // A lane this call did not find was retired by an EVICTION, and the
        // freeze that owes is the departing socket's rather than this one's -
        // see `freeze_if_unattended` and `SocketSession`'s `Drop`. Retiring a
        // lane is therefore an opportunity to freeze, never the only one.
        let Some(account) = account else {
            return;
        };
        let passenger = self
            .passengers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account.as_str())
            .map(Arc::clone);
        if let Some(passenger) = passenger {
            self.freeze_if_unattended(&passenger);
        }
    }

    /// Count one socket in, from the instant `/ws` decides to admit it, and
    /// hand back the GUARD that counts it out again.
    ///
    /// THE COUNT IS ONLY REACHABLE THROUGH THE GUARD, which is the whole reason
    /// this returns one rather than pairing with a `depart` call. A raise and a
    /// release written as two statements leave a window - a panic, or an
    /// `async fn` cancelled by a client disconnect, between the two - in which
    /// the count is raised and nothing will ever lower it, and an account
    /// permanently counted-in is exactly the never-frozen, never-collected,
    /// still-swept ledger this counter exists to close. Returning the guard
    /// makes that window unrepresentable: there is no way to raise the count
    /// without simultaneously owning the thing that lowers it.
    ///
    /// Taken under the LANE LOCK, which is what serializes an admission against
    /// a departing socket's freeze decision: the two questions
    /// `freeze_if_unattended` asks are answered against one state of the venue
    /// rather than two.
    pub(crate) fn admit(self: &Arc<Self>, passenger: &Arc<Passenger>) -> Admission {
        let lanes = self.locked_lanes();
        passenger.admit();
        drop(lanes);
        Admission {
            run: Arc::clone(self),
            passenger: Arc::clone(passenger),
        }
    }

    /// Freeze `passenger` if nothing is reading it any more.
    ///
    /// TWO CONDITIONS, AND BOTH ARE LOAD-BEARING. No lane is bound to the
    /// account, AND no admitted socket is still on its way to binding one. The
    /// lane table alone says "attached" one instant too long for an evicted
    /// socket and one instant too short for an admitted one, so a freeze
    /// decided on it alone either fires on a live account or - the defect this
    /// closes - never fires at all: an eviction retires the incumbent's lane
    /// eagerly, so the incumbent's own teardown found no lane, resolved no
    /// account and returned without freezing, and a newcomer that never bound
    /// left the account attached with zero connections. That account is then
    /// never TTL-collected (`collect_expired_accounts` filters on
    /// `frozen_for()`) and is still swept while holding no seat, which cancels
    /// its resting orders - the exact opposite of the contract that a frozen
    /// account's book survives for the socket that returns.
    ///
    /// Idempotent: `freeze` is a no-op on an account already frozen, so every
    /// path that could be the last one may call this.
    fn freeze_if_unattended(&self, passenger: &Passenger) {
        let lanes = self.locked_lanes();
        let account = passenger.account_id.as_str();
        if passenger.admissions() > 0 || lanes.iter().any(|bound| bound.account_id == account) {
            return;
        }
        passenger.freeze();
        drop(lanes);
    }

    /// One socket has finished with `passenger`: give up its admission and
    /// freeze the account if it was the last thing reading it.
    ///
    /// Private, and reached only by dropping an `Admission`: see `admit`.
    fn depart(&self, passenger: &Passenger) {
        passenger.depart();
        self.freeze_if_unattended(passenger);
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

    /// A live-session token. Taken by `ws::ws_upgrade` BEFORE the 101 is
    /// returned, carried on the `SocketSession`, and dropped only after that
    /// session's writer has flushed - so the token's lifetime strictly contains
    /// the session's, and strictly contains the hyper connection's, which ends
    /// at the 101. Taking it inside the spawned handler instead would leave a
    /// window in which the connection is upgraded and this count has not yet
    /// risen. See `sessions_tx`.
    pub(crate) fn session_guard(&self) -> watch::Receiver<()> {
        self.sessions_tx.subscribe()
    }

    /// Resolves once no websocket session is live. Immediately, when none is.
    ///
    /// Awaited on the PLANNED-COMPLETION shutdown only, and that limit is
    /// deliberate rather than an oversight. A planned completion is a promise
    /// the venue made about its declared duration - every open socket is told,
    /// and each session ends itself the moment it hears - so waiting here is
    /// bounded by the sessions' own teardown and is the difference between an
    /// announced run and a reset one. A SIGNAL is not that promise: the
    /// launcher ended the run, `RunComplete` is deliberately NOT published, and
    /// nothing tells a session to stop - so waiting would idle out the whole
    /// shutdown grace on any venue with a socket attached and turn a clean stop
    /// into a bailed one. The signal path therefore keeps its abrupt teardown,
    /// which is what `sigterm_closes_without_announcing_run_complete` observes.
    pub(crate) async fn sessions_drained(&self) {
        self.sessions_tx.closed().await;
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

/// The account a frame is ABOUT when it says so ITSELF, rather than through an
/// order the run has to look up. `None` means the frame carries no account
/// identity and attribution falls to [`addressed_order`].
///
/// `AccountState` is the whole of it today, and it is not a curiosity: the sweep
/// runs one engine pass PER PASSENGER, so an N-account venue produces N account
/// snapshots per pass, each true of exactly one ledger. Attributing them by
/// order ownership - which is what delivery did before this existed - finds no
/// order, reads them as venue-wide, and hands every passenger every other
/// passenger's balances and positions. A consumer has no way to tell: the
/// snapshot names its account, but a client that was promised one ledger per run
/// has no reason to check, and the known nautilus adapter deliberately does not.
/// Sizing off a sibling's equity is the consequence, and it moves capital.
///
/// Attribution here is by the frame's OWN field rather than by which passenger
/// the sweep was iterating, deliberately: delivery is a pure function of the
/// batch, and a frame that cannot say who it belongs to must not be attributed
/// by ambient context that a later refactor can silently change.
pub(crate) fn addressed_account(
    event: &mogwai_protocol::ServerMessage,
) -> Option<&mogwai_protocol::AccountId> {
    match event {
        mogwai_protocol::ServerMessage::AccountState(state) => Some(&state.account_id),
        _ => None,
    }
}

/// The order a frame is ABOUT, for account attribution. `None` means the
/// frame is not order-scoped - a venue fault, a completion - and belongs to
/// every connection unless [`addressed_account`] claims it for one.
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

    /// The planned-completion shutdown waits on a LIVE SESSION and not merely
    /// on the accept loop.
    ///
    /// THE DIFFERENTIAL IS THE POINT, and both halves are asserted because the
    /// second alone passes against the broken shape too. `sessions_drained`
    /// must NOT resolve while a guard is held - that is the whole defect: the
    /// venue's serve future can complete while a websocket session is still
    /// mid-announcement, because axum stops tracking an upgraded connection at
    /// the 101 - and it must resolve promptly once the last guard drops, or a
    /// clean completion would idle out the shutdown grace and bail.
    ///
    /// A WALL TIMEOUT IS THE OBSERVABLE HERE RATHER THAN A BET, because the
    /// negative half of a "does not resolve" claim has no positive event to
    /// wait for. 200 ms against an operation that is a single atomic wake is
    /// three orders of magnitude of margin, and losing it means the guard was
    /// ignored outright rather than that the host was slow.
    #[tokio::test]
    async fn a_live_session_guard_holds_the_shutdown_open() {
        let run = run(1_000, 400, None);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                run.sessions_drained()
            )
            .await
            .is_ok(),
            "with no session live the venue owes nobody an announcement and must not wait"
        );

        let session = run.session_guard();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                run.sessions_drained()
            )
            .await
            .is_err(),
            "a live session still owes its RunComplete and its close; exiting here is what \
             reset the peer instead of announcing to it"
        );

        drop(session);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                run.sessions_drained()
            )
            .await
            .is_ok(),
            "the last session ended, so the wait must release rather than run out the grace"
        );
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

    /// AN EVICTED INCUMBENT STILL OWES THE FREEZE. Eviction retires the
    /// incumbent's lane EAGERLY, so the incumbent's own teardown finds no lane
    /// to match its id - and a freeze that keyed off finding one simply
    /// returned. The account was then left attached with zero connections:
    /// never TTL-collected, because `collect_expired_accounts` filters on
    /// `frozen_for()`, and still swept while holding no seat, which cancels its
    /// resting orders.
    #[test]
    fn an_evicted_account_freezes_when_the_incumbent_finishes_tearing_down() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("FREEZE-002").unwrap();
        let passenger = run.passenger(&account);
        let incumbent_admission = run.admit(&passenger);
        let (lanes, _rx) = crate::admission::ExecLanes::detached();
        let incumbent = run.bind_lanes(lanes, account.as_str(), Some("alpha"));
        passenger.attach();

        // A stranger claims the account and then never binds anything - the
        // refused upgrade, and the upgrade abandoned after the 101 - so the
        // incumbent's own teardown is the last thing that touches the account.
        assert_eq!(run.evict_account(account.as_str(), Some("beta")), 1);
        run.release_lanes(incumbent);
        drop(incumbent_admission);

        assert!(
            passenger.is_frozen(),
            "nothing is reading this account, so it must be frozen: collectable, and out of the \
             sweep"
        );
        assert!(
            passenger.frozen_for().is_some(),
            "and the TTL reaper reads exactly that"
        );
    }

    /// The other half of the same rule: an account is not frozen out from under
    /// a socket that has been ADMITTED and has not bound its lane yet. That
    /// window is real - it spans the 101 and everything `handle_socket` does
    /// before `bind_lanes` - and freezing inside it would leave a live
    /// connection trading a ledger the sweeper skips.
    #[test]
    fn an_admitted_socket_holds_the_account_open_before_it_binds_a_lane() {
        let run = run(1_000, 400, None);
        let account = mogwai_protocol::AccountId::parse("FREEZE-003").unwrap();
        let passenger = run.passenger(&account);
        let incumbent_admission = run.admit(&passenger);
        let (lanes, _rx) = crate::admission::ExecLanes::detached();
        let incumbent = run.bind_lanes(lanes, account.as_str(), Some("alpha"));
        passenger.attach();

        // The newcomer is counted on before the upgrade completes, exactly as
        // `/ws` does it.
        let newcomer_admission = run.admit(&passenger);
        assert_eq!(run.evict_account(account.as_str(), Some("beta")), 1);
        run.release_lanes(incumbent);
        drop(incumbent_admission);
        assert!(
            !passenger.is_frozen(),
            "the newcomer is on its way in, so the account is still being read"
        );

        drop(newcomer_admission);
        assert!(
            passenger.is_frozen(),
            "and when that newcomer goes without ever binding, the account freezes"
        );
    }

    /// Seat a claimed account the way `/ws` does: derive `resetting` ONCE,
    /// before anything is evicted, and hand it to `seat`. A test that spelled
    /// the predicate out for itself would be pinning the rule against its own
    /// copy of it rather than against the venue's.
    fn seat(
        run: &Arc<Run>,
        account_id: &mogwai_protocol::AccountId,
        session: Option<&str>,
    ) -> Arc<Passenger> {
        let resetting = run.seat_discards_ledger(account_id, true, session);
        run.seat(account_id, true, session, resetting)
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
        seat(&run, &account, Some("worker-1"));
        run.bind_lanes(data, account.as_str(), Some("worker-1"));
        assert_eq!(
            run.evict_account(account.as_str(), Some("worker-1")),
            0,
            "the host's second leg is the same client, not a claimant"
        );
        seat(&run, &account, Some("worker-1"));
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

    /// A LEDGER MINTED AFTER AN UNQUALIFIED ARM CARRIES IT. This is the whole of
    /// finding 5: the control plane walked the passengers that existed at the
    /// instant of the request, so an operator who armed a `PartialFillNext` and
    /// then started a subagent got a run that believed it was perturbed and was
    /// not - while `docs/havoc.md` and `arm_divergence`'s own comment both said
    /// the arm reaches a passenger that boards later.
    ///
    /// THREE OBSERVABLES, one per storage class, because the three are applied
    /// by three different lines and any of them can be dropped alone.
    ///
    /// THE ENGINE QUEUE IS OBSERVED THROUGH THE CAP, which is the only reader
    /// `mogwai-engine` exposes: fill the venue record to
    /// `MAX_ARMED_DIVERGENCES` with nobody seated, mint a ledger, and arm once
    /// more. A ledger that replayed the record is AT the cap and sheds its
    /// oldest entry, so the arm reports one; a ledger opened empty has room and
    /// reports nothing. That also pins the mirroring the eviction report's
    /// wording rests on - "every ledger holds the same arms and hits the cap
    /// together" - rather than only the fact of a replay.
    ///
    /// The fee surcharge is applied by the line beside the queue replay in
    /// `ArmRecord::open_engine` and is NOT asserted here: the engine exposes no
    /// reader for it, and inventing one for a test is not worth a public API.
    #[tokio::test]
    async fn a_ledger_minted_after_a_venue_wide_arm_carries_it() {
        let run = run(1_000, 400, None);
        fill_venue_record(&run).await;

        // The subagent that started after the operator armed.
        let late = run.passenger(&mogwai_protocol::AccountId::parse("LATE-001").unwrap());

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
             ledgers that were already seated"
        );
    }

    /// THE SECOND MINT SITE OWES THE SAME REPLAY. `POST /account` builds its own
    /// ledger from client-named balances, and the cold review found it opening
    /// that ledger with every havoc field at zero - so a subagent that starts by
    /// POSTing its account escaped the operator's arms entirely, which is
    /// verbatim the scenario finding 5 names.
    ///
    /// The three observables and the cap trick are `a_ledger_minted_after_a_
    /// venue_wide_arm_carries_it`'s; what is new here is only the path.
    #[tokio::test]
    async fn an_account_opened_by_its_client_carries_the_venue_wide_arms() {
        let run = run(1_000, 400, None);
        fill_venue_record(&run).await;

        let posted = mogwai_protocol::AccountId::parse("POST-001").unwrap();
        run.open_account(
            &posted,
            std::collections::HashMap::from([("USD".to_owned(), Decimal::from(50_000))]),
            mogwai_protocol::risk::AccountPolicy::default(),
        )
        .expect("a fresh id opens");
        let opened = run.peek_passenger(&posted).expect("just opened");

        assert!(
            opened.dark.open_at(SimClock::identity(), 1_000),
            "a blackout armed venue-wide must reach an account its client opened afterwards"
        );
        assert_eq!(
            opened.delay_ms.load(Ordering::Relaxed),
            37,
            "an ack delay armed venue-wide must reach a client-opened account too"
        );
        let shed = run
            .arm(
                None,
                VenueArm::Engine(mogwai_protocol::control::Divergence::DuplicateNextFill),
            )
            .await;
        assert!(
            shed.is_some(),
            "a client-opened ledger must hold the venue's armed queue, at the cap with the \
             ledgers that were already seated"
        );
    }

    /// AN ARM AGAINST AN ACCOUNT THAT HAS NOT CONNECTED MUST NOT COST THAT
    /// CLIENT ITS OWN ACCOUNT. The arm is recorded, not minted: the client's
    /// `POST /account` still succeeds, still gets the balances it asked for, and
    /// finds the arm standing on the ledger it opened.
    ///
    /// Minting at the control plane instead - which an earlier draft of this
    /// round did - answers that request `409 already open` and hands the account
    /// default balances and no policy, so the fix for a silent no-op became a
    /// refusal on a legitimate client path.
    ///
    /// HONEST ABOUT WHAT BITES: the second assertion bites on a dropped record.
    /// The `open_account` call succeeding is a GUARD, not a proven-biting
    /// assertion - reproducing the regression it forbids means putting a mint
    /// back into `Run::arm`, and there is no perturbation of the shipped code
    /// that produces it.
    #[tokio::test]
    async fn a_named_arm_before_a_client_opens_its_account_does_not_lock_it_out() {
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
        let opened = run.peek_passenger(&named).expect("just opened");
        assert!(
            opened.dark.open_at(SimClock::identity(), 1_000),
            "the arm posted before the client existed must be standing on the ledger it opened"
        );
    }

    /// The same record reaches an account that arrives on a SOCKET instead, and
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

        let stranger = run.passenger(&mogwai_protocol::AccountId::parse("SUB-03").unwrap());
        assert!(
            !stranger.dark.open_at(SimClock::identity(), 1_000),
            "an arm naming one account must not black out another"
        );
        let named = run.passenger(&mogwai_protocol::AccountId::parse("SUB-02").unwrap());
        assert!(
            named.dark.open_at(SimClock::identity(), 1_000),
            "an arm naming an account that had not connected must be standing when it does"
        );
    }

    /// A clear lifts the venue RECORD and every pending named one too, or an
    /// account connecting after it would be opened from arms the operator
    /// already lifted. The engine queue is deliberately untouched on the seated
    /// ledgers and on the venue record, which is `Engine::clear_armed`'s
    /// documented split.
    #[tokio::test]
    async fn a_clear_stops_arming_ledgers_minted_after_it() {
        let run = run(1_000, 400, None);
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
            Some("SUB-04"),
            VenueArm::StallData {
                armed_ns: 1_000,
                span_ns: 5_000,
            },
        )
        .await;
        run.clear_venue_arms().await;

        let late = run.passenger(&mogwai_protocol::AccountId::parse("LATE-002").unwrap());
        assert!(
            !late.dark.open_at(SimClock::identity(), 1_000),
            "a cleared blackout must not be re-armed onto a ledger minted after the clear"
        );
        assert_eq!(
            late.delay_ms.load(Ordering::Relaxed),
            0,
            "a cleared ack delay must not be re-armed either"
        );
        let named = run.passenger(&mogwai_protocol::AccountId::parse("SUB-04").unwrap());
        assert!(
            !named.stall.open_at(SimClock::identity(), 1_000),
            "a clear stops everything, including an arm waiting for an account to arrive"
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

    /// A SOCKET RECLAIMING A LONG-FROZEN LEDGER MUST NOT HAVE IT COLLECTED OUT
    /// FROM UNDER IT, which the freeze stamp alone cannot express.
    ///
    /// A returning `/ws` upgrade is counted onto its account before the 101 and
    /// only clears the freeze once its handler reaches `resume` - the 101, a
    /// task spawn and an instrument registration later. Collecting on the stamp
    /// alone therefore discarded the account inside that window, and the client
    /// that came back for its book was silently minted a fresh one. Both
    /// assertions are here because the second alone passes against the broken
    /// shape: what proves the fix is the account surviving WHILE admitted.
    #[test]
    fn an_admitted_socket_spares_the_account_it_is_reclaiming() {
        let run = run(1_000, 400, None);
        let returning = mogwai_protocol::AccountId::parse("BACK-001").unwrap();
        let passenger = run.passenger(&returning);
        assert!(
            passenger.is_frozen(),
            "an account nobody has connected to is unattended, and this test is about that state"
        );

        let admission = run.admit(&passenger);
        assert!(
            run.collect_expired_accounts(std::time::Duration::ZERO)
                .is_empty(),
            "a socket is on its way in: the ledger it is reclaiming must still be there when it \
             arrives"
        );

        drop(admission);
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
        // warmup generation - NOT from boot. A run whose warmup is larger than
        // its duration must still get its whole declared duration.
        let bounded = run(1_000_000, 999_000, Some(30));
        assert_eq!(bounded.deadline_ns, Some(1_000_030));

        let indefinite = run(1_000, 0, None);
        assert_eq!(indefinite.deadline_ns, None);
    }
}
