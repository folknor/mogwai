// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! On-demand paced boats, shared by passengers asking for the same river and speed.
//!
//! Three nouns, and the boat is the one that carries no semantics.
//!
//! A river is a tape and is shared. Its identity is everything that mutates the
//! water: symbol or preset, session shape, loop shape, seed, resolved
//! bundle, market regime and generator havoc. The tape protocol version is a
//! build identity rather than a key field: one process cannot contain rivers
//! from two builds. Speed is not in that list, because it changes delivery
//! cadence and no generated value.
//!
//! A passenger is one connected trader: its own account, ledger, orders and
//! view, never shared, one per connection.
//!
//! A boat is this module's cache. It is a cursor keyed by (river, speed) whose
//! whole purpose is to generate and pace one river once rather than N times.
//! Nothing a passenger can observe depends on whether it shares a cursor, so
//! reasoning about which passengers may share a boat is reasoning about a cache
//! as though it carried meaning. Duration and the transport havoc family
//! (`GoDark`, `DelayAcks`, `StallData`, `CommandLatency`) are passenger-local
//! and therefore never split a river.
//!
//! The test for any new knob is one question: does it change the water or the
//! view? Water goes into river identity, so callers wanting different answers
//! get different rivers. A view change rides the passenger and leaves the river
//! shareable.

use mogwai_protocol::SimClock;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};
use tokio::sync::Semaphore;

use crate::{
    config::now_ns,
    source::{RiverKey, Rivers},
    tape::{Tape, TapeFunding, TapeSpawn},
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BoatKey {
    river: RiverKey,
    speed_micros: u64,
    placement: Placement,
}

/// Where a boat's clock is anchored: the run's shared origin, or a named
/// window's own start.
///
/// Only the start, deliberately. The window's end is the passenger's - each
/// passenger cuts its own delivery off and completes at its own end, exactly
/// as a passenger duration works - and an owner discriminator used to sit
/// here too, splitting identical placements into per-consumer hulls. Both
/// were semantics on a cache: the glossary's Boat entry says passengers
/// asking for the same river and the same speed share one boat, and a hull
/// is shareable by anyone whose clock it is, because the tape is exogenous
/// and broadcast frames carry no passenger identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Placement {
    Shared,
    Named { start_ns: u64 },
}

/// The one quantization. Boarding and looking a cadence up both go through
/// here, so a consumer writing `100.0000001` cannot board one boat and then fail
/// to find it.
///
/// Micro-multiples, so the key is `Hash` and `Eq` and two consumers writing
/// `100` and `100.0000001` share a boat. Bounded first, by
/// `mogwai_protocol::control::validate_delivery_speed`, which is the one place
/// the bound is written: `mogwai-adapter` refuses the same values against the
/// same function, so a config cannot pass its own validation and then take this
/// 400 forever.
pub(crate) fn quantize_speed(speed: f64) -> anyhow::Result<u64> {
    if let Err(refusal) = mogwai_protocol::control::validate_delivery_speed(speed) {
        anyhow::bail!(refusal);
    }
    Ok((speed * 1_000_000.0).round() as u64)
}

impl BoatKey {
    #[cfg(test)]
    pub(crate) fn new(river: RiverKey, speed: f64) -> anyhow::Result<Self> {
        Ok(Self {
            river,
            speed_micros: quantize_speed(speed)?,
            placement: Placement::Shared,
        })
    }

    #[cfg(test)]
    pub(crate) fn named(river: RiverKey, speed: f64, start_ns: u64) -> anyhow::Result<Self> {
        Ok(Self {
            river,
            speed_micros: quantize_speed(speed)?,
            placement: Placement::Named { start_ns },
        })
    }

    fn for_request(req: &BoardRequest) -> anyhow::Result<Self> {
        Ok(Self {
            river: req.river.clone(),
            speed_micros: quantize_speed(req.speed)?,
            placement: req
                .window
                .as_ref()
                .map_or(Placement::Shared, |window| Placement::Named {
                    start_ns: window.start_ns,
                }),
        })
    }

    pub(crate) fn river(&self) -> &RiverKey {
        &self.river
    }

    pub(crate) fn speed_micros(&self) -> u64 {
        self.speed_micros
    }

    /// The quantized speed, back in the units a consumer asked in. The one
    /// dequantization, so a message naming a sitting cadence and a lookup
    /// matching one cannot disagree about what `2` means.
    pub(crate) fn speed(&self) -> f64 {
        self.speed_micros as f64 / 1_000_000.0
    }

