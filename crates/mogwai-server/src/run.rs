// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! State owned by one venue process.  There are deliberately no keys or
//! lookup methods here: a process is one instrument, one ledger, and one tape.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use mogwai_engine::{Engine, EngineConfig};
use mogwai_protocol::{CommandClass, InstrumentDef, SimClock};
use rust_decimal::Decimal;
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::{
    admission::ExecLanes,
    source,
    tape::{Tape, TapeSpawn},
};

pub(crate) struct Run {
    /// The single instrument. Its CONTENT is owned by
    /// `notes/problem-instrument-model.md`; this struct fixes only that there
    /// is exactly one of it.
    pub(crate) instrument: InstrumentDef,
    pub(crate) engine: AsyncMutex<Engine>,
    pub(crate) tape: Arc<Tape>,
    /// The run clock. Owned HERE rather than beside the router state: a run has
    /// one clock, and a second copy in the HTTP state is a second thing that
    /// could be re-anchored independently of the tape it dates.
    pub(crate) sim: SimClock,
    /// Sim instant at which the run began serving live, set after warmup was
    /// materialized. The epoch every duration is measured from.
    pub(crate) started_ns: u64,
    /// Sim instant at which the run stops itself, or `None` for indefinite.
    /// Equals `started_ns + run_duration_ns`.
    pub(crate) deadline_ns: Option<u64>,
    /// Sim span of history generated eagerly at boot and held for the life of
    /// the process. The history floor is `started_ns - warmup_ns`.
    pub(crate) warmup_ns: u64,
    pub(crate) delay_ms: AtomicU64,
    pub(crate) submit_act_ms: AtomicU64,
    pub(crate) modify_act_ms: AtomicU64,
    pub(crate) cancel_act_ms: AtomicU64,
    pub(crate) submit_ack_ms: AtomicU64,
    pub(crate) modify_ack_ms: AtomicU64,
    pub(crate) cancel_ack_ms: AtomicU64,
    pub(crate) dark_until_ns: AtomicU64,
    pub(crate) stall_until_ns: AtomicU64,
    complete_tx: watch::Sender<Option<(u64, u64)>>,
    /// Every live connection's outbound lanes, so venue-ORIGINATED output - a
    /// penetration fill nobody commanded - reaches all of them. This is what
    /// `AccountSlot::session_lanes` was, moved to the one thing that now owns
    /// execution: with a single ledger there is exactly one broadcast target,
    /// and a fill booked into it is a fact about the run, not about whichever
    /// socket happened to submit the order.
    lanes: Mutex<Vec<(u64, ExecLanes)>>,
    next_lane_id: AtomicU64,
}

