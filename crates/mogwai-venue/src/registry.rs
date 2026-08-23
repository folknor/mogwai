// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The connection registry: one structure that answers who is reading an
//! account.
//!
//! Four structures used to hold parts of that answer - a run-wide lane table,
//! a per-account attach count, a per-account map of rides, and a freeze stamp
//! that doubled as the authority on whether an account was attended. The
//! consistency rules between them lived in prose, two live defects were closed
//! by adding the fourth rather than by removing the possibility, and nothing
//! detected the next lifecycle path that updated three of them.
//!
//! Here the connection is the record and everything else is derived from it. An
//! account is attended when some connection of it is reading; it rides a boat
//! when some reading connection of it holds that boat. Neither is stored, so
//! neither can disagree with the connection table that produced it.
//!
//! # The continuity lease
//!
//! Deriving attendance from readers alone is wrong during an eviction, and this
//! is the subtlety the whole design turns on. A newcomer is admitted, the
//! incumbent's lane is released, and the newcomer has not reached its own
//! reading boundary yet. There are zero readers in that window and the account
//! is not unattended: a successor has already been committed to it. Freezing
//! there would retire a book and re-base every scan frontier, nondeterministically,
//! because a socket arrived.
//!
//! So a committed admission takes a handoff, and the unattended transition is
//! readers reaching zero with no handoff outstanding. A pending admission is
//! deliberately not a handoff: it is a proposal that can still fail, and a
//! proposal must not thaw an account.
//!
//! # Reserve, place, commit
//!
//! Admission is fallible and slow - it places a boat, which materializes tape -
//! and it must be exclusive, because two upgrades claiming one account must not
//! each pass a check against a ledger the other is replacing. Holding this
//! registry's mutex across that work would make it an ambient lock over tape
//! synthesis.
//!
//! The reservation resolves it. Phase one takes the lock, decides every refusal
//! that can be decided from registry state, and installs an exclusive
//! `PendingAdmission`. Phase two does the fallible work holding no lock. Phase
//! three retakes the lock, revalidates the reservation is still the one it
//! installed, and commits. The commit is the sole linearization point, and the
//! incumbent is untouched until it succeeds - so a slow or failed placement
//! costs a live consumer nothing, where previously a placement failure could
//! arrive after the eviction.
//!
//! The one hard rule, which is what keeps the lock graph flat: **no guard from
//! this module survives an await, a placement, an engine acquisition, a lane
//! send, or a ticket drop.** The registry mutex and the engine's async mutex
//! never nest in either direction. Displaced lanes are handed back out of the
//! commit and closed by the caller after the lock is released, so a destructor
//! cannot reach back in.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::admission::ExecLanes;
use crate::boatyard::BoatKey;
use crate::source::RiverKey;

/// The water an admission intends to read, and the cadence it intends to read
/// it at.
///
/// Known before the boat exists, which is what lets the cadence rule be decided
/// in phase one: the river is resolved from the request and the speed is
/// quantized from it, so neither waits on a placement.
///
/// One ledger, one cadence, per river. Two sockets of one account may ride two
/// rivers at two speeds - a supported shape - but two speeds of one river would
/// judge one book on two clocks.
///
/// Whether that rule should be account-wide rather than per river is a real
/// question and is not settled here: a ledger's balances, settled cash, daily
/// resets and peak-equity ratchet are all functions of simulated time, so an
/// account reading two rivers at two cadences already drifts, which is the open
/// multi-river peak-equity item. Tightening it is a product decision with a
/// consumer-visible refusal attached, so it is filed rather than smuggled in
/// behind a structural rewrite.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct Seat {
    pub(crate) river: RiverKey,
    pub(crate) speed_micros: u64,
}

impl Seat {
    pub(crate) fn speed(&self) -> f64 {
        self.speed_micros as f64 / 1_000_000.0
    }
}