    pub(crate) fn origin_ns(&self, shared_origin_ns: u64) -> u64 {
        match self.placement {
            Placement::Shared => shared_origin_ns,
            Placement::Named { start_ns } => start_ns,
        }
    }

    /// The placement epoch this boat's clock is anchored on, as admission
    /// compares it. The start alone is the epoch: a clock is its rate and its
    /// epoch, and the window's end never enters the clock - it is the
    /// passenger's own delivery cutoff, exactly as a duration is.
    pub(crate) fn placement_start_ns(&self) -> Option<u64> {
        match self.placement {
            Placement::Shared => None,
            Placement::Named { start_ns } => Some(start_ns),
        }
    }
}

pub(crate) struct Boat {
    key: BoatKey,
    pub(crate) sim: SimClock,
    pub(crate) tape: Arc<Tape>,
    pub(crate) last_swept_ns: AtomicU64,
    /// Completed fill-sweeper passes on this boat. This is an observation
    /// seam, not a scheduling input.
    pub(crate) completed_sweep_passes: AtomicU64,
    /// This river's acceptance-time market reading, memoized per sweep-interval
    /// bucket on THIS boat's clock. Per boat because the bucket is a function
    /// of the boat's clock and the walk it saves is a walk of this river only:
    /// a run-level memo held one entry, so two symbols evicted each other into
    /// a guaranteed miss and then serialized on the walk behind one mutex.
    pub(crate) market_readings: crate::fills::MarketReadingCache,
    /// The high and low this river reached since the sweeper last looked,
    /// written by the tape thread. What gives peak equity and a trailing stop
    /// tick resolution without a per-tick evaluation; see `crate::extremes`.
    pub(crate) extremes: Arc<crate::extremes::PriceExtremes>,
    /// The resident trailing print window the same thread keeps, so a market
    /// reading miss folds resident prints instead of regenerating the tape;
    /// see `crate::vol_window` for the coverage and fallback rules.
    pub(crate) vol_window: Arc<crate::vol_window::VolWindow>,
    worker: Mutex<Option<JoinHandle<()>>>,
    cancel: Arc<AtomicBool>,
}

/// A placement in flight, and the handoff every other joiner waits on. A
/// `Semaphore` and not a `Notify`: a joiner clones this while holding the
/// registry mutex and only then awaits, so a wakeup delivered in that window
/// must be remembered. `notify_waiters` forgets it and wedges the joiner
/// forever; a closed semaphore fails every later acquire immediately.
enum Slot {
    Placing(Arc<Semaphore>),
    Placed(PlacedBoat),
    /// A placement that failed, kept so every joiner on the same key observes
    /// the one result of the one operation they joined.
    ///
    /// Without it a failure was not a result at all: the slot was removed, the
    /// waiter's `acquire` error was discarded, the loop saw no slot and
    /// installed a fresh `Placing` of its own. So N joiners on one key ran the
    /// same deterministic failure N times, serially, each paying the whole walk
    /// before failing exactly as the first one had. They joined one placement
    /// and each got their own.
    ///
    /// Retained for the life of the run rather than retried, because every
    /// failure this can hold is deterministic in the key: the river cap is a
    /// property of the run, and a generator that could not walk this river to
    /// this origin will not walk it next time either.
    Failed(Arc<PlacementFailure>),
}
struct PlacedBoat {
    boat: Arc<Boat>,
    passengers: u32,
}

/// Why a placement failed, and whose fault it was.
///
/// Shared rather than cloned because the underlying error is not `Clone` and
/// because every waiter is meant to see the same failure, not a copy of it.
#[derive(Debug)]
pub(crate) struct PlacementFailure {
    pub(crate) message: String,
    /// Whether the venue failed to keep a promise, as opposed to answering the
    /// request. Decides the status the consumer sees and whether the run
    /// latches a terminal fault; see `MaterializeRefusal::is_venue_fault`.
    pub(crate) venue_fault: bool,
}

impl std::fmt::Display for PlacementFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub(crate) struct Boatyard {
    rivers: Arc<Rivers>,
    boats: Mutex<HashMap<BoatKey, Slot>>,
    fanout_depth: usize,
    fault_tx: mpsc::Sender<mogwai_data::TickFault>,
    origin_ns: u64,
}

