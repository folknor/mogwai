// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! On-demand paced boats, shared by passengers asking for the same river and speed.

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
    tape::{Tape, TapeSpawn},
};

/// The largest speed the sharing key can carry, in micro-multiples: one
/// million times real time, which is already far past any paced use. The bound
/// exists to keep the quantization honest rather than to express a policy.
const MAX_SPEED_MICROS: u64 = 1_000_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BoatKey {
    river: RiverKey,
    speed_micros: u64,
}

pub(crate) struct Boat {
    key: BoatKey,
    /// The quantized speed this boat was placed at. Distinct from `sim.speed`,
    /// which maps a zero pacing speed onto 1.0 so the affine clock stays
    /// invertible; a refusal must name what a joiner has to ask for.
    pub(crate) speed: f64,
    pub(crate) sim: SimClock,
    pub(crate) tape: Arc<Tape>,
    pub(crate) published_ns: Arc<AtomicU64>,
    pub(crate) last_swept_ns: AtomicU64,
    worker: Mutex<Option<JoinHandle<()>>>,
    cancel: Arc<AtomicBool>,
}

/// A placement in flight, and the handoff every other joiner waits on. A
/// `Semaphore` and not a `Notify`: a joiner clones this while holding the
/// registry mutex and only then awaits, so a wakeup delivered in that window
/// must be REMEMBERED. `notify_waiters` forgets it and wedges the joiner
/// forever; a closed semaphore fails every later acquire immediately.
enum Slot {
    Placing(Arc<Semaphore>),
    Seated(Seat),
}
struct Seat {
    boat: Arc<Boat>,
    passengers: u32,
}

pub(crate) struct Boatyard {
    rivers: Arc<Rivers>,
    boats: Mutex<HashMap<RiverKey, Slot>>,
    fanout_depth: usize,
    zero_speed_stall_ms: u64,
    fault_tx: mpsc::Sender<mogwai_data::TickFault>,
    origin_ns: u64,
}

pub(crate) struct BoardRequest {
    pub(crate) river: RiverKey,
    pub(crate) speed: f64,
}
#[derive(Debug)]
pub(crate) enum BoardRefusal {
    SpeedInUse { sitting_speed: f64 },
    Placement(anyhow::Error),
}

pub(crate) struct Ticket {
    yard: Arc<Boatyard>,
    boat: Arc<Boat>,
}

