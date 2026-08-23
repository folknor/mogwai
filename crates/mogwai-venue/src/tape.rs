// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! One boat-owned paced tape and its bounded broadcast fanout.

use crate::config::now_ns;
use mogwai_data::TickFault;
use mogwai_protocol::SimClock;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::broadcast;

#[derive(Clone)]
pub(crate) struct TapeFrame {
    pub(crate) payload: Arc<str>,
    pub(crate) ts_event: u64,
}
pub(crate) struct Tape {
    tx: broadcast::Sender<TapeFrame>,
    last_quote: Mutex<Option<TapeFrame>>,
    cancel: Arc<AtomicBool>,
    alive: AtomicBool,
    fault: Mutex<Option<TickFault>>,
}
pub(crate) struct TapeSpawn {
    pub(crate) sim: SimClock,
    pub(crate) speed: f64,
    pub(crate) fanout_depth: usize,
    pub(crate) fault_tx: mpsc::Sender<TickFault>,
    /// Where this thread records the high and low of the span since the sweeper
    /// last looked. See `crate::extremes`: it is what gives peak equity and a
    /// trailing stop tick resolution without evaluating either here.
    pub(crate) extremes: Arc<crate::extremes::PriceExtremes>,
    /// The resident trailing window the submit path's market reading is served
    /// from. Folded at pull time, before the pacing sleep, because the pull is
    /// what proves every earlier instant complete; see `crate::vol_window` for
    /// why that leaks no lookahead.
    pub(crate) vol_window: Arc<crate::vol_window::VolWindow>,
}
impl Tape {
    pub(crate) fn start(
        mut cursor: Box<dyn mogwai_data::TickSource + Send>,
        spawn: TapeSpawn,
    ) -> (Arc<Self>, thread::JoinHandle<()>) {
        let (tx, _) = broadcast::channel(spawn.fanout_depth);
        let tape = Arc::new(Self {
            tx,
            last_quote: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
            fault: Mutex::new(None),
        });
        let worker = Arc::clone(&tape);
        let handle = thread::spawn(move || {
            let wall_anchor = now_ns();
            let instant_anchor = Instant::now();
            // This thread's own running extremes. Stack-local on purpose: the
            // comparison is per tick and must not take a lock, so the shared
            // slot is written only when an extreme actually moves.
            let mut span = crate::extremes::SpanWriter::default();
            // Ticks published since this thread last offered the host a
            // scheduling point. Only an unpaced tape uses it; a paced one sleeps
            // to a wall deadline and yields by doing so.
            let mut since_yield: u32 = 0;
            while !worker.cancel.load(Ordering::Relaxed) {
                let Some(tick) = cursor.next_tick() else {
                    if let Some(fault) = cursor.fault() {
                        *worker
                            .fault
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fault);
                        tracing::error!(?fault, "tape source faulted");
                        let _fault_receiver_gone = spawn.fault_tx.send(fault);
                    }
                    spawn.vol_window.close();
                    break;
                };
                match &tick {
                    mogwai_data::TickEvent::Trade(trade) => {
                        spawn.vol_window.fold(trade.ts_event, Some(trade.price));
                    }
                    mogwai_data::TickEvent::Quote(quote) => {
                        spawn.vol_window.fold(quote.ts_event, None);
                    }
                }
                pace(
                    &worker,
                    &spawn,
                    tick.ts_event(),
                    wall_anchor,
                    instant_anchor,
                    &mut since_yield,
                );
                if worker.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let ts_event = tick.ts_event();
                // Trades only, and that is the same rule the mark reads follow:
                // a mark is a last-print read, so an extreme drawn from quotes
                // would be an extreme no fill or valuation could ever have been
                // taken at. Recorded before publication, so a reader that takes
                // the span after seeing a frame cannot miss that frame's price.
                if let mogwai_data::TickEvent::Trade(trade) = &tick {
                    spawn.extremes.record(&mut span, trade.price, ts_event);
                }
                let is_quote = matches!(tick, mogwai_data::TickEvent::Quote(_));
                let event = match tick {
                    mogwai_data::TickEvent::Trade(trade) => {
                        mogwai_protocol::VenueMessage::Trade(trade)
                    }
                    mogwai_data::TickEvent::Quote(quote) => {
                        mogwai_protocol::VenueMessage::Quote(quote)
                    }
                };
                let Ok(payload) = serde_json::to_string(&event) else {
                    break;
                };
                let frame = TapeFrame {
                    payload: Arc::from(payload),
                    ts_event,
                };
                worker.publish(frame, is_quote);
            }
            worker.alive.store(false, Ordering::Release);
        });
        (tape, handle)
    }
    /// Subscribe, plus the last quote this boat published if it has published
    /// one. The option is the contract, not an implementation detail: a socket
    /// binding between a boat's first trade and its first quote gets `None` and
    /// therefore sees a trade as its first market frame. Callers must not turn
    /// this into a snapshot-first promise - there is nothing to snapshot yet,
    /// and the tape's own first quote is immediately behind it.
    pub(crate) fn subscribe_with_snapshot(
        &self,
    ) -> (broadcast::Receiver<TapeFrame>, Option<TapeFrame>) {
        self.subscribe_with_snapshot_inner(|| {})
    }
    fn subscribe_with_snapshot_inner(
        &self,
        after_subscribe: impl FnOnce(),
    ) -> (broadcast::Receiver<TapeFrame>, Option<TapeFrame>) {
        let last = self
            .last_quote
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let receiver = self.tx.subscribe();
        after_subscribe();
        (receiver, last.clone())
    }
    fn publish(&self, frame: TapeFrame, is_quote: bool) {
        if is_quote {
            let mut last = self
                .last_quote
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *last = Some(frame.clone());
            drop(self.tx.send(frame));
        } else {
            drop(self.tx.send(frame));
        }
    }
    pub(crate) fn fault(&self) -> Option<TickFault> {
        *self
            .fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }
    /// False once the pacing thread has left its loop. The one observable a
    /// wind-down test can assert on without reaching into the worker handle
    /// the ticket's drop already took.
    #[cfg(test)]
    pub(crate) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
}

