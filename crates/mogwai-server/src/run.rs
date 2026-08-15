// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! State owned by one venue process: one ledger and one paced boat over many rivers.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use mogwai_engine::{Engine, EngineConfig};
use mogwai_protocol::{CommandClass, InstrumentDef, RunSeeds, SimClock, Symbol};
use rust_decimal::Decimal;
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::{
    admission::ExecLanes,
    source,
    tape::{Tape, TapeSpawn},
};

pub(crate) struct Run {
    /// The shape this run placed its one paced boat on. Every configured shape
    /// has a river in `rivers` and is servable for history; only this one has a
    /// live tape, which is why a `/ws` upgrade may still bind nothing else.
    pub(crate) boot_symbol: Symbol,
    /// Every configured river, created on first use and keyed independently, so
    /// two symbols never serialize on each other's checkpoint chain.
    pub(crate) rivers: Arc<source::Rivers>,
    pub(crate) oms_type: mogwai_protocol::OmsType,
    pub(crate) engine: AsyncMutex<Engine>,
    pub(crate) seeds: RunSeeds,
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
    /// trigger fill nobody commanded - reaches all of them. This is what
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
        rivers: Arc<source::Rivers>,
        balances: std::collections::HashMap<String, Decimal>,
        sim: SimClock,
        started_ns: u64,
        warmup_ns: u64,
        run_duration_ns: Option<u64>,
        seeds: RunSeeds,
        speed: f64,
        fanout_depth: usize,
        zero_speed_stall_ms: u64,
        oms_type: mogwai_protocol::OmsType,
        fill_band_max_ticks: u32,
        account_id: mogwai_protocol::AccountId,
        fault_tx: std::sync::mpsc::Sender<mogwai_data::TickFault>,
    ) -> Arc<Self> {
        let symbol = std::sync::Arc::clone(&instrument.symbol);
        let tape = Tape::start(
            symbol.to_string(),
            TapeSpawn {
                rivers: Arc::clone(&rivers),
                sim,
                speed,
                fanout_depth,
                zero_speed_stall_ms,
                fault_tx,
            },
        );
        let (complete_tx, _) = watch::channel(None);
        let mut engine = Engine::build(EngineConfig {
            account_id,
            instruments: Vec::new(),
            balances,
            fill_seed: seeds.fill,
        });
        engine.set_oms_type(oms_type);
        engine.set_liquidation_band_ticks(fill_band_max_ticks);
        // The engine starts EMPTY. An instrument becomes tradable when a socket
        // binds its symbol or an order names it, through `ensure_instrument`.
        Arc::new(Self {
            engine: AsyncMutex::new(engine),
            boot_symbol: instrument.symbol,
            rivers,
            oms_type,
            seeds,
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

    /// Make `symbol` tradable on this run's engine: register the def and install
    /// the margin policy and fee schedule from its profile. Called when a socket
    /// binds a symbol and before an order for it is admitted. `false` means no
    /// profile resolves it, and the caller lets the engine produce its own
    /// unknown-instrument rejection rather than inventing a second wording.
    ///
    /// This is the ONE path from a profile to engine policy - `Run::new` no
    /// longer has a copy - and the installs are guarded on the registration
    /// having been NEW, so re-binding a symbol a client is already trading never
    /// resets its configuration.
    pub(crate) async fn ensure_instrument(&self, symbol: &str) -> bool {
        let Some(profile) = self.rivers.profiles().get(symbol).cloned() else {
            return false;
        };
        let mut engine = self.engine.lock().await;
        if !engine.ensure_instrument(profile.def.clone()) {
            return true;
        }
        if let Some(margin) = profile.margin {
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
                },
            );
        }
        if let Some(fees) = profile.fees {
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
        true
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
            0.0,
            8,
            1,
            mogwai_protocol::OmsType::Netting,
            200,
            mogwai_protocol::AccountId::parse(crate::config::DEFAULT_ACCOUNT_ID)
                .expect("the default account id is legal"),
            std::sync::mpsc::channel().0,
        )
    }

    #[test]
    fn the_history_floor_is_the_fixed_tape_origin() {
        let run = run(1_000, 400, None);
        assert_eq!(run.data_origin_ns(), source::TAPE_ORIGIN_NS);
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