/// How far a connection has got through admission.
///
/// `Committed` and `Reading` are distinct because attendance is a question
/// about delivery, not about admission: a committed connection has passed every
/// check and displaced whoever it replaced, but nothing reaches a consumer
/// through it until its handler binds lanes and begins reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Admitted and holding a continuity handoff, not yet delivering.
    Committed,
    /// Delivering. This is what makes an account attended.
    Reading,
}

struct ConnectionRecord {
    id: u64,
    callsign: Option<String>,
    phase: Phase,
    /// The boat this connection reads, and the only record of the ride. A ride
    /// is a property of the connection rather than a counted per-account map:
    /// two sockets of one ledger sharing a boat are two records naming it, so
    /// the first to close cannot take the ride away from the second, and there
    /// is no separate release to forget.
    ride: Option<BoatKey>,
    lanes: Option<ExecLanes>,
}

/// An admission in flight, holding exclusive authority over one account while
/// the fallible work happens outside the lock.
///
/// The id alone, because what the admission intends - its ledger incarnation and
/// the seat it means to take - is carried by the `Reservation` the caller holds
/// and is checked back against this entry at commit. Storing a second copy here
/// would be two records of one intention that could disagree.
struct PendingAdmission {
    id: u64,
}

struct AccountEntry {
    /// Which ledger this is. Bumped whenever the account is reset, so a
    /// reservation taken against the outgoing ledger cannot commit onto the
    /// incoming one.
    incarnation: u64,
    connections: Vec<ConnectionRecord>,
    pending: Option<PendingAdmission>,
    /// Committed admissions that have not begun reading. See the continuity
    /// lease in the module docs.
    handoffs: usize,
    /// When this account last became unattended. Payload of a derived
    /// transition rather than an independent source of truth: whether the
    /// account is frozen is answered by the connection table, and this records
    /// the instant of a transition that cannot be reconstructed afterwards.
    ///
    /// Wall rather than simulated, because a frozen account has no simulated
    /// clock: the boat that carried one wound down with its last passenger.
    /// This is what the TTL is measured against.
    frozen_since: Option<Instant>,
}

impl AccountEntry {
    fn new(incarnation: u64) -> Self {
        Self {
            incarnation,
            connections: Vec::new(),
            pending: None,
            handoffs: 0,
            // Created frozen. An account exists from the instant its id is
            // first seen, which is before any socket reads it.
            frozen_since: Some(Instant::now()),
        }
    }

    fn readers(&self) -> usize {
        self.connections
            .iter()
            .filter(|conn| conn.phase == Phase::Reading)
            .count()
    }

    /// The unattended test, and the one place it is decided.
    fn attended(&self) -> bool {
        self.readers() > 0 || self.handoffs > 0
    }

    /// Re-derive the freeze stamp after any transition that could have changed
    /// attendance. Idempotent, so every path that might be the last may call it.
    ///
    /// A frozen account holds no rides, because its connections are gone, so it
    /// is free to come back at whatever cadence its consumer now wants. That
    /// falls out of deriving the rule from connections rather than storing it.
    fn settle_attendance(&mut self) {
        if self.attended() {
            self.frozen_since = None;
        } else {
            if self.frozen_since.is_none() {
                self.frozen_since = Some(Instant::now());
            }
        }
    }
}

/// What a refused admission is refused for.
#[derive(Debug)]
pub(crate) enum AdmissionRefusal {
    /// This account already rides that river at a different cadence. Carries the
    /// seat it is sitting in, so the refusal can name the cadence that is
    /// actually there rather than the one that was asked for.
    CadenceConflict(Seat),
    /// Another upgrade holds this account. One admission at a time per account
    /// is what makes the commit a linearization point; a consumer that loses
    /// may retry.
    Busy,
}