impl Boatyard {
    pub(crate) fn new(
        rivers: Arc<Rivers>,
        fanout_depth: usize,
        zero_speed_stall_ms: u64,
        fault_tx: mpsc::Sender<mogwai_data::TickFault>,
        origin_ns: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            rivers,
            boats: Mutex::new(HashMap::new()),
            fanout_depth,
            zero_speed_stall_ms,
            fault_tx,
            origin_ns,
        })
    }
    pub(crate) async fn board(
        self: &Arc<Self>,
        req: &BoardRequest,
    ) -> Result<Ticket, BoardRefusal> {
        if !req.speed.is_finite() || req.speed < 0.0 {
            return Err(BoardRefusal::Placement(anyhow::anyhow!(
                "speed must be finite and non-negative"
            )));
        }
        // Quantized to micro-multiples so the key is `Hash` and `Eq` and two
        // clients writing `100` and `100.0000001` share a boat. Bounded first:
        // a float cast saturates rather than wrapping, so an absurd speed would
        // otherwise be silently REPLACED by a different one and then paced at
        // it.
        if req.speed * 1_000_000.0 > MAX_SPEED_MICROS as f64 {
            return Err(BoardRefusal::Placement(anyhow::anyhow!(
                "speed {} is beyond the quantization range; the maximum is {}",
                req.speed,
                MAX_SPEED_MICROS / 1_000_000
            )));
        }
        let speed_micros = (req.speed * 1_000_000.0).round() as u64;
        let speed = speed_micros as f64 / 1_000_000.0;
        let key = BoatKey {
            river: req.river.clone(),
            speed_micros,
        };
        loop {
            let wait = {
                let mut boats = self.locked();
                match boats.get_mut(&req.river) {
                    Some(Slot::Seated(seat)) if seat.boat.key == key => {
                        seat.passengers += 1;
                        return Ok(Ticket {
                            yard: Arc::clone(self),
                            boat: Arc::clone(&seat.boat),
                        });
                    }
                    Some(Slot::Seated(seat)) => {
                        return Err(BoardRefusal::SpeedInUse {
                            sitting_speed: seat.boat.speed,
                        });
                    }
                    Some(Slot::Placing(placing)) => Some(Arc::clone(placing)),
                    None => {
                        boats.insert(
                            req.river.clone(),
                            Slot::Placing(Arc::new(Semaphore::new(0))),
                        );
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
        let origin_ns = self.origin_ns;
        let cursor = tokio::task::spawn_blocking(move || rivers.place_cursor(&river, origin_ns))
            .await
            .map_err(|err| BoardRefusal::Placement(anyhow::anyhow!(err)))
            .and_then(|result| result.map_err(BoardRefusal::Placement));
        let boat = cursor.map(|cursor| {
            let published_ns = Arc::new(AtomicU64::new(self.origin_ns));
            let sim = SimClock {
                sim_epoch_ns: self.origin_ns,
                wall_anchor_ns: now_ns(),
                speed: if speed == 0.0 { 1.0 } else { speed },
            };
            let (tape, worker) = Tape::start(
                cursor,
                TapeSpawn {
                    sim,
                    speed,
                    fanout_depth: self.fanout_depth,
                    zero_speed_stall_ms: self.zero_speed_stall_ms,
                    fault_tx: self.fault_tx.clone(),
                    published_ns: Arc::clone(&published_ns),
                },
            );
            let cancel = tape.cancel_flag();
            Arc::new(Boat {
                key,
                speed,
                sim,
                tape,
                published_ns,
                last_swept_ns: AtomicU64::new(self.origin_ns),
                worker: Mutex::new(Some(worker)),
                cancel,
            })
        });
        let mut boats = self.locked();
        let placing = match boats.remove(&req.river) {
            Some(Slot::Placing(placing)) => placing,
            _ => unreachable!("placement owns placeholder"),
        };
        // Closed under the mutex, with the seat already published or the
        // placeholder already gone, so a joiner that wakes cannot observe the
        // in-flight state it was waiting on.
        placing.close();
        match boat {
            Ok(boat) => {
                boats.insert(
                    req.river.clone(),
                    Slot::Seated(Seat {
                        boat: Arc::clone(&boat),
                        passengers: 1,
                    }),
                );
                Ok(Ticket {
                    yard: Arc::clone(self),
                    boat,
                })
            }
            Err(err) => Err(err),
        }
    }
    fn locked(&self) -> std::sync::MutexGuard<'_, HashMap<RiverKey, Slot>> {
        self.boats
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn boat_for_symbol(&self, symbol: &str) -> Option<Arc<Boat>> {
        self.locked().values().find_map(|slot| match slot {
            Slot::Seated(seat) if seat.boat.key.river.symbol() == symbol => {
                Some(Arc::clone(&seat.boat))
            }
            _ => None,
        })
    }
    pub(crate) fn seated_symbols(&self) -> Vec<String> {
        self.locked()
            .values()
            .filter_map(|slot| match slot {
                Slot::Seated(seat) => Some(seat.boat.key.river.symbol().to_owned()),
                _ => None,
            })
            .collect()
    }
    pub(crate) fn boats(&self) -> Vec<Arc<Boat>> {
        self.locked()
            .values()
            .filter_map(|slot| match slot {
                Slot::Seated(seat) => Some(Arc::clone(&seat.boat)),
                _ => None,
            })
            .collect()
    }
}

impl Boat {
    pub(crate) fn symbol(&self) -> &str {
        self.key.river.symbol()
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
            let remove = match boats.get_mut(&self.boat.key.river) {
                Some(Slot::Seated(seat)) => {
                    seat.passengers -= 1;
                    seat.passengers == 0
                }
                _ => false,
            };
            if !remove {
                return;
            }
            boats.remove(&self.boat.key.river);
            self.boat.cancel.store(true, Ordering::Release);
            self.boat
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        };
        // Joined OFF the registry mutex, and off a runtime worker: the worker
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
        let key = rivers.resolve_key(rivers.resolve_profile("BTCUSDT").unwrap());
        let (fault_tx, _fault_rx) = mpsc::channel();
        (
            Boatyard::new(rivers, 64, 10, fault_tx, crate::source::TAPE_ORIGIN_NS),
            key,
        )
    }

    #[tokio::test]
    async fn two_requests_with_one_sharing_key_share_one_boat() {
        let (yard, river) = yard();
        let one = yard
            .board(&BoardRequest {
                river: river.clone(),
                speed: 1.0,
            })
            .await
            .unwrap();
        let two = yard
            .board(&BoardRequest {
                river,
                speed: 1.0000001,
            })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(one.boat(), two.boat()));
    }

    #[tokio::test]
    async fn a_second_speed_on_a_boated_river_is_refused_naming_the_sitting_speed() {
        let (yard, river) = yard();
        let _one = yard
            .board(&BoardRequest {
                river: river.clone(),
                speed: 2.0,
            })
            .await
            .unwrap();
        match yard.board(&BoardRequest { river, speed: 3.0 }).await {
            Err(BoardRefusal::SpeedInUse { sitting_speed }) => assert_eq!(sitting_speed, 2.0),
            _ => panic!("second speed was not refused"),
        }
    }

    #[tokio::test]
    async fn the_last_passenger_leaving_winds_the_boat_down_and_joins_its_worker() {
        let (yard, river) = yard();
        let ticket = yard
            .board(&BoardRequest { river, speed: 1.0 })
            .await
            .unwrap();
        let tape = Arc::clone(&ticket.boat().tape);
        drop(ticket);
        assert!(yard.seated_symbols().is_empty());
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

    #[tokio::test]
    async fn a_failed_placement_leaves_no_placeholder_for_the_next_joiner() {
        let (yard, river) = yard();
        let unplaceable = river.with_unresolvable_bundle();
        for _ in 0..2 {
            match yard
                .board(&BoardRequest {
                    river: unplaceable.clone(),
                    speed: 1.0,
                })
                .await
            {
                Err(BoardRefusal::Placement(_)) => {}
                _ => panic!("an unresolvable river must refuse placement"),
            }
        }
        assert!(
            yard.locked().is_empty(),
            "a failed placement left a placeholder behind"
        );
    }

    /// The placement HANDOFF, which the sequential tests cannot reach: every
    /// boarder but one finds a placeholder, and each of them clones the
    /// handoff under the registry mutex and only then awaits it. A handoff
    /// that forgets a wakeup delivered in that window wedges those boarders
    /// forever, so this test fails as a HANG rather than an assertion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_boarders_share_one_placement() {
        let (yard, river) = yard();
        let boarding: Vec<_> = (0..8)
            .map(|_| {
                let yard = Arc::clone(&yard);
                let river = river.clone();
                tokio::spawn(async move {
                    yard.board(&BoardRequest { river, speed: 1.0 })
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
        assert_eq!(yard.seated_symbols().len(), 1);
    }

    /// The race a sequential test cannot catch: a joiner arriving exactly as
    /// the last passenger leaves must either place a fresh boat or board a
    /// live one, never a boat whose worker is winding down.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_joiner_racing_the_last_departure_never_boards_a_wound_down_boat() {
        for _ in 0..16 {
            let (yard, river) = yard();
            let sitting = yard
                .board(&BoardRequest {
                    river: river.clone(),
                    speed: 1.0,
                })
                .await
                .unwrap();
            let joining = {
                let yard = Arc::clone(&yard);
                tokio::spawn(async move {
                    let ticket = yard
                        .board(&BoardRequest { river, speed: 1.0 })
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