/// Maximum wall time one slice of a pacing sleep runs before the tape thread
/// re-checks its cancel flag, so a stopping venue is not parked for a whole
/// inter-tick gap.
const TAPE_SLEEP_POLL: Duration = Duration::from_millis(20);

/// Pace one tick against the run clock, or, for a firehose, yield to the host
/// often enough that one river cannot capture a core.
fn pace(
    tape: &Tape,
    spawn: &TapeSpawn,
    ts: u64,
    wall_anchor: u64,
    instant_anchor: Instant,
    since_yield: &mut u32,
) {
    if spawn.speed == 0.0 {
        *since_yield += 1;
        if *since_yield >= UNPACED_YIELD_TICKS {
            *since_yield = 0;
            thread::yield_now();
        }
        return;
    }
    sleep_until_wall_cancellable(tape, spawn.sim.wall_ns(ts), wall_anchor, instant_anchor);
}

/// How many ticks an unpaced tape publishes between scheduling points.
///
/// Receiver-blind, and that is the point. An unpaced tape used to park while the
/// ring was more than half full, which made the slowest subscriber the thing
/// that decided when a shared boat published. That was tolerable only while a
/// lagging passenger was ejected: one dead consumer cost one stall and then
/// left. A declared hole is no longer fatal, so the ejection is gone - and a
/// passenger whose sustainable read rate is below the publish rate would then
/// have parked the boat every stall interval for the whole run, imposing its own
/// slowness on every other account sharing that hull. That is exactly the
/// non-interference passengers of different accounts are owed.
///
/// What replaces it depends on production work alone, never on ring occupancy
/// or the slowest cursor: a fixed cadence bounds how long one unpaced river can
/// hold a core, without letting any consumer influence the tape. The ring is now
/// the only delivery slack a passenger has, and overrunning it is declared
/// rather than waited out.
const UNPACED_YIELD_TICKS: u32 = 64;

fn sleep_until_wall_cancellable(
    tape: &Tape,
    target_wall_ns: u64,
    wall_anchor: u64,
    instant_anchor: Instant,
) {
    let due = instant_anchor + Duration::from_nanos(target_wall_ns.saturating_sub(wall_anchor));
    loop {
        if tape.cancel.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if now >= due {
            return;
        }
        thread::sleep((due - now).min(TAPE_SLEEP_POLL));
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn a_concurrent_publish_cannot_advance_the_snapshot_past_a_queued_frame() {
        let (tx, _) = broadcast::channel(16);
        let tape = Arc::new(Tape {
            tx,
            last_quote: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
            fault: Mutex::new(None),
        });
        tape.publish(
            TapeFrame {
                payload: Arc::from("q1"),
                ts_event: 1,
            },
            true,
        );
        let subscribed = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let subscriber_tape = Arc::clone(&tape);
        let subscriber_subscribed = Arc::clone(&subscribed);
        let subscriber_release = Arc::clone(&release);
        let subscriber = thread::spawn(move || {
            subscriber_tape.subscribe_with_snapshot_inner(|| {
                subscriber_subscribed.wait();
                subscriber_release.wait();
            })
        });
        subscribed.wait();
        let publisher_started = Arc::new(std::sync::Barrier::new(2));
        let worker = Arc::clone(&tape);
        let worker_started = Arc::clone(&publisher_started);
        let publisher = thread::spawn(move || {
            worker_started.wait();
            worker.publish(
                TapeFrame {
                    payload: Arc::from("q2"),
                    ts_event: 2,
                },
                true,
            );
        });
        publisher_started.wait();
        release.wait();
        let (mut receiver, snapshot) = subscriber.join().unwrap();
        publisher.join().unwrap();
        let snapshot = snapshot.unwrap();
        let queued = receiver.try_recv().expect("concurrent quote was queued");
        assert_eq!(snapshot.ts_event, 1);
        assert_eq!(queued.ts_event, 2);
    }

    #[test]
    fn a_subscriber_sees_a_bbo_before_its_first_trade() {
        let (tx, _) = broadcast::channel(16);
        let tape = Tape {
            tx,
            last_quote: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
            fault: Mutex::new(None),
        };
        tape.publish(
            TapeFrame {
                payload: Arc::from("quote"),
                ts_event: 10,
            },
            true,
        );
        let (mut receiver, snapshot) = tape.subscribe_with_snapshot();
        tape.publish(
            TapeFrame {
                payload: Arc::from("trade"),
                ts_event: 11,
            },
            false,
        );
        let snapshot = snapshot.expect("current BBO snapshot");
        let trade = receiver.try_recv().expect("queued trade");
        assert_eq!(snapshot.payload.as_ref(), "quote");
        assert_eq!(trade.payload.as_ref(), "trade");
        assert!(snapshot.ts_event <= trade.ts_event);
    }
}
