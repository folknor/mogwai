// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The venue re-checking one ACCOUNT's resting limits against the tape.
//!
//! Spawned by `AccountRegistry::acquire`, and ONLY when `penetration_ticks > 0`,
//! so a default venue pays nothing at all - not a task, not a timer, not a lock
//! acquisition. Lives as long as the slot: account-owned, not session-owned,
//! because `POST /orders` is a first-class order carrier. A session-owned
//! sweeper would leave an HTTP-only account's orders permanently unfillable,
//! freeze a disconnected account's book mid-window, make the `QueryOrders`
//! truth store honestly report a venue that cannot execute, and double the tape
//! walk when two sockets share one account.

use std::{
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use mogwai_engine::ScanResult;
use mogwai_protocol::{AdmissionSubject, ServerMessage, SimClock};

use crate::{
    accounts::AccountSlot, admission::ExecLanes, config::sim_now_ns, fills,
    source::InstrumentProfiles,
};

/// Wall floor under the converted sweep interval. Under an accelerated clock
/// `wall_duration` shrinks linearly while the per-pass fixed cost (checkpoint
/// restore, two lock round-trips) does not, so an unfloored sweep at
/// `speed = 100` is a 1 ms hot loop. The floor costs sim-time resolution the
/// gate does not need and buys a cost that stays bounded in wall time.
pub(crate) const MIN_SWEEP_WALL: Duration = Duration::from_millis(5);

pub(crate) struct FillSweep {
    pub(crate) slot: Arc<AccountSlot>,
    pub(crate) sim: SimClock,
    pub(crate) profiles: Arc<InstrumentProfiles>,
    pub(crate) data_origin_ns: u64,
    pub(crate) interval_ms: u64,
}

/// Three phases per pass, and the split is load-bearing: the tape walk costs a
/// checkpoint restore plus a bounded drain against a process-wide mutex, so it
/// runs OFF the engine lock and on `spawn_blocking` or it stalls both this
/// account's order commands and a runtime worker. The engine re-validates every
/// result against its order revision in phase three, which is what makes the
/// off-lock gap safe.
pub(crate) fn spawn_account_sweeper(sweep: FillSweep) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let wall = sweep
                .sim
                .wall_duration(crate::config::sim_duration_from_millis(sweep.interval_ms))
                .max(MIN_SWEEP_WALL);
            // Teardown wins the race rather than being noticed one interval
            // late, so a destroyed or reaped account stops walking the tape at
            // once and drops its `Arc<AccountSlot>` with it.
            tokio::select! {
                () = tokio::time::sleep(wall) => {}
                () = sweep.slot.wait_for_teardown() => break,
            }
            if sweep.slot.tombstoned.load(Ordering::Relaxed) {
                break;
            }
            let scans = { sweep.slot.engine.lock().await.pending_scans() };
            if scans.is_empty() {
                continue;
            }
            // Sampled ONCE for the pass, so every order is judged against the
            // same instant no matter how long the walks take.
            let to_ns = sim_now_ns(sweep.sim);
            let mut groups: HashMap<String, Vec<_>> = HashMap::new();
            for scan in scans {
                groups.entry(scan.symbol.clone()).or_default().push(scan);
            }
            let mut results = Vec::new();
            for (symbol, scans) in groups {
                let profiles = Arc::clone(&sweep.profiles);
                let origin = sweep.data_origin_ns;
                let scans_for_walk = scans.clone();
                let walked = tokio::task::spawn_blocking(move || {
                    fills::count_penetrations(&symbol, &scans_for_walk, to_ns, &profiles, origin)
                })
                .await
                .ok()
                .flatten();
                // A `None` walk (the positioning seek could not reach the
                // earliest frontier) yields no result at all for the symbol, so
                // nothing advances: an unreachable span is not zero
                // penetrations.
                if let Some(walk) = walked {
                    results.extend(scans.into_iter().zip(walk.counted).map(|(scan, counted)| {
                        ScanResult {
                            client_order_id: scan.client_order_id,
                            from_ns: scan.from_ns,
                            revision: scan.revision,
                            counted,
                            scanned_to_ns: walk.reached_ns,
                        }
                    }));
                }
            }
            let mut engine = sweep.slot.engine.lock().await;
            if sweep.slot.tombstoned.load(Ordering::Relaxed) {
                break;
            }
            let (events, emitted) = engine.apply_scans(&results, to_ns);
            let shape = engine.book_shape();
            drop(engine);
            if events.is_empty() {
                continue;
            }
            deliver(&sweep.slot, &shape, &events, emitted, to_ns);
        }
    })
}

/// Hand one executed batch to every session currently bound to the account.
///
/// Execution is account-scoped; DELIVERY stays per session, because `ExecLanes`
/// is per session. A session whose reservation is refused gets the ordinary
/// `AdmissionRejected` on its priority lane and learns the real state from
/// `QueryOrders`/`QueryFills`; the EXECUTION is never rolled back. A client's
/// byte budget does not get to decide whether the market traded through a price,
/// and making it decide is what would wedge a book permanently once a batch
/// outgrew the fixed per-session budget.
fn deliver(
    slot: &Arc<AccountSlot>,
    shape: &mogwai_protocol::sizing::BookShape,
    events: &[ServerMessage],
    emitted: usize,
    ts: u64,
) {
    let subject = events.iter().find_map(|event| match event {
        ServerMessage::OrderFilled(fill) => Some(AdmissionSubject::Submit {
            client_order_id: fill.client_order_id.clone(),
        }),
        _ => None,
    });
    let lanes: Vec<(u64, ExecLanes)> = slot
        .session_lanes
        .lock()
        .expect("session lanes poisoned")
        .clone();
    let mut closed = Vec::new();
    for (id, lane) in lanes {
        let Some(reservation) = lane.reserve_penetrated(shape, emitted) else {
            if refuse(&lane, subject.clone(), ts).is_err() {
                closed.push(id);
            }
            continue;
        };
        if lane
            .submit_produced(reservation, Instant::now(), None, events.to_vec())
            .is_err()
        {
            closed.push(id);
        }
    }
    if !closed.is_empty() {
        slot.session_lanes
            .lock()
            .expect("session lanes poisoned")
            .retain(|(id, _)| !closed.contains(id));
    }
}

fn refuse(
    lane: &ExecLanes,
    subject: Option<AdmissionSubject>,
    ts: u64,
) -> Result<(), crate::admission::LaneClosed> {
    let Some(subject) = subject else {
        return Ok(());
    };
    // No priority slot either: the session is already saturated, and the fill is
    // in the truth store regardless. Silence here costs nothing a reconciliation
    // query does not recover.
    let Some(slot) = lane.reserve_admission() else {
        return Ok(());
    };
    lane.emit_admission(
        slot,
        ServerMessage::AdmissionRejected {
            subject,
            reason: "execution output admission budget exhausted".into(),
            ts_event: ts,
        },
    )
}
