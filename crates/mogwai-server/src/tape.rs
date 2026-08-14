// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The one process-owned paced tape and its bounded broadcast fanout.

use crate::{config::now_ns, source};
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
    symbol: String,
    profiles: Arc<source::InstrumentProfiles>,
    fault: Mutex<Option<TickFault>>,
}
pub(crate) struct TapeSpawn {
    pub(crate) profiles: Arc<source::InstrumentProfiles>,
    pub(crate) sim: SimClock,
    pub(crate) speed: f64,
    pub(crate) fanout_depth: usize,
    pub(crate) zero_speed_stall_ms: u64,
    pub(crate) fault_tx: mpsc::Sender<TickFault>,
}
impl Tape {
    pub(crate) fn start(symbol: String, spawn: TapeSpawn) -> Arc<Self> {
        assert!(
            source::activate_live(&symbol, &spawn.profiles),
            "validated run symbol must own a tape index"
        );
        let (tx, _) = broadcast::channel(spawn.fanout_depth);
        let tape = Arc::new(Self {
            tx,
            last_quote: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
            symbol: symbol.clone(),
            profiles: Arc::clone(&spawn.profiles),
            fault: Mutex::new(None),
        });
        let worker = Arc::clone(&tape);
        thread::spawn(move || {
            let wall_anchor = now_ns();
            let instant_anchor = Instant::now();
            let now = spawn.sim.sim_ns(wall_anchor);
            if source::build_history_source(&symbol, Some(now), &spawn.profiles).is_none() {
                tracing::error!(symbol, "tape source could not be positioned");
                worker.alive.store(false, Ordering::Release);
                return;
            }
            while !worker.cancel.load(Ordering::Relaxed) {
                let Some((tick, fault)) = source::next_live_tick(&symbol, &spawn.profiles) else {
                    tracing::error!(symbol, "canonical tape source disappeared");
                    break;
                };
                let Some(tick) = tick else {
                    if let Some(fault) = fault {
                        *worker
                            .fault
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fault);
                        tracing::error!(?fault, "tape source faulted");
                        let _fault_receiver_gone = spawn.fault_tx.send(fault);
                    }
                    break;
                };
                pace(
                    &worker,
                    &spawn,
                    tick.ts_event(),
                    wall_anchor,
                    instant_anchor,
                );
                if worker.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let ts_event = tick.ts_event();
                let is_quote = matches!(tick, mogwai_data::TickEvent::Quote(_));
                let event = match tick {
                    mogwai_data::TickEvent::Trade(trade) => {
                        mogwai_protocol::ServerMessage::Trade(trade)
                    }
                    mogwai_data::TickEvent::Quote(quote) => {
                        mogwai_protocol::ServerMessage::Quote(quote)
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
        tape
    }
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
    pub(crate) fn arm_flow_surge(
        &self,
        start_ns: u64,
        duration_ms: u64,
        rate_mult: f64,
        children_mult: f64,
    ) -> Result<(), ()> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(());
        }
        source::arm_flow_surge(
            &self.symbol,
            &self.profiles,
            start_ns,
            duration_ms,
            rate_mult,
            children_mult,
        )
        .then_some(())
        .ok_or(())
    }
    pub(crate) fn clear_flow_surge(&self) -> Result<(), ()> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(());
        }
        source::clear_flow_surge(&self.symbol, &self.profiles)
            .then_some(())
            .ok_or(())
    }
    pub(crate) fn fault(&self) -> Option<TickFault> {
        *self
            .fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    #[cfg(test)]
    pub(crate) fn stop_for_test(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Maximum wall time one slice of a pacing sleep runs before the tape thread
/// re-checks its cancel flag, so a stopping venue is not parked for a whole
/// inter-tick gap.
const TAPE_SLEEP_POLL: Duration = Duration::from_millis(20);

/// Poll slice of the `speed == 0.0` headroom park.
const TAPE_HEADROOM_POLL: Duration = Duration::from_millis(5);

/// Pace one tick against the run clock, or skip delivery pacing for a firehose.
fn pace(tape: &Tape, spawn: &TapeSpawn, ts: u64, wall_anchor: u64, instant_anchor: Instant) {
    let target = if spawn.speed == 0.0 {
        await_headroom(tape, spawn);
        None
    } else {
        Some(spawn.sim.wall_ns(ts))
    };
    if let Some(target) = target {
        sleep_until_wall_cancellable(tape, target, wall_anchor, instant_anchor);
    }
}

/// Park an unpaced (`speed == 0.0`) tape while the ring is more than half full,
/// so it does not overwrite frames its readers are actively draining. A
/// subscriber that is not merely slow but STOPPED never drains, so the park
/// times out at `zero_speed_stall_ms` and the tape resumes: one dead client
/// costs one stall and is then ejected by the ring as `FeedLagged`, rather than
/// stalling the whole run forever.
fn await_headroom(tape: &Tape, spawn: &TapeSpawn) {
    if tape.tx.receiver_count() == 0 {
        return;
    }
    let lead = (spawn.fanout_depth / 2).max(1);
    let deadline = Instant::now() + Duration::from_millis(spawn.zero_speed_stall_ms);
    loop {
        if tape.cancel.load(Ordering::Relaxed) || tape.tx.receiver_count() == 0 {
            return;
        }
        if tape.tx.len() <= lead {
            return;
        }
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        thread::sleep((deadline - now).min(TAPE_HEADROOM_POLL));
    }
}

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
            symbol: "BTCUSDT".to_owned(),
            profiles: Arc::new(source::InstrumentProfiles::defaults()),
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
            symbol: "BTCUSDT".to_owned(),
            profiles: Arc::new(source::InstrumentProfiles::defaults()),
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

    #[test]
    fn a_stopped_tape_refuses_flow_controls() {
        let (tx, _) = broadcast::channel(1);
        let tape = Tape {
            tx,
            last_quote: Mutex::new(None),
            cancel: Arc::new(AtomicBool::new(true)),
            alive: AtomicBool::new(false),
            symbol: "BTCUSDT".to_owned(),
            profiles: Arc::new(source::InstrumentProfiles::defaults()),
            fault: Mutex::new(None),
        };
        assert!(tape.arm_flow_surge(0, 1, 2.0, 2.0).is_err());
        assert!(tape.clear_flow_surge().is_err());
    }
}