pub(crate) struct BoardRequest {
    pub(crate) river: RiverKey,
    pub(crate) speed: f64,
    pub(crate) window: Option<mogwai_protocol::control::TapeWindow>,
}
impl BoardRequest {
    #[cfg(test)]
    pub(crate) fn shared(river: RiverKey, speed: f64) -> Self {
        Self {
            river,
            speed,
            window: None,
        }
    }

    #[cfg(test)]
    fn named(river: RiverKey, start_ns: u64, end_ns: u64) -> Self {
        Self {
            river,
            speed: 1.0,
            window: Some(mogwai_protocol::control::TapeWindow {
                start_ns,
                end_ns,
                data_origin_ns: start_ns.saturating_sub(400),
            }),
        }
    }
}
#[derive(Debug)]
pub(crate) enum BoardRefusal {
    Placement(Arc<PlacementFailure>),
}

pub(crate) struct Ticket {
    yard: Arc<Boatyard>,
    boat: Arc<Boat>,
}

impl Boatyard {
    pub(crate) fn new(
        rivers: Arc<Rivers>,
        fanout_depth: usize,
        fault_tx: mpsc::Sender<mogwai_data::TickFault>,
        origin_ns: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            rivers,
            boats: Mutex::new(HashMap::new()),
            fanout_depth,
            fault_tx,
            origin_ns,
        })
    }
    pub(crate) async fn board(
        self: &Arc<Self>,
        req: &BoardRequest,
    ) -> Result<Ticket, BoardRefusal> {
        let key = BoatKey::for_request(req).map_err(|err| {
            // A speed this quantization refuses is a bad request, not a venue
            // that failed at anything.
            BoardRefusal::Placement(Arc::new(PlacementFailure {
                message: format!("{err:#}"),
                venue_fault: false,
            }))
        })?;
        let speed = key.speed();
        loop {
            let wait = {
                let mut boats = self.locked();
                match boats.get_mut(&key) {
                    Some(Slot::Placed(placed)) => {
                        placed.passengers += 1;
                        return Ok(Ticket {
                            yard: Arc::clone(self),
                            boat: Arc::clone(&placed.boat),
                        });
                    }
                    // The one result of the one placement this caller joined.
                    // Returned rather than retried: every failure held here is
                    // deterministic in the key, so a retry pays the whole walk
                    // again to fail identically.
                    Some(Slot::Failed(failure)) => {
                        return Err(BoardRefusal::Placement(Arc::clone(failure)));
                    }
                    Some(Slot::Placing(placing)) => Some(Arc::clone(placing)),
                    None => {
                        boats.insert(key.clone(), Slot::Placing(Arc::new(Semaphore::new(0))));
                        None
                    }
                }
            };
            if let Some(wait) = wait {
                drop(wait.acquire().await);
                continue;
            }
            break;
        }
        let rivers = Arc::clone(&self.rivers);
        let river = req.river.clone();
        let origin_ns = key.origin_ns(self.origin_ns);
        let cursor = tokio::task::spawn_blocking(move || rivers.place_cursor(&river, origin_ns))
            .await
            .map_err(|err| {
                // The blocking task panicked or was cancelled. Nothing the
                // caller asked for produces that, so it is ours.
                Arc::new(PlacementFailure {
                    message: format!("placement task did not complete: {err}"),
                    venue_fault: true,
                })
            })
            .and_then(|result| {
                result.map_err(|refusal| {
                    Arc::new(PlacementFailure {
                        venue_fault: refusal.is_venue_fault(),
                        message: refusal.to_string(),
                    })
                })
            })
            .map_err(BoardRefusal::Placement);
        let boat = cursor.map(|cursor| {
            let funding = match self.rivers.resolve_profile(req.river.symbol()) {
                Ok(profile) => profile
                    .def
                    .class
                    .funding()
                    .filter(|terms| terms.interval_ns != 0)
                    .map(|terms| TapeFunding {
                        symbol: Arc::clone(&profile.def.symbol),
                        terms,
                        rivers: Arc::clone(&self.rivers),
                    }),
                Err(error) => {
                    tracing::warn!(symbol = req.river.symbol(), %error, "boat will publish no funding frames because its profile could not be resolved");
                    None
                }
            };
            // The hull is unbounded even for a named placement: a window's end
            // is each passenger's own delivery cutoff, enforced where frames
            // cross the socket, and the hull winds down when the last such
            // passenger closes and drops its ticket.
            let sim = crate::config::delivery_clock(origin_ns, now_ns(), speed);
            let extremes = Arc::new(crate::extremes::PriceExtremes::default());
            let vol_window = Arc::new(crate::vol_window::VolWindow::starting_at(origin_ns));
            let (tape, worker) = Tape::start(
                cursor,
                TapeSpawn {
                    sim,
                    speed,
                    fanout_depth: self.fanout_depth,
                    fault_tx: self.fault_tx.clone(),
                    extremes: Arc::clone(&extremes),
                    vol_window: Arc::clone(&vol_window),
                    funding,
                },
            );
            let cancel = tape.cancel_flag();
            Arc::new(Boat {
                key: key.clone(),
                sim,
                tape,
                last_swept_ns: AtomicU64::new(origin_ns),
                completed_sweep_passes: AtomicU64::new(0),
                market_readings: crate::fills::MarketReadingCache::for_river(req.river.clone()),
                extremes,
                vol_window,
                worker: Mutex::new(Some(worker)),
                cancel,
            })
        });
        let mut boats = self.locked();
        let placing = match boats.remove(&key) {
            Some(Slot::Placing(placing)) => placing,
            _ => unreachable!("placement owns placeholder"),
        };
        // Closed under the mutex, with the boat already placed or the
        // placeholder already gone, so a joiner that wakes cannot observe the
        // in-flight state it was waiting on.
        placing.close();
        match boat {
            Ok(boat) => {
                boats.insert(
                    key,
                    Slot::Placed(PlacedBoat {
                        boat: Arc::clone(&boat),
                        passengers: 1,
                    }),
                );
                Ok(Ticket {
                    yard: Arc::clone(self),
                    boat,
                })
            }
            Err(BoardRefusal::Placement(failure)) => {
                // Recorded rather than left absent, so a joiner that wakes finds
                // this failure instead of an empty slot to re-place into.
                boats.insert(key, Slot::Failed(Arc::clone(&failure)));
                Err(BoardRefusal::Placement(failure))
            }
        }
    }
    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<BoatKey, Slot>> {
        self.boats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Every placed boat on `symbol`, any speed and any river.
    ///
    /// Test-only now. It used to answer the generator control plane's question,
    /// which was whether this label had a boat and whether an arm might
    /// therefore mutate its water; the fork deleted that question along with
    /// mid-run mutation. What survives is its use as an observation: one label
    /// can carry several boats, and after the fork they need not even be
    /// reading the same river.
    #[cfg(test)]
    pub(crate) fn boats_for_symbol(&self, symbol: &str) -> Vec<Arc<Boat>> {
        self.locked()
            .values()
            .filter_map(|slot| match slot {
                Slot::Placed(placed) if placed.boat.key.river.symbol() == symbol => {
                    Some(Arc::clone(&placed.boat))
                }
                _ => None,
            })
            .collect()
    }
    /// Every symbol carrying a placed boat, sorted and deduplicated.
    ///
    /// Test-only since the control plane stopped consulting the boatyard to
    /// decide what an omitted generator symbol meant. It survives because it is
    /// the observable a placement test asserts on.
    #[cfg(test)]
    pub(crate) fn placed_symbols(&self) -> Vec<String> {
        let mut symbols: Vec<String> = self
            .locked()
            .values()
            .filter_map(|slot| match slot {
                Slot::Placed(placed) => Some(placed.boat.key.river.symbol().to_owned()),
                _ => None,
            })
            .collect();
        symbols.sort();
        symbols.dedup();
        symbols
    }

    pub(crate) fn boats(&self) -> Vec<Arc<Boat>> {
        self.locked()
            .values()
            .filter_map(|slot| match slot {
                Slot::Placed(placed) => Some(Arc::clone(&placed.boat)),
                _ => None,
            })
            .collect()
    }
}

