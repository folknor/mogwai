// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The one process-owned paced tape and its bounded broadcast fanout.

use crate::{config::now_ns, source};
use mogwai_protocol::SimClock;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::broadcast;

#[derive(Clone)]
pub(crate) struct TapeFrame {
    pub(crate) payload: Arc<str>,
}
pub(crate) struct Tape {
    tx: broadcast::Sender<TapeFrame>,
    cancel: Arc<AtomicBool>,
}
pub(crate) struct TapeSpawn {
    pub(crate) profiles: Arc<source::InstrumentProfiles>,
    pub(crate) sim: SimClock,
    pub(crate) speed: f64,
    pub(crate) fanout_depth: usize,
    pub(crate) zero_speed_stall_ms: u64,
}
impl Tape {
    pub(crate) fn start(symbol: String, spawn: TapeSpawn) -> Arc<Self> {
        let (tx, _) = broadcast::channel(spawn.fanout_depth);
        let tape = Arc::new(Self {
            tx,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        let worker = Arc::clone(&tape);
        thread::spawn(move || {
            let wall_anchor = now_ns();
            let instant_anchor = Instant::now();
            let now = spawn.sim.sim_ns(wall_anchor);
            let Some(mut source) = source::build_live_source(&symbol, &spawn.profiles, now) else {
                return;
            };
            while !worker.cancel.load(Ordering::Relaxed) {
                let Some(tick) = source.next_tick() else {
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
                drop(worker.tx.send(TapeFrame {
                    payload: Arc::from(payload),
                }));
            }
        });
        tape
    }
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<TapeFrame> {
        self.tx.subscribe()
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