impl Run {
    #[expect(
        clippy::too_many_arguments,
        reason = "the boot-only values are explicit so the single-run ownership is visible at construction"
    )]
    pub(crate) fn new(
        instrument: InstrumentDef,
        profiles: Arc<source::InstrumentProfiles>,
        balances: std::collections::HashMap<String, Decimal>,
        penetration_ticks: u32,
        sim: SimClock,
        started_ns: u64,
        warmup_ns: u64,
        run_duration_ns: Option<u64>,
        speed: f64,
        gap_cap_ms: u64,
        fanout_depth: usize,
        zero_speed_stall_ms: u64,
    ) -> Arc<Self> {
        let symbol = instrument.symbol.clone();
        let data_origin_ns = started_ns.saturating_sub(warmup_ns);
        let tape = Tape::start(
            symbol,
            data_origin_ns,
            TapeSpawn {
                profiles,
                sim,
                speed,
                gap_cap_ms,
                fanout_depth,
                zero_speed_stall_ms,
            },
        );
        let (complete_tx, _) = watch::channel(None);
        Arc::new(Self {
            engine: AsyncMutex::new(Engine::build(EngineConfig {
                account_id: mogwai_protocol::AccountId::parse("MOGWAI").expect("fixed account id"),
                instruments: vec![instrument.clone()],
                balances,
                penetration_ticks,
            })),
            instrument,
            tape,
            sim,
            started_ns,
            deadline_ns: run_duration_ns.map(|duration| started_ns.saturating_add(duration)),
            warmup_ns,
            delay_ms: AtomicU64::new(0),
            submit_act_ms: AtomicU64::new(0),
            modify_act_ms: AtomicU64::new(0),
            cancel_act_ms: AtomicU64::new(0),
            submit_ack_ms: AtomicU64::new(0),
            modify_ack_ms: AtomicU64::new(0),
            cancel_ack_ms: AtomicU64::new(0),
            dark_until_ns: AtomicU64::new(0),
            stall_until_ns: AtomicU64::new(0),
            complete_tx,
            lanes: Mutex::new(Vec::new()),
            next_lane_id: AtomicU64::new(0),
        })
    }

    /// Enrol one connection's lanes for venue-originated output. The returned
    /// id is what `release_lanes` retires, so a reconnecting client cannot
    /// retire the lanes of the connection that replaced it.
    pub(crate) fn bind_lanes(&self, lanes: ExecLanes) -> u64 {
        let id = self.next_lane_id.fetch_add(1, Ordering::Relaxed);
        self.locked_lanes().push((id, lanes));
        id
    }

    pub(crate) fn release_lanes(&self, id: u64) {
        self.locked_lanes().retain(|(bound, _)| *bound != id);
    }

    /// The lanes to deliver one venue-originated batch to. Cloned out under the
    /// lock rather than held across the delivery: delivery serializes JSON and
    /// touches per-connection budgets, and doing that while holding a run-wide
    /// mutex would let one connection's cost block every other's teardown.
    pub(crate) fn bound_lanes(&self) -> Vec<(u64, ExecLanes)> {
        self.locked_lanes().clone()
    }

    fn locked_lanes(&self) -> std::sync::MutexGuard<'_, Vec<(u64, ExecLanes)>> {
        self.lanes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Earliest sim instant the tape can serve. Derived, never stored twice:
    /// the floor and the warmup span are the same fact seen from two ends, and
    /// keeping a second copy is how they drift apart.
    pub(crate) fn data_origin_ns(&self) -> u64 {
        self.started_ns.saturating_sub(self.warmup_ns)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(started_ns: u64, warmup_ns: u64, run_duration_ns: Option<u64>) -> Arc<Run> {
        let profiles = Arc::new(source::InstrumentProfiles::defaults());
        let instrument = profiles
            .instrument_defs()
            .into_iter()
            .next()
            .expect("default instrument");
        Run::new(
            instrument,
            profiles,
            std::collections::HashMap::new(),
            0,
            SimClock::identity(),
            started_ns,
            warmup_ns,
            run_duration_ns,
            0.0,
            0,
            8,
            1,
        )
    }

    #[test]
    fn the_history_floor_is_derived_from_the_warmup_span() {
        // Decision 7: `data_origin_ns = run_start_ns - warmup_ns`. Deriving it
        // rather than storing it is what makes the two impossible to disagree.
        let run = run(1_000, 400, None);
        assert_eq!(run.data_origin_ns(), 600);
        assert_eq!(run.started_ns, 1_000);
        assert_eq!(run.warmup_ns, 400);
        run.tape.stop_for_test();
    }

    #[test]
    fn the_deadline_is_measured_from_the_post_warmup_epoch() {
        // Decision 8: the deadline counts from `started_ns`, which is set after
        // warmup generation - NOT from boot. A run whose warmup is larger than
        // its duration must still get its whole declared duration.
        let bounded = run(1_000_000, 999_000, Some(30));
        assert_eq!(bounded.deadline_ns, Some(1_000_030));
        bounded.tape.stop_for_test();

        let indefinite = run(1_000, 0, None);
        assert_eq!(indefinite.deadline_ns, None);
        indefinite.tape.stop_for_test();
    }
}