impl Boat {
    pub(crate) fn symbol(&self) -> &str {
        self.key.river.symbol()
    }
    /// A stable identity for this boat that outlives the `Arc`, so a scheduler
    /// keying per-boat state cannot confuse it with a boat on another river or
    /// at another cadence that happened to be allocated at the same address.
    ///
    /// Not an identity across lifetimes: the key is the sharing key, so a boat
    /// placed after this one winds down carries the same key if it is the same
    /// river at the same speed. State keyed by it must therefore be released
    /// when its holder lets go, never left to be reclaimed by a match.
    pub(crate) fn key(&self) -> BoatKey {
        self.key.clone()
    }
}
impl Ticket {
    pub(crate) fn boat(&self) -> &Arc<Boat> {
        &self.boat
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let worker = {
            let mut boats = self.yard.locked();
            let remove = match boats.get_mut(&self.boat.key) {
                Some(Slot::Placed(placed)) if Arc::ptr_eq(&placed.boat, &self.boat) => {
                    placed.passengers -= 1;
                    placed.passengers == 0
                }
                Some(Slot::Placed(_)) => {
                    tracing::error!(
                        symbol = %self.boat.symbol(),
                        "a stale ticket matched a replacement boat; leaving the replacement untouched"
                    );
                    false
                }
                _ => false,
            };
            if !remove {
                return;
            }
            boats.remove(&self.boat.key);
            self.boat.cancel.store(true, Ordering::Release);
            self.boat
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        };
        // Joined off the registry mutex, and off a runtime worker: the worker
        // may be mid-generator-step or waiting on the river mutex, so the join
        // is bounded by a poll slice plus one tick's work. A ticket can also be
        // dropped outside any runtime (process teardown, a non-async test), so
        // fall back to a detached OS thread rather than panicking in a
        // destructor.
        if let Some(worker) = worker {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn_blocking(move || drop(worker.join()));
                }
                Err(_) => {
                    std::thread::spawn(move || drop(worker.join()));
                }
            }
        }
    }
}

