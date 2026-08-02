// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The venue re-checking the run's resting limits against the tape.
//!
//! Spawned once at boot, unconditionally, because every resting limit now
//! carries a trigger only a tape walk can advance. A pass with nothing resting
//! is still just one lock acquisition and a `continue`.
//!
//! Owned by the RUN rather than by an account or a session: one process is one
//! ledger now, and a session-owned sweep would freeze a disconnected client's
//! book mid-window, make the `QueryOrders` truth store honestly report a venue
//! that cannot execute, and double the tape walk when two sockets are open on
//! the one run.
//!
//! Without this task the venue accepts resting limits nothing will ever fill: a
//! submit decides only its own order, against the reading it arrived with, so
//! only a sweep pass ever walks the span a trigger is waiting on.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use mogwai_engine::ScanResult;
use mogwai_protocol::{AdmissionSubject, ServerMessage};

use crate::{
    admission::ExecLanes, config::sim_now_ns, fills, run::Run, source::InstrumentProfiles,
};

/// Wall floor under the converted sweep interval. Under an accelerated clock
/// `wall_duration` shrinks linearly while the per-pass fixed cost (checkpoint
/// restore, two lock round-trips) does not, so an unfloored sweep at
/// `speed = 100` is a 1 ms hot loop. The floor costs sim-time resolution the
/// gate does not need and buys a cost that stays bounded in wall time.
pub(crate) const MIN_SWEEP_WALL: Duration = Duration::from_millis(5);

pub(crate) struct FillSweep {
    pub(crate) run: Arc<Run>,
    pub(crate) profiles: Arc<InstrumentProfiles>,
    pub(crate) interval_ms: u64,
}

/// Three phases per pass, and the split is load-bearing: the tape walk costs a
/// checkpoint restore plus a bounded drain against a process-wide mutex, so it
/// runs OFF the engine lock and on `spawn_blocking` or it stalls both order
/// entry and a runtime worker. The engine re-validates every result against its
/// order revision in phase three, which is what makes the off-lock gap safe.
pub(crate) fn spawn_fill_sweeper(sweep: FillSweep) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let sim = sweep.run.sim;
        let mut completion = sweep.run.completion();
        loop {
            let wall = sim
                .wall_duration(crate::config::sim_duration_from_millis(sweep.interval_ms))
                .max(MIN_SWEEP_WALL);
            // A completed run stops walking the tape at once rather than one
            // interval late: the completion sequence is already announcing
            // itself on every socket, and a fill booked after that would be a
            // fill nobody is listening for.
            tokio::select! {
                () = tokio::time::sleep(wall) => {}
                _ = completion.changed() => break,
            }
            let scans = { sweep.run.engine.lock().await.pending_scans() };
            if scans.is_empty() {
                continue;
            }
            // Sampled ONCE for the pass, so every order is judged against the
            // same instant no matter how long the walks take.
            let to_ns = sim_now_ns(sim);
            let mut groups: HashMap<String, Vec<_>> = HashMap::new();
            for scan in scans {
                groups.entry(scan.symbol.clone()).or_default().push(scan);
            }
            let mut results = Vec::new();
            for (symbol, scans) in groups {
                let profiles = Arc::clone(&sweep.profiles);
                let scans_for_walk = scans.clone();
                let walked = tokio::task::spawn_blocking(move || {
                    fills::scan_triggers(&symbol, &scans_for_walk, to_ns, &profiles)
                })
                .await
                .ok()
                .flatten();
                // A `None` walk (the positioning seek could not reach the
                // earliest frontier) yields no result at all for the symbol, so
                // nothing advances: an unreachable span is not a span
                // nothing triggered in.
                if let Some(walk) = walked {
                    results.extend(scans.into_iter().zip(walk.hits).map(|(scan, hit)| {
                        ScanResult {
                            client_order_id: scan.client_order_id,
                            from_ns: scan.from_ns,
                            revision: scan.revision,
                            hit,
                            scanned_to_ns: walk.reached_ns,
                        }
                    }));
                }
            }
            let mut engine = sweep.run.engine.lock().await;
            let (events, emitted) = engine.apply_scans(&results, to_ns);
            let shape = engine.book_shape();
            drop(engine);
            if events.is_empty() {
                continue;
            }
            deliver(&sweep.run, &shape, &events, emitted, to_ns);
        }
    })
}

/// Hand one executed batch to every connection currently open on the run.
///
/// Execution is run-scoped; DELIVERY stays per connection, because `ExecLanes`
/// is per connection. A connection whose reservation is refused gets the
/// ordinary `AdmissionRejected` on its priority lane and learns the real state
/// from `QueryOrders`/`QueryFills`; the EXECUTION is never rolled back. A
/// client's byte budget does not get to decide whether the market traded
/// through a price, and making it decide is what would wedge a book permanently
/// once a batch outgrew the fixed per-connection budget.
fn deliver(
    run: &Arc<Run>,
    shape: &mogwai_protocol::sizing::BookShape,
    events: &[ServerMessage],
    emitted: usize,
    ts: u64,
) {
    let subject = events.iter().find_map(|event| match event {
        ServerMessage::OrderFilled(fill) => Some(AdmissionSubject::Submit {
            client_order_id: fill.client_order_id.clone(),
        }),
        ServerMessage::OrderTriggered {
            client_order_id, ..
        }
        | ServerMessage::OrderCanceled {
            client_order_id, ..
        }
        | ServerMessage::OrderRejected {
            client_order_id, ..
        } => Some(AdmissionSubject::Submit {
            client_order_id: client_order_id.clone(),
        }),
        _ => None,
    });
    let mut closed = Vec::new();
    for (id, lane) in run.bound_lanes() {
        let Some(reservation) = lane.reserve_swept(shape, emitted) else {
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
    // A lane whose receiver is gone is a connection that is already tearing
    // down; retiring it here means a wedged socket cannot make every later pass
    // pay for it.
    for id in closed {
        run.release_lanes(id);
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
    // No priority slot either: the connection is already saturated, and the
    // fill is in the truth store regardless. Silence here costs nothing a
    // reconciliation query does not recover.
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