/// Exclusive authority over one account's admission, held across the fallible
/// work that happens outside the registry lock.
///
/// Dropping it without committing rolls the reservation back, which is why the
/// upgrade path may bail at any point between reserve and commit - including by
/// having its future cancelled - without stranding the account. The rollback is
/// synchronous and touches only this registry: anything slow the caller was
/// holding, a boat ticket in particular, is dropped by the caller after this
/// guard has released the lock.
pub(crate) struct Reservation {
    registry: std::sync::Arc<ConnectionRegistry>,
    account_id: String,
    id: u64,
    incarnation: u64,
    /// What this admission intends to ride. Kept so a test can read back what
    /// was reserved; the commit takes the ride it actually placed, because the
    /// boat is the authority on that once it exists.
    #[cfg_attr(not(test), expect(dead_code, reason = "read back only by tests"))]
    seat: Seat,
    committed: bool,
}

impl Reservation {
    #[cfg(test)]
    pub(crate) fn seat(&self) -> &Seat {
        &self.seat
    }

    pub(crate) fn account_id(&self) -> &str {
        &self.account_id
    }
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("account_id", &self.account_id)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.registry.rollback(&self.account_id, self.id);
    }
}

/// What a commit displaced, handed back so the caller can close it after the
/// registry lock is released.
pub(crate) struct Committed {
    /// Lanes belonging to connections this admission evicted. Closing a lane
    /// sends on a channel and can block, so it never happens under the lock.
    pub(crate) displaced: Vec<BoundLane>,
    /// This connection's identity, minted here rather than by the lanes it will
    /// later bind. The registry has to name the connection at commit, which is
    /// before any socket machinery exists, and the same id must reach
    /// `ExecLanes` so an order accepted on those lanes is attributable to the
    /// connection that submitted it.
    pub(crate) connection_id: u64,
    /// Whether this admission found the account unattended, and is therefore a
    /// return from a freeze rather than an additional socket on a live ledger.
    ///
    /// Answered here because this is the only instant that can answer it. The
    /// commit itself makes the account attended, so anything downstream that
    /// asks "was it frozen" gets `false` no matter how long the ledger had been
    /// sitting. What rides on it is whether the returning passenger retires the
    /// book its account holds off the river it is joining, which is a
    /// consumer-visible cancellation and must not fire on an ordinary second
    /// socket of a live account.
    pub(crate) resumed_from_freeze: bool,
}

/// One connection's outbound lanes, tagged with who holds them.
#[derive(Clone)]
pub(crate) struct BoundLane {
    pub(crate) id: u64,
    pub(crate) account_id: String,
    pub(crate) lanes: ExecLanes,
}

pub(crate) struct ConnectionRegistry {
    entries: Mutex<HashMap<String, AccountEntry>>,
    next_reservation: AtomicU64,
    next_connection: AtomicU64,
}

