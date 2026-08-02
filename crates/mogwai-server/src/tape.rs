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
    pub(crate) gap_cap_ms: u64,
    pub(crate) fanout_depth: usize,
    pub(crate) zero_speed_stall_ms: u64,
}
impl Tape {
    pub(crate) fn start(symbol: String, data_origin_ns: u64, spawn: TapeSpawn) -> Arc<Self> {
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
            let Some(mut source) =
                source::build_live_source(&symbol, &spawn.profiles, data_origin_ns, now)
            else {
                return;
            };
            let mut previous = None;
            let mut deadline = None;
            while !worker.cancel.load(Ordering::Relaxed) {
                let Some(tick) = source.next_tick() else {
                    break;
                };
                pace(
                    &worker,
                    &spawn,
                    tick.ts_event(),
                    &mut previous,
                    &mut deadline,
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

/// Pace one tick: deadline pacing against the simulated clock in accelerated
/// mode, chained gap pacing capped at `gap_cap_ms` in identity mode, and no
/// pacing at all at `speed == 0.0`, whose throttle is [`await_headroom`].
///
/// Chained deadlines rather than a fresh relative sleep per tick: a per-tick
/// relative sleep never accounts for the wall time spent generating and
/// broadcasting the previous tick, so realized spacing would progressively lag
/// over a long run.
fn pace(
    tape: &Tape,
    spawn: &TapeSpawn,
    ts: u64,
    previous: &mut Option<u64>,
    deadline: &mut Option<u64>,
    wall_anchor: u64,
    instant_anchor: Instant,
) {
    let target = if !spawn.sim.is_identity() {
        Some(spawn.sim.wall_ns(ts))
    } else if spawn.speed > 0.0 {
        // Nanosecond resolution: dividing down to whole milliseconds would
        // collapse the micros-apart bursts the generator emits into zero-delay
        // sends, so the realized timeline would not track original / speed.
        let wait = previous.map(|p| {
            let mut wait_ns = (ts.saturating_sub(p) as f64 / spawn.speed) as u64;
            if spawn.gap_cap_ms > 0 {
                wait_ns = wait_ns.min(spawn.gap_cap_ms.saturating_mul(1_000_000));
            }
            wait_ns
        });
        *previous = Some(ts);
        wait.map(|wait_ns| {
            let due = deadline.unwrap_or(wall_anchor).saturating_add(wait_ns);
            *deadline = Some(due);
            due
        })
    } else {
        await_headroom(tape, spawn);
        None
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