impl std::fmt::Debug for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ticket")
            .field("key", &self.boat.key)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yard() -> (Arc<Boatyard>, RiverKey) {
        let rivers = crate::fills::test_rivers();
        let key = rivers.resolve_key(&rivers.resolve_profile("BTCUSDT").unwrap(), None);
        let (fault_tx, _fault_rx) = mpsc::channel();
        (
            Boatyard::new(rivers, 64, fault_tx, crate::source::TAPE_ORIGIN_NS),
            key,
        )
    }

    /// Re-anchored by piece 13: `resolve_profile("SECOND")` still succeeds, but
    /// no longer because `"SECOND"` is configured - resolution is total now, so
    /// this only pins that the two labels key two distinct rivers. It is not a
    /// live guard on configured-only lookup and must not be read as one.
    fn two_symbol_yard() -> (Arc<Boatyard>, RiverKey, RiverKey) {
        let rivers = crate::fills::test_rivers_with_a_second_symbol();
        let first = rivers.resolve_key(&rivers.resolve_profile("BTCUSDT").unwrap(), None);
        let second = rivers.resolve_key(&rivers.resolve_profile("SECOND").unwrap(), None);
        let (fault_tx, _fault_rx) = mpsc::channel();
        (
            Boatyard::new(
                Arc::clone(&rivers),
                64,
                fault_tx,
                crate::source::TAPE_ORIGIN_NS,
            ),
            first,
            second,
        )
    }

    /// A regression fence, not a discovery instrument: with the memo a field of
    /// `Boat`, "two boats do not evict each other" is a type-level fact, and
    /// what this guards is someone re-introducing sharing. The defect it names
    /// - a run-level one-entry memo that two symbols alternately evicted, each
    /// submit then paying a full window walk behind one global mutex - has no
    /// production expression left, so it was reproduced by injection: one
    /// `MarketReadingCache` read four times with an alternating guard field
    /// reports four walks where the two per-boat memos report one each.
    #[tokio::test]
    async fn two_boats_do_not_evict_each_other_s_market_reading() {
        let (yard, first, second) = two_symbol_yard();
        let first = yard.board(&BoardRequest::shared(first, 1.0)).await.unwrap();
        let second = yard
            .board(&BoardRequest::shared(second, 1.0))
            .await
            .unwrap();
        // Not the yard's origin: a 300 s window walk backwards from
        // `TAPE_ORIGIN_NS` has no tape behind it, so the reads would memoize a
        // refusal and the counters would pass while proving nothing about the
        // walk they saved. Hence the `is_some` assertion too.
        let ts = crate::source::TAPE_ORIGIN_NS + 86_400_000_000_000;
        // `None` for the resident window on every read in these memo tests:
        // their subject is the memo's hit/miss split measured through the walk
        // counter, and the boat's own window would serve a miss walk-free or
        // not depending on how far its tape thread happens to have pulled.
        for boat in [first.boat(), second.boat(), first.boat(), second.boat()] {
            assert!(
                boat.market_readings
                    .read(ts, &yard.rivers, 0.005, 200, 100, None)
                    .is_some()
            );
        }
        assert_eq!(first.boat().market_readings.walks(), 1);
        assert_eq!(second.boat().market_readings.walks(), 1);
    }

    /// The half the eviction fence cannot see: a memo that never caches at all
    /// also reports one walk per boat there. Same late-instant discipline, and
    /// the base is floored onto a bucket boundary so `base + 1` is in the same
    /// bucket and `base + one interval` is in the next by construction rather
    /// than by the current value of `TAPE_ORIGIN_NS`.
    #[tokio::test]
    async fn one_boat_pays_for_one_walk_per_bucket() {
        const INTERVAL_MS: u64 = 100;
        const BUCKET_NS: u64 = INTERVAL_MS * 1_000_000;
        let (yard, river) = yard();
        let ticket = yard.board(&BoardRequest::shared(river, 1.0)).await.unwrap();
        let base = (crate::source::TAPE_ORIGIN_NS + 86_400_000_000_000) / BUCKET_NS * BUCKET_NS;
        assert!(
            ticket
                .boat()
                .market_readings
                .read(base, &yard.rivers, 0.005, 200, INTERVAL_MS, None)
                .is_some()
        );
        assert!(
            ticket
                .boat()
                .market_readings
                .read(base + 1, &yard.rivers, 0.005, 200, INTERVAL_MS, None)
                .is_some()
        );
        assert_eq!(ticket.boat().market_readings.walks(), 1);
        assert!(
            ticket
                .boat()
                .market_readings
                .read(
                    base + BUCKET_NS,
                    &yard.rivers,
                    0.005,
                    200,
                    INTERVAL_MS,
                    None
                )
                .is_some()
        );
        assert_eq!(ticket.boat().market_readings.walks(), 2);
    }

    #[tokio::test]
    async fn two_requests_with_one_sharing_key_share_one_boat() {
        let (yard, river) = yard();
        let one = yard
            .board(&BoardRequest::shared(river.clone(), 1.0))
            .await
            .unwrap();
        let two = yard
            .board(&BoardRequest::shared(river, 1.0000001))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(one.boat(), two.boat()));
    }

    /// The glossary's Boat sentence, over named windows: passengers asking for
    /// the same river and the same speed share one boat, whoever they are. An
    /// owner discriminator used to split identical placements into
    /// per-consumer hulls; a hull is a cache and carries no identity, so a
    /// second account replicating the same window reads the same cursor. A
    /// different end shares too, because the end is the passenger's own
    /// cutoff, not part of the clock the hull carries.
    #[tokio::test]
    async fn named_windows_share_across_owners_and_start_at_their_bound() {
        let (yard, river) = yard();
        let start_ns = 2_000_000_000;
        let end_ns = start_ns + 1_000_000_000;
        let first = yard
            .board(&BoardRequest::named(river.clone(), start_ns, end_ns))
            .await
            .unwrap();
        let paired_leg = yard
            .board(&BoardRequest::named(river.clone(), start_ns, end_ns))
            .await
            .unwrap();
        let replication = yard
            .board(&BoardRequest::named(
                river.clone(),
                start_ns,
                end_ns + 500_000_000,
            ))
            .await
            .unwrap();
        let elsewhere = yard
            .board(&BoardRequest::named(river, start_ns + 1, end_ns))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(first.boat(), paired_leg.boat()));
        assert!(
            Arc::ptr_eq(first.boat(), replication.boat()),
            "a later end is the same clock, so it is the same hull"
        );
        assert!(
            !Arc::ptr_eq(first.boat(), elsewhere.boat()),
            "a different start is a different clock epoch, so it is a second hull"
        );
        assert_eq!(first.boat().sim.sim_epoch_ns, start_ns);
        assert_eq!(first.boat().key().placement_start_ns(), Some(start_ns));
    }

    #[tokio::test]
    async fn a_second_speed_on_a_boated_river_is_a_second_boat() {
        let (yard, river) = yard();
        let one = yard
            .board(&BoardRequest::shared(river.clone(), 2.0))
            .await
            .unwrap();
        let two = yard
            .board(&BoardRequest::shared(river, 3.0))
            .await
            .expect("an unserved speed is a cache miss, not a refusal");
        assert!(
            !Arc::ptr_eq(one.boat(), two.boat()),
            "distinct speeds are distinct cursors on the same water"
        );
        assert_eq!(one.boat().key().speed(), 2.0);
        assert_eq!(two.boat().key().speed(), 3.0);
        assert_eq!(one.boat().symbol(), two.boat().symbol());
        assert_eq!(yard.placed_symbols(), ["BTCUSDT"]);
        assert_eq!(yard.boats_for_symbol("BTCUSDT").len(), 2);
        drop(two);
        assert_eq!(
            yard.boats_for_symbol("BTCUSDT").len(),
            1,
            "winding one cadence down leaves the other placed"
        );
    }

    #[tokio::test]
    async fn the_last_passenger_leaving_winds_the_boat_down_and_joins_its_worker() {
        let (yard, river) = yard();
        let ticket = yard.board(&BoardRequest::shared(river, 1.0)).await.unwrap();
        let tape = Arc::clone(&ticket.boat().tape);
        drop(ticket);
        assert!(yard.placed_symbols().is_empty());
        // The join is detached, so the assertion is on the worker's own exit
        // flag rather than on the handle the drop already consumed.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while tape.is_alive() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            !tape.is_alive(),
            "the wound-down boat's worker never exited"
        );
    }

    /// A failed placement is a result, and every later boarder on that key gets
    /// that one result rather than running the failure again.
    ///
    /// The old shape removed the slot entirely, which left nothing for the next
    /// boarder to find: it installed a fresh placeholder and re-ran the same
    /// deterministic failure, paying the whole walk to fail exactly as the first
    /// one had. What the removal was protecting against - a stale `Placing`
    /// placeholder wedging joiners forever - is still closed, because the slot a
    /// boarder finds here is a finished result and never an in-flight one.
    #[tokio::test]
    async fn a_failed_placement_is_shared_rather_than_re_run() {
        let (yard, river) = yard();
        let unplaceable = river.with_unresolvable_bundle();
        let mut failures = Vec::new();
        for _ in 0..2 {
            match yard
                .board(&BoardRequest::shared(unplaceable.clone(), 1.0))
                .await
            {
                Err(BoardRefusal::Placement(failure)) => failures.push(failure),
                _ => panic!("an unresolvable river must refuse placement"),
            }
        }
        assert!(
            Arc::ptr_eq(&failures[0], &failures[1]),
            "the second boarder ran its own placement instead of joining the first one's result"
        );
    }
    /// The placement handoff, which the sequential tests cannot reach: every
    /// boarder but one finds a placeholder, and each of them clones the
    /// handoff under the registry mutex and only then awaits it. A handoff
    /// that forgets a wakeup delivered in that window wedges those boarders
    /// forever, so this test fails as a hang rather than an assertion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_boarders_share_one_placement() {
        let (yard, river) = yard();
        let boarding: Vec<_> = (0..8)
            .map(|_| {
                let yard = Arc::clone(&yard);
                let river = river.clone();
                tokio::spawn(async move {
                    yard.board(&BoardRequest::shared(river, 1.0))
                        .await
                        .expect("a first boarder is never refused")
                })
            })
            .collect();
        let mut tickets = Vec::new();
        for handle in boarding {
            tickets.push(handle.await.unwrap());
        }
        let first = Arc::clone(tickets[0].boat());
        assert!(
            tickets
                .iter()
                .all(|ticket| Arc::ptr_eq(ticket.boat(), &first)),
            "concurrent first boarders placed more than one boat"
        );
        assert_eq!(yard.placed_symbols().len(), 1);
    }

    /// The race a sequential test cannot catch: a joiner arriving exactly as
    /// the last passenger leaves must either place a fresh boat or board a
    /// live one, never a boat whose worker is winding down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_joiner_racing_the_last_departure_never_boards_a_wound_down_boat() {
        for _ in 0..16 {
            let (yard, river) = yard();
            let sitting = yard
                .board(&BoardRequest::shared(river.clone(), 1.0))
                .await
                .unwrap();
            let joining = {
                let yard = Arc::clone(&yard);
                tokio::spawn(async move {
                    let ticket = yard
                        .board(&BoardRequest::shared(river, 1.0))
                        .await
                        .expect("a same-speed join is never refused");
                    assert!(
                        ticket.boat().tape.is_alive(),
                        "boarded a boat whose worker had stopped"
                    );
                    assert!(
                        !ticket.boat().cancel.load(Ordering::Acquire),
                        "boarded a cancelled boat"
                    );
                })
            };
            drop(sitting);
            joining.await.unwrap();
        }
    }
}