impl ConnectionRegistry {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            next_reservation: AtomicU64::new(1),
            next_connection: AtomicU64::new(1),
        })
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<String, AccountEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Phase one. Decide every refusal answerable from registry state, then take
    /// exclusive authority over this account's admission.
    ///
    /// The cadence rule is decided here, against the rides this account's
    /// connections already hold, and the reservation then holds the account
    /// exclusively until it commits or rolls back. That is what closes the race
    /// the old ordering could not: the check and the claim used to be separate
    /// steps with an eviction between them, so an upgrade could pass the check,
    /// evict an incumbent, and then lose the account to a concurrent upgrade and
    /// have to refuse - after the incumbent was already closed.
    ///
    /// `resetting` says the caller is about to discard this ledger. A reset
    /// account keeps no book and no clock, so nothing it was riding constrains
    /// what the newcomer may ride; asking the outgoing ledger would refuse
    /// exactly the reconnect-at-a-new-cadence the reset knob exists to serve.
    pub(crate) fn reserve(
        self: &std::sync::Arc<Self>,
        account_id: &str,
        seat: Seat,
        resetting: bool,
    ) -> Result<Reservation, AdmissionRefusal> {
        let mut entries = self.locked();
        let entry = entries
            .entry(account_id.to_owned())
            .or_insert_with(|| AccountEntry::new(1));
        if entry.pending.is_some() {
            return Err(AdmissionRefusal::Busy);
        }
        if resetting {
            entry.incarnation += 1;
        } else if let Some(sitting) = entry.connections.iter().find_map(|conn| {
            conn.ride.as_ref().filter(|ride| {
                *ride.river() == seat.river && ride.speed_micros() != seat.speed_micros
            })
        }) {
            return Err(AdmissionRefusal::CadenceConflict(Seat {
                river: sitting.river().clone(),
                speed_micros: sitting.speed_micros(),
            }));
        }
        let id = self.next_reservation.fetch_add(1, Ordering::Relaxed);
        let incarnation = entry.incarnation;
        entry.pending = Some(PendingAdmission { id });
        Ok(Reservation {
            registry: std::sync::Arc::clone(self),
            account_id: account_id.to_owned(),
            id,
            incarnation,
            seat,
            committed: false,
        })
    }

    /// Phase three. Install the connection, take its continuity handoff, and
    /// select whoever it displaces.
    ///
    /// The displaced set is returned rather than closed here: closing a lane
    /// sends on a channel, and doing that under this lock would make every
    /// consumer's teardown cost a registry acquisition.
    ///
    /// Sockets sharing a callsign coexist; a different or absent callsign
    /// displaces the incumbent, which is the whole of the eviction rule and is
    /// stated over the callsign because the callsign is the only identity the
    /// venue has.
    ///
    /// `evicts` is false for a connection that named no account. Naming an id is
    /// a statement about identity - this ledger is mine, hand it over - and
    /// eviction is the answer to it; naming none means the consumer has no
    /// opinion, and the default account exists for exactly that case. Evicting
    /// there would break the shape it serves, since one consumer opening two
    /// sockets on two symbols names no account on either and would evict itself.
    pub(crate) fn commit(
        &self,
        reservation: &mut Reservation,
        callsign: Option<&str>,
        ride: Option<BoatKey>,
        evicts: bool,
    ) -> Committed {
        let connection_id = self.next_connection.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.locked();
        let Some(entry) = entries.get_mut(&reservation.account_id) else {
            // The account cannot vanish while a reservation is outstanding, so
            // this is unreachable; treated as a lost commit rather than a panic
            // because the caller is mid-upgrade and a refusal is recoverable.
            reservation.committed = true;
            return Committed {
                displaced: Vec::new(),
                connection_id,
                resumed_from_freeze: false,
            };
        };
        // Revalidate under the lock. The reservation is exclusive, so nothing
        // should have moved, and this is cheap enough to assert rather than
        // assume: a commit onto a ledger incarnation other than the one every
        // check upstream was taken against would install a connection whose
        // funding and timeline were decided for a ledger that no longer exists.
        let stale = entry.pending.as_ref().map(|pending| pending.id) != Some(reservation.id)
            || entry.incarnation != reservation.incarnation;
        if stale {
            reservation.committed = true;
            return Committed {
                displaced: Vec::new(),
                connection_id,
                resumed_from_freeze: false,
            };
        }
        // Sampled before this admission is installed, because installing it is
        // what makes the account attended.
        let resumed_from_freeze = !entry.attended();
        entry.pending = None;
        reservation.committed = true;
        // Who is displaced, and separately which of them have lanes to close.
        // The two are not the same set: a committed incumbent that has not
        // reached its reading boundary holds no lanes and must still be
        // displaced, so deriving the displaced set from the lanes it yielded
        // would leave it registered and holding a handoff forever.
        let evicted = |conn: &ConnectionRecord| {
            evicts
                && match (conn.callsign.as_deref(), callsign) {
                    // Sockets sharing a callsign coexist, which is what lets one
                    // nautilus host hold a data leg and an execution leg under
                    // one account without evicting itself.
                    (Some(held), Some(claiming)) => held != claiming,
                    // Silence is never a claim to be the incumbent, so an absent
                    // callsign on either side evicts. Reading it as "same
                    // identity" would let a stranger quietly share a ledger.
                    _ => true,
                }
        };
        let displaced: Vec<BoundLane> = entry
            .connections
            .iter_mut()
            .filter(|conn| evicted(conn))
            .filter_map(|conn| {
                conn.lanes.take().map(|lanes| BoundLane {
                    id: conn.id,
                    account_id: reservation.account_id.clone(),
                    lanes,
                })
            })
            .collect();
        // A displaced connection that was still Committed gives up the handoff
        // it was holding, or the account would be attended by a record nothing
        // will ever release.
        let surrendered = entry
            .connections
            .iter()
            .filter(|conn| evicted(conn) && conn.phase == Phase::Committed)
            .count();
        entry.handoffs = entry.handoffs.saturating_sub(surrendered);
        entry.connections.retain(|conn| !evicted(conn));
        entry.connections.push(ConnectionRecord {
            id: connection_id,
            callsign: callsign.map(str::to_owned),
            phase: Phase::Committed,
            ride,
            lanes: None,
        });
        entry.handoffs += 1;
        entry.settle_attendance();
        Committed {
            displaced,
            connection_id,
            resumed_from_freeze,
        }
    }

    /// The connection reaches its reading boundary: bind its lanes, and retire
    /// the handoff it has been holding since commit.
    ///
    /// Retiring the handoff here rather than at commit is what makes the lease
    /// cover exactly the window it must: from the instant the incumbent may be
    /// closed until the instant the successor is actually delivering.
    pub(crate) fn begin_reading(&self, account_id: &str, connection_id: u64, lanes: ExecLanes) {
        let mut entries = self.locked();
        let Some(entry) = entries.get_mut(account_id) else {
            return;
        };
        let Some(conn) = entry
            .connections
            .iter_mut()
            .find(|conn| conn.id == connection_id)
        else {
            return;
        };
        if conn.phase == Phase::Committed {
            conn.phase = Phase::Reading;
            conn.lanes = Some(lanes);
            entry.handoffs = entry.handoffs.saturating_sub(1);
        }
        entry.settle_attendance();
    }

    /// The connection is gone. Removes its record, its ride and its lanes in one
    /// transition, and re-derives attendance.
    ///
    /// One removal rather than the several the old shape needed, which is what
    /// retires the tolerance it carried: a ride used to be given up separately
    /// from a lane and from an attach count, at different moments, so each
    /// release had to tolerate finding nothing.
    pub(crate) fn release(&self, account_id: &str, connection_id: u64) {
        let mut entries = self.locked();
        let Some(entry) = entries.get_mut(account_id) else {
            return;
        };
        if let Some(index) = entry
            .connections
            .iter()
            .position(|conn| conn.id == connection_id)
        {
            let conn = entry.connections.remove(index);
            if conn.phase == Phase::Committed {
                // Committed and gone before it ever read: release the handoff it
                // was holding. The incumbent it displaced is already gone, so
                // the account genuinely becomes unattended here.
                entry.handoffs = entry.handoffs.saturating_sub(1);
            }
        }
        entry.settle_attendance();
    }

    /// Every lane a venue-originated batch could be delivered to.
    ///
    /// Cloned out under the lock rather than held across the delivery: delivery
    /// serializes JSON and touches per-connection budgets, and doing that while
    /// holding a registry-wide mutex would let one connection's cost block every
    /// other connection's teardown.
    pub(crate) fn bound_lanes(&self) -> Vec<BoundLane> {
        let entries = self.locked();
        entries
            .iter()
            .flat_map(|(account_id, entry)| {
                entry.connections.iter().filter_map(move |conn| {
                    conn.lanes.clone().map(|lanes| BoundLane {
                        id: conn.id,
                        account_id: account_id.clone(),
                        lanes,
                    })
                })
            })
            .collect()
    }

    /// Whether this account is riding `key`, derived from its reading
    /// connections rather than stored.
    pub(crate) fn is_seated_on(&self, account_id: &str, key: &BoatKey) -> bool {
        let entries = self.locked();
        entries.get(account_id).is_some_and(|entry| {
            entry
                .connections
                .iter()
                .any(|conn| conn.ride.as_ref() == Some(key))
        })
    }

    /// Register an account that exists but has never been connected to.
    ///
    /// Called from every mint, so the registry knows an account from the instant
    /// its id is first seen rather than from its first admission. Without it a
    /// minted-and-abandoned ledger has no entry, and every derived answer about
    /// it has to guess: `frozen_for` would report nothing unattended and the TTL
    /// would never collect it, which is precisely the immortal-account failure
    /// the freeze stamp exists to prevent.
    ///
    /// Idempotent, and never disturbs an entry that already exists.
    pub(crate) fn ensure(&self, account_id: &str) {
        self.locked()
            .entry(account_id.to_owned())
            .or_insert_with(|| AccountEntry::new(1));
    }

    /// Whether some connection of this account already presents `callsign`.
    ///
    /// Counts committed connections as well as reading ones, so a second leg
    /// arriving while the first is still between its commit and its first read
    /// is recognized as the same identity rather than treated as a stranger.
    pub(crate) fn has_callsign_on(&self, account_id: &str, callsign: &str) -> bool {
        let entries = self.locked();
        entries.get(account_id).is_some_and(|entry| {
            entry
                .connections
                .iter()
                .any(|conn| conn.callsign.as_deref() == Some(callsign))
        })
    }

    /// Whether nothing is reading this account.
    pub(crate) fn is_frozen(&self, account_id: &str) -> bool {
        let entries = self.locked();
        entries
            .get(account_id)
            .is_none_or(|entry| !entry.attended())
    }

    /// How long this account has been unattended, or `None` while it is read.
    pub(crate) fn frozen_for(&self, account_id: &str) -> Option<Duration> {
        let entries = self.locked();
        let entry = entries.get(account_id)?;
        if entry.attended() {
            return None;
        }
        entry.frozen_since.map(|since| since.elapsed())
    }

    /// Forget an account entirely. Reached when the TTL collects a ledger
    /// nobody reclaimed, so the registry does not outlive what it describes.
    pub(crate) fn forget(&self, account_id: &str) {
        self.locked().remove(account_id);
    }

    /// Give up a reservation that never committed.
    fn rollback(&self, account_id: &str, reservation_id: u64) {
        let mut entries = self.locked();
        let Some(entry) = entries.get_mut(account_id) else {
            return;
        };
        if entry.pending.as_ref().map(|pending| pending.id) == Some(reservation_id) {
            entry.pending = None;
        }
        // A failed proposal changes no lifecycle state, so attendance is
        // re-derived rather than assumed: the account was frozen before this
        // reservation and stays frozen, and a live one stays live.
        entry.settle_attendance();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat() -> Seat {
        Seat {
            river: RiverKey::synthetic(1),
            speed_micros: 1_000_000,
        }
    }

    fn seat_at(speed_micros: u64) -> Seat {
        Seat {
            river: RiverKey::synthetic(1),
            speed_micros,
        }
    }

    #[test]
    fn an_account_with_no_connection_is_frozen() {
        let registry = ConnectionRegistry::new();
        let reservation = registry.reserve("A", seat(), false).unwrap();
        drop(reservation);
        assert!(registry.is_frozen("A"));
    }

    #[test]
    fn a_committed_admission_holds_the_account_attended_before_it_reads() {
        // The continuity lease. Between commit and the reading boundary there
        // are zero readers, and the account must not read as unattended - a
        // freeze there retires a book because a socket arrived.
        let registry = ConnectionRegistry::new();
        let mut reservation = registry.reserve("A", seat(), false).unwrap();
        drop(registry.commit(&mut reservation, None, None, true));
        assert!(!registry.is_frozen("A"));
        assert!(registry.frozen_for("A").is_none());
    }

    #[test]
    fn a_committed_admission_that_never_reads_freezes_on_release() {
        let registry = ConnectionRegistry::new();
        let mut reservation = registry.reserve("A", seat(), false).unwrap();
        drop(registry.commit(&mut reservation, None, None, true));
        registry.release("A", 1);
        assert!(registry.is_frozen("A"));
    }

    #[test]
    fn a_rolled_back_reservation_leaves_a_live_account_live() {
        let registry = ConnectionRegistry::new();
        let mut first = registry.reserve("A", seat(), false).unwrap();
        drop(registry.commit(&mut first, Some("one"), None, true));
        // The reservation is dropped without committing, exactly as an upgrade
        // that fails placement or has its future cancelled would leave it.
        let second = registry.reserve("A", seat(), false).unwrap();
        drop(second);
        assert!(!registry.is_frozen("A"));
    }

    #[test]
    fn one_admission_at_a_time_per_account() {
        let registry = ConnectionRegistry::new();
        let _first = registry.reserve("A", seat(), false).unwrap();
        assert!(matches!(
            registry.reserve("A", seat(), false),
            Err(AdmissionRefusal::Busy)
        ));
    }

    fn ride(speed: f64) -> BoatKey {
        BoatKey::new(RiverKey::synthetic(1), speed).expect("a legal speed")
    }

    #[test]
    fn a_second_cadence_on_one_river_is_refused() {
        let registry = ConnectionRegistry::new();
        let mut first = registry.reserve("A", seat(), false).unwrap();
        drop(registry.commit(&mut first, None, Some(ride(1.0)), true));
        assert!(matches!(
            registry.reserve("A", seat_at(2_000_000), false),
            Err(AdmissionRefusal::CadenceConflict(held)) if held == seat()
        ));
    }

    /// The other river is a different river, so it is not a second cadence on
    /// this one. Two sockets of one account riding two rivers at two speeds is a
    /// supported shape, and a rule that refused it would break the default
    /// account's two-symbol case.
    #[test]
    fn a_second_cadence_on_another_river_is_admitted() {
        let registry = ConnectionRegistry::new();
        let mut first = registry.reserve("A", seat(), false).unwrap();
        drop(registry.commit(&mut first, None, Some(ride(1.0)), true));
        let elsewhere = Seat {
            river: RiverKey::synthetic(2),
            speed_micros: 2_000_000,
        };
        assert!(registry.reserve("A", elsewhere, false).is_ok());
    }

    /// A ride ends with its connection, so the cadence it held stops
    /// constraining the account. Nothing has to release it separately.
    #[test]
    fn a_departed_ride_stops_refusing_a_new_cadence() {
        let registry = ConnectionRegistry::new();
        let mut first = registry.reserve("A", seat(), false).unwrap();
        let committed = registry.commit(&mut first, None, Some(ride(1.0)), true);
        registry.release("A", committed.connection_id);
        assert!(registry.reserve("A", seat_at(2_000_000), false).is_ok());
    }

    #[test]
    fn a_reset_ledger_may_bind_any_seat() {
        let registry = ConnectionRegistry::new();
        let mut first = registry.reserve("A", seat(), false).unwrap();
        drop(registry.commit(&mut first, None, Some(ride(1.0)), true));
        let faster = seat_at(4_000_000);
        let reserved = registry.reserve("A", faster.clone(), true).unwrap();
        assert_eq!(reserved.seat(), &faster);
    }

    #[test]
    fn a_shared_callsign_coexists_and_a_different_one_displaces() {
        let registry = ConnectionRegistry::new();
        let mut first = registry.reserve("A", seat(), false).unwrap();
        drop(registry.commit(&mut first, Some("leg"), None, true));
        let mut second = registry.reserve("A", seat(), false).unwrap();
        let committed = registry.commit(&mut second, Some("leg"), None, true);
        assert!(committed.displaced.is_empty());
        let mut third = registry.reserve("A", seat(), false).unwrap();
        let committed = registry.commit(&mut third, Some("other"), None, true);
        // Neither incumbent had bound lanes, so nothing is handed back to
        // close; what matters is that they are no longer registered.
        assert!(committed.displaced.is_empty());
        assert!(!registry.is_frozen("A"));
    }
}
